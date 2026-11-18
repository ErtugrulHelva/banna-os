//! NVMe (PCIe SSD): yönetici kuyruğu, tek G/Ç kuyruk çifti, Identify, PRP listeli
//! okuma/yazma, FLUSH ve kapanışta düzgün durdurma (CC.SHN).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use super::{BlockDevice, Result, register_disk};
use crate::arch::tsc;
use crate::drivers::pci;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sched::Mutex;
use crate::sync::SpinLock;

const REG_CAP: usize = 0x00;
const REG_VS: usize = 0x08;
const REG_INTMS: usize = 0x0c;
const REG_CC: usize = 0x14;
const REG_CSTS: usize = 0x1c;
const REG_AQA: usize = 0x24;
const REG_ASQ: usize = 0x28;
const REG_ACQ: usize = 0x30;

const CC_EN: u32 = 1 << 0;
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

const ADMIN_ENTRIES: u16 = 32;
const IO_ENTRIES: u16 = 64;
const BOUNCE_PAGES: usize = 32;
const COMMAND_TIMEOUT_NS: u64 = 10_000_000_000;

const OP_FLUSH: u8 = 0x00;
const OP_WRITE: u8 = 0x01;
const OP_READ: u8 = 0x02;
const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const ADMIN_SET_FEATURES: u8 = 0x09;
const FEATURE_HOST_MEMORY: u32 = 0x0d;

struct Queue {
    sq_phys: u64,
    cq_phys: u64,
    entries: u16,
    sq_tail: u16,
    cq_head: u16,
    phase: u16,
    sq_doorbell: *mut u32,
    cq_doorbell: *mut u32,
    next_cid: u16,
}

unsafe impl Send for Queue {}

impl Queue {
    fn new(regs: *mut u8, qid: u16, entries: u16, stride: usize) -> Option<Self> {
        let sq_phys = pmm::alloc_contiguous(((entries as u64 * 64).div_ceil(PAGE_SIZE)).max(1))?;
        let cq_phys = pmm::alloc_dma_frame()?;
        unsafe {
            core::ptr::write_bytes(phys_to_virt(sq_phys), 0, entries as usize * 64);
            core::ptr::write_bytes(phys_to_virt(cq_phys), 0, PAGE_SIZE as usize);
        }
        let doorbell = |index: usize| unsafe { regs.add(0x1000 + index * stride) as *mut u32 };
        Some(Self {
            sq_phys,
            cq_phys,
            entries,
            sq_tail: 0,
            cq_head: 0,
            phase: 1,
            sq_doorbell: doorbell(2 * qid as usize),
            cq_doorbell: doorbell(2 * qid as usize + 1),
            next_cid: 0,
        })
    }

    /// Komutu gönderir ve tamamlanmasını yoklayarak bekler; NVMe durum kodu (0 = başarı).
    fn submit(&mut self, mut command: [u32; 16]) -> core::result::Result<u32, u16> {
        self.next_cid = self.next_cid.wrapping_add(1);
        let cid = self.next_cid;
        command[0] = (command[0] & 0xffff) | (cid as u32) << 16;
        unsafe {
            let slot = phys_to_virt(self.sq_phys).add(self.sq_tail as usize * 64) as *mut u32;
            for (i, dw) in command.iter().enumerate() {
                write_volatile(slot.add(i), *dw);
            }
        }
        self.sq_tail = (self.sq_tail + 1) % self.entries;
        fence(Ordering::SeqCst);
        unsafe { write_volatile(self.sq_doorbell, self.sq_tail as u32) };
        let deadline = tsc::now_ns() + COMMAND_TIMEOUT_NS;
        loop {
            let entry =
                unsafe { phys_to_virt(self.cq_phys).add(self.cq_head as usize * 16) as *const u32 };
            let dw3 = unsafe { read_volatile(entry.add(3)) };
            if (dw3 >> 16) as u16 & 1 == self.phase {
                fence(Ordering::SeqCst);
                let result = unsafe { read_volatile(entry) };
                self.cq_head += 1;
                if self.cq_head == self.entries {
                    self.cq_head = 0;
                    self.phase ^= 1;
                }
                unsafe { write_volatile(self.cq_doorbell, self.cq_head as u32) };
                if dw3 as u16 != cid {
                    continue;
                }
                let status = (dw3 >> 17) as u16 & 0x7fff;
                return if status == 0 { Ok(result) } else { Err(status) };
            }
            if tsc::now_ns() > deadline {
                return Err(0xffff);
            }
            core::hint::spin_loop();
        }
    }
}

struct Io {
    queue: Queue,
    bounce: u64,
    prp_list: u64,
}

pub struct NvmeDisk {
    controller: usize,
    nsid: u32,
    sectors: u64,
    block_size: usize,
    max_blocks: usize,
    model: String,
    io: Arc<Mutex<Io>>,
}

unsafe impl Send for NvmeDisk {}
unsafe impl Sync for NvmeDisk {}

impl NvmeDisk {
    fn transfer(&self, io: &mut Io, opcode: u8, lba: u64, blocks: usize) -> Result<()> {
        let bytes = blocks * self.block_size;
        let pages = bytes.div_ceil(PAGE_SIZE as usize);
        let prp2 = match pages {
            1 => 0,
            2 => io.bounce + PAGE_SIZE,
            _ => io.prp_list,
        };
        let mut cmd = [0u32; 16];
        cmd[0] = opcode as u32;
        cmd[1] = self.nsid;
        cmd[6] = io.bounce as u32;
        cmd[7] = (io.bounce >> 32) as u32;
        cmd[8] = prp2 as u32;
        cmd[9] = (prp2 >> 32) as u32;
        cmd[10] = lba as u32;
        cmd[11] = (lba >> 32) as u32;
        cmd[12] = (blocks - 1) as u32;
        io.queue.submit(cmd).map(|_| ()).map_err(|status| {
            crate::log!(
                "nvme: {} lba {} failed, status {:#x}",
                if opcode == OP_READ { "read" } else { "write" },
                lba,
                status
            );
            "NVMe I/O error"
        })
    }

    fn flush(&self) {
        let mut cmd = [0u32; 16];
        cmd[0] = OP_FLUSH as u32;
        cmd[1] = self.nsid;
        let _ = self.io.lock().queue.submit(cmd);
    }
}

impl BlockDevice for NvmeDisk {
    fn name(&self) -> String {
        format!("nvme{}n{}", self.controller, self.nsid)
    }

    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let bs = self.block_size;
        let mut done = 0;
        for chunk in buf.chunks_mut(self.max_blocks * bs) {
            let blocks = chunk.len() / bs;
            let mut io = self.io.lock();
            self.transfer(&mut io, OP_READ, lba + done, blocks)?;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    phys_to_virt(io.bounce),
                    chunk.as_mut_ptr(),
                    chunk.len(),
                )
            };
            done += blocks as u64;
        }
        Ok(())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let bs = self.block_size;
        let mut done = 0;
        for chunk in buf.chunks(self.max_blocks * bs) {
            let blocks = chunk.len() / bs;
            let mut io = self.io.lock();
            unsafe {
                core::ptr::copy_nonoverlapping(chunk.as_ptr(), phys_to_virt(io.bounce), chunk.len())
            };
            self.transfer(&mut io, OP_WRITE, lba + done, blocks)?;
            done += blocks as u64;
        }
        Ok(())
    }
}

struct Controller {
    regs: *mut u8,
    disks: Vec<Arc<NvmeDisk>>,
}

unsafe impl Send for Controller {}

static CONTROLLERS: SpinLock<Vec<Controller>> = SpinLock::new(Vec::new());

fn r32(regs: *mut u8, offset: usize) -> u32 {
    unsafe { read_volatile(regs.add(offset) as *const u32) }
}

fn w32(regs: *mut u8, offset: usize, value: u32) {
    unsafe { write_volatile(regs.add(offset) as *mut u32, value) }
}

fn w64(regs: *mut u8, offset: usize, value: u64) {
    w32(regs, offset, value as u32);
    w32(regs, offset + 4, (value >> 32) as u32);
}

fn wait_ready(regs: *mut u8, ready: bool, timeout_ms: u64) -> bool {
    let deadline = tsc::now_ns() + timeout_ms * 1_000_000;
    while (r32(regs, REG_CSTS) & CSTS_RDY != 0) != ready {
        if r32(regs, REG_CSTS) & CSTS_CFS != 0 || tsc::now_ns() > deadline {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

fn identify(admin: &mut Queue, buffer: u64, cns: u32, nsid: u32) -> bool {
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_IDENTIFY as u32;
    cmd[1] = nsid;
    cmd[6] = buffer as u32;
    cmd[7] = (buffer >> 32) as u32;
    cmd[10] = cns;
    admin.submit(cmd).is_ok()
}

/// DRAM'siz SSD'ler (ör. Patriot P300) eşleme tablolarını sistem belleğinde tutmak ister:
/// Host Memory Buffer. Linux gibi en çok 128 MiB, 2 MiB'lık bitişik parçalar hâlinde
/// verilir; ayrılamazsa ya da `HMMIN` karşılanamazsa SSD HMB'siz (daha yavaş) çalışır.
fn enable_host_memory(admin: &mut Queue, preferred: u32, minimum: u32) -> Option<u64> {
    const CHUNK_PAGES: u64 = 512;
    const MAX_PAGES: u64 = 32 * 1024;
    let wanted = (preferred as u64).min(MAX_PAGES).max(minimum as u64);
    if wanted == 0 || wanted > MAX_PAGES {
        return None;
    }
    let list = pmm::alloc_dma_frame()?;
    let mut total = 0u64;
    let mut count = 0usize;
    while total < wanted && count < 256 {
        let pages = CHUNK_PAGES.min(wanted - total);
        let Some(chunk) = pmm::alloc_contiguous(pages) else {
            break;
        };
        unsafe {
            core::ptr::write_bytes(phys_to_virt(chunk), 0, (pages * PAGE_SIZE) as usize);
            let entry = phys_to_virt(list).add(count * 16);
            write_volatile(entry as *mut u64, chunk);
            write_volatile(entry.add(8) as *mut u32, pages as u32);
            write_volatile(entry.add(12) as *mut u32, 0);
        }
        total += pages;
        count += 1;
    }
    if total == 0 || total < minimum as u64 {
        crate::log!(
            "nvme: host memory buffer: only {} of {} pages available",
            total,
            wanted
        );
        return None;
    }
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_SET_FEATURES as u32;
    cmd[10] = FEATURE_HOST_MEMORY;
    cmd[11] = 1;
    cmd[12] = total as u32;
    cmd[13] = list as u32;
    cmd[14] = (list >> 32) as u32;
    cmd[15] = count as u32;
    match admin.submit(cmd) {
        Ok(_) => Some(total * PAGE_SIZE),
        Err(status) => {
            crate::log!("nvme: host memory buffer rejected (status {:#x})", status);
            None
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from(core::str::from_utf8(bytes).unwrap_or("").trim())
}

fn init_controller(
    index: usize,
    dev: &pci::Device,
) -> core::result::Result<Vec<Arc<NvmeDisk>>, &'static str> {
    dev.enable();
    let base = dev.bar(0).ok_or("no BAR0")?;
    let regs = map_physical(base, 0x1000, NO_CACHE).map_err(|_| "cannot map registers")?;
    let cap = r32(regs, REG_CAP) as u64 | (r32(regs, REG_CAP + 4) as u64) << 32;
    // MQES 0 tabanlıdır; gerçek SSD'ler çoğunlukla 0xffff (65536 girdi) bildirir.
    let max_entries = ((cap & 0xffff) + 1).min(u16::MAX as u64) as u16;
    let timeout_ms = ((cap >> 24) & 0xff).max(1) * 500;
    let stride = 4usize << ((cap >> 32) & 0xf);
    // Yönetici + bir G/Ç kuyruk çiftinin kapı zilleri (DSTRD büyükse 0x4000'ü aşar).
    let regs = map_physical(base, (0x1000 + 4 * stride) as u64, NO_CACHE)
        .map_err(|_| "cannot map registers")?;
    if (cap >> 37) & 1 == 0 {
        return Err("NVM command set not supported");
    }
    if (cap >> 48) & 0xf != 0 {
        return Err("4 KiB pages not supported");
    }

    w32(regs, REG_CC, r32(regs, REG_CC) & !CC_EN);
    if !wait_ready(regs, false, timeout_ms) {
        return Err("controller did not disable");
    }
    let admin_entries = ADMIN_ENTRIES.min(max_entries);
    let mut admin = Queue::new(regs, 0, admin_entries, stride).ok_or("out of memory")?;
    w32(
        regs,
        REG_AQA,
        ((admin_entries as u32 - 1) << 16) | (admin_entries as u32 - 1),
    );
    w64(regs, REG_ASQ, admin.sq_phys);
    w64(regs, REG_ACQ, admin.cq_phys);
    w32(regs, REG_INTMS, u32::MAX);
    // NVM komut kümesi, 4 KiB sayfa, 64 baytlık SQ ve 16 baytlık CQ girdileri.
    w32(regs, REG_CC, CC_EN | (6 << 16) | (4 << 20));
    if !wait_ready(regs, true, timeout_ms) {
        return Err("controller did not become ready");
    }

    let buffer = pmm::alloc_dma_frame().ok_or("out of memory")?;
    if !identify(&mut admin, buffer, 1, 0) {
        return Err("identify controller failed");
    }
    let id = unsafe { core::slice::from_raw_parts(phys_to_virt(buffer), 4096) };
    let model = text(&id[24..64]);
    let serial = text(&id[4..24]);
    let mdts = id[77];
    let hmb_preferred = u32::from_le_bytes(id[272..276].try_into().unwrap());
    let hmb_minimum = u32::from_le_bytes(id[276..280].try_into().unwrap());
    let namespaces = u32::from_le_bytes(id[516..520].try_into().unwrap()).min(16);
    let version = r32(regs, REG_VS);
    // Gerçek donanımda doğrulanana kadar isteğe bağlı (`nvme_hmb` önyükleme bayrağı).
    let host_memory = if hmb_preferred > 0 && crate::boot::has_flag("nvme_hmb") {
        enable_host_memory(&mut admin, hmb_preferred, hmb_minimum)
    } else {
        None
    };

    let io_entries = IO_ENTRIES.min(max_entries);
    let queue = Queue::new(regs, 1, io_entries, stride).ok_or("out of memory")?;
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_CREATE_CQ as u32;
    cmd[6] = queue.cq_phys as u32;
    cmd[7] = (queue.cq_phys >> 32) as u32;
    cmd[10] = ((io_entries as u32 - 1) << 16) | 1;
    cmd[11] = 1;
    admin
        .submit(cmd)
        .map_err(|_| "create I/O completion queue failed")?;
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_CREATE_SQ as u32;
    cmd[6] = queue.sq_phys as u32;
    cmd[7] = (queue.sq_phys >> 32) as u32;
    cmd[10] = ((io_entries as u32 - 1) << 16) | 1;
    cmd[11] = (1 << 16) | 1;
    admin
        .submit(cmd)
        .map_err(|_| "create I/O submission queue failed")?;

    let bounce = pmm::alloc_contiguous(BOUNCE_PAGES as u64).ok_or("out of memory")?;
    let prp_list = pmm::alloc_dma_frame().ok_or("out of memory")?;
    unsafe {
        let list = phys_to_virt(prp_list) as *mut u64;
        for i in 1..BOUNCE_PAGES {
            write_volatile(list.add(i - 1), bounce + i as u64 * PAGE_SIZE);
        }
    }
    let max_bytes = if mdts == 0 || mdts >= 16 {
        BOUNCE_PAGES * PAGE_SIZE as usize
    } else {
        ((1usize << mdts) * PAGE_SIZE as usize).min(BOUNCE_PAGES * PAGE_SIZE as usize)
    };
    let io = Arc::new(Mutex::new(Io {
        queue,
        bounce,
        prp_list,
    }));
    crate::log!(
        "nvme: {} '{}' serial '{}' NVMe {}.{}, {} namespace(s), max transfer {} KiB",
        index,
        model,
        serial,
        version >> 16,
        (version >> 8) & 0xff,
        namespaces,
        max_bytes / 1024
    );
    if let Some(bytes) = host_memory {
        crate::log!("nvme: {} host memory buffer {} MiB", index, bytes >> 20);
    }

    let mut disks = Vec::new();
    for nsid in 1..=namespaces {
        unsafe { core::ptr::write_bytes(phys_to_virt(buffer), 0, 4096) };
        if !identify(&mut admin, buffer, 0, nsid) {
            continue;
        }
        let ns = unsafe { core::slice::from_raw_parts(phys_to_virt(buffer), 4096) };
        let size = u64::from_le_bytes(ns[0..8].try_into().unwrap());
        if size == 0 {
            continue;
        }
        let format = (ns[26] & 0xf) as usize;
        let lbaf = u32::from_le_bytes(ns[128 + format * 4..132 + format * 4].try_into().unwrap());
        let lbads = (lbaf >> 16) & 0xff;
        let block_size = if lbads < 32 { 1usize << lbads } else { 0 };
        if !(512..=4096).contains(&block_size) || lbaf & 0xffff != 0 {
            crate::log!(
                "nvme: namespace {} format {}+{} bytes (unsupported)",
                nsid,
                block_size,
                lbaf & 0xffff
            );
            continue;
        }
        disks.push(Arc::new(NvmeDisk {
            controller: index,
            nsid,
            sectors: size,
            block_size,
            max_blocks: max_bytes / block_size,
            model: model.clone(),
            io: io.clone(),
        }));
    }
    pmm::free_frame(buffer);
    CONTROLLERS.lock().push(Controller {
        regs,
        disks: disks.clone(),
    });
    Ok(disks)
}

pub fn probe() {
    let devices = pci::find_all(0x01, 0x08);
    for (index, dev) in devices.iter().filter(|d| d.prog_if == 0x02).enumerate() {
        match init_controller(index, dev) {
            Ok(disks) => {
                for disk in disks {
                    register_disk(disk);
                }
            }
            Err(e) => crate::log!(
                "nvme: {:02x}:{:02x}.{}: {}",
                dev.bus,
                dev.slot,
                dev.function,
                e
            ),
        }
    }
}

/// Kapanmadan önce önbelleği boşaltır ve denetleyiciyi düzgün durdurur (CC.SHN = normal).
pub fn shutdown() {
    let list = CONTROLLERS.lock();
    for c in list.iter() {
        for disk in &c.disks {
            disk.flush();
        }
        let cc = r32(c.regs, REG_CC) & !(3 << 14);
        w32(c.regs, REG_CC, cc | (1 << 14));
        let deadline = tsc::now_ns() + 2_000_000_000;
        while (r32(c.regs, REG_CSTS) >> 2) & 3 != 2 && tsc::now_ns() < deadline {
            core::hint::spin_loop();
        }
    }
}
