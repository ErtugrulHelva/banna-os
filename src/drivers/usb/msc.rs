//! USB yığın bellek (Bulk-Only Transport + SCSI): flash bellekler blok aygıtı olarak
//! depolama katmanına eklenir; sonradan takılanlar `/usbN` altına bağlanır.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use super::host::{self, CC_STALL, EndpointConfig};
use super::{Dma, InterfaceInfo, UsbDevice};
use crate::sched::Mutex;
use crate::storage::{self, BlockDevice};
use crate::sync::SpinLock;

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const MAX_TRANSFER: usize = 64 * 1024;
const TIMEOUT_MS: u64 = 10_000;

static NEXT_INDEX: AtomicUsize = AtomicUsize::new(0);

struct Io {
    _dma: Dma,
    buffer: u64,
    tag: u32,
}

pub struct UsbDisk {
    controller: usize,
    slot: u8,
    interface: u8,
    bulk_in: u8,
    bulk_out: u8,
    index: usize,
    sectors: u64,
    block_size: u32,
    model: String,
    io: Mutex<Io>,
    gone: AtomicBool,
    errors: AtomicU32,
}

unsafe impl Send for UsbDisk {}
unsafe impl Sync for UsbDisk {}

enum Direction<'a> {
    None,
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

impl UsbDisk {
    fn virt(&self, io: &Io) -> *mut u8 {
        crate::memory::phys_to_virt(io.buffer)
    }

    fn clear_halt(&self, endpoint: u8) {
        host::reset_endpoint(self.controller, self.slot, endpoint);
        let _ = host::control(
            self.controller,
            self.slot,
            host::Setup {
                request_type: 0x02,
                request: 1,
                value: 0,
                index: endpoint as u16,
                length: 0,
            },
            &mut [],
        );
    }

    /// BOT sıfırlama kurtarması (USB MSC BOT §5.3.4).
    fn reset_recovery(&self) {
        let _ = host::control(
            self.controller,
            self.slot,
            host::Setup {
                request_type: 0x21,
                request: 0xff,
                value: 0,
                index: self.interface as u16,
                length: 0,
            },
            &mut [],
        );
        self.clear_halt(self.bulk_in);
        self.clear_halt(self.bulk_out);
    }

    fn bulk(&self, endpoint: u8, phys: u64, len: usize) -> Result<usize, u8> {
        let r = host::bulk(self.controller, self.slot, endpoint, phys, len, TIMEOUT_MS);
        if r == Err(CC_STALL) {
            self.clear_halt(endpoint);
        }
        r
    }

    /// Bir SCSI komutu çalıştırır: CBW → veri → CSW. CSW durumunu döndürür.
    fn command(&self, cdb: &[u8], data: Direction) -> Result<u8, &'static str> {
        if self.gone.load(Ordering::Relaxed) {
            return Err("USB transfer failed");
        }
        let mut io = self.io.lock();
        io.tag = io.tag.wrapping_add(1);
        let tag = io.tag;
        let (len, flags) = match &data {
            Direction::None => (0, 0),
            Direction::In(b) => (b.len(), 0x80),
            Direction::Out(b) => (b.len(), 0x00),
        };
        let base = self.virt(&io);
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        cbw[12] = flags;
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);
        // Tamponun sonundaki 512 bayt CBW/CSW içindir; veri başta.
        let control_off = MAX_TRANSFER as u64;
        unsafe { core::ptr::copy_nonoverlapping(cbw.as_ptr(), base.add(control_off as usize), 31) };
        if self
            .bulk(self.bulk_out, io.buffer + control_off, 31)
            .is_err()
        {
            self.reset_recovery();
            return Err("USB transfer failed");
        }
        match data {
            Direction::None => {}
            Direction::In(buf) => match self.bulk(self.bulk_in, io.buffer, buf.len()) {
                Ok(n) => unsafe { core::ptr::copy_nonoverlapping(base, buf.as_mut_ptr(), n) },
                Err(CC_STALL) => {}
                Err(_) => {
                    self.reset_recovery();
                    return Err("USB transfer failed");
                }
            },
            Direction::Out(buf) => {
                unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), base, buf.len()) };
                match self.bulk(self.bulk_out, io.buffer, buf.len()) {
                    Ok(_) | Err(CC_STALL) => {}
                    Err(_) => {
                        self.reset_recovery();
                        return Err("USB transfer failed");
                    }
                }
            }
        }
        let mut csw_result = self.bulk(self.bulk_in, io.buffer + control_off, 13);
        if csw_result == Err(CC_STALL) {
            csw_result = self.bulk(self.bulk_in, io.buffer + control_off, 13);
        }
        let mut csw = [0u8; 13];
        unsafe {
            core::ptr::copy_nonoverlapping(base.add(control_off as usize), csw.as_mut_ptr(), 13)
        };
        let valid = csw_result == Ok(13)
            && u32::from_le_bytes(csw[0..4].try_into().unwrap()) == CSW_SIGNATURE
            && u32::from_le_bytes(csw[4..8].try_into().unwrap()) == tag;
        if !valid || csw[12] == 2 {
            self.reset_recovery();
            return Err("USB transfer failed");
        }
        Ok(csw[12])
    }

    /// 2 TiB'ı aşan LBA'lar için READ/WRITE(16), aksi hâlde (10).
    fn cdb_rw(write: bool, lba: u64, sectors: u32) -> Vec<u8> {
        if lba + sectors as u64 > u32::MAX as u64 {
            let mut cdb = alloc::vec![0u8; 16];
            cdb[0] = if write { 0x8a } else { 0x88 };
            cdb[2..10].copy_from_slice(&lba.to_be_bytes());
            cdb[10..14].copy_from_slice(&sectors.to_be_bytes());
            return cdb;
        }
        let mut cdb = alloc::vec![0u8; 10];
        cdb[0] = if write { 0x2a } else { 0x28 };
        cdb[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
        cdb[7..9].copy_from_slice(&(sectors as u16).to_be_bytes());
        cdb
    }

    /// Başarısız komutu REQUEST SENSE ile hatayı temizleyip iki kez daha dener.
    fn retry(&self, mut run: impl FnMut() -> Result<u8, &'static str>) -> storage::Result<()> {
        for attempt in 0..3 {
            match run() {
                Ok(0) => return Ok(()),
                _ if attempt < 2 => {
                    let _ = self.request_sense();
                }
                _ => {}
            }
        }
        self.errors.fetch_add(1, Ordering::Relaxed);
        Err("USB disk I/O error")
    }

    fn request_sense(&self) -> Result<[u8; 18], &'static str> {
        let mut sense = [0u8; 18];
        self.command(&[0x03, 0, 0, 0, 18, 0], Direction::In(&mut sense))?;
        Ok(sense)
    }
}

impl BlockDevice for UsbDisk {
    fn name(&self) -> String {
        format!("usb{}", self.index)
    }

    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn removable(&self) -> bool {
        true
    }

    fn block_size(&self) -> usize {
        self.block_size as usize
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> storage::Result<()> {
        let bs = self.block_size as usize;
        let per = MAX_TRANSFER / bs;
        let mut done = 0;
        for chunk in buf.chunks_mut(per * bs) {
            let count = (chunk.len() / bs) as u32;
            let cdb = Self::cdb_rw(false, lba + done, count);
            self.retry(|| self.command(cdb.as_slice(), Direction::In(&mut *chunk)))?;
            done += count as u64;
        }
        Ok(())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> storage::Result<()> {
        let bs = self.block_size as usize;
        let per = MAX_TRANSFER / bs;
        let mut done = 0;
        for chunk in buf.chunks(per * bs) {
            let count = (chunk.len() / bs) as u32;
            let cdb = Self::cdb_rw(true, lba + done, count);
            self.retry(|| self.command(cdb.as_slice(), Direction::Out(chunk)))?;
            done += count as u64;
        }
        Ok(())
    }
}

static DISKS: SpinLock<Vec<(usize, u8, Arc<UsbDisk>)>> = SpinLock::new(Vec::new());

pub fn attach(device: &UsbDevice, iface: &InterfaceInfo) -> Option<&'static str> {
    // Yalnızca SCSI şeffaf komut kümesi + Bulk-Only.
    if iface.subclass != 0x06 || iface.protocol != 0x50 {
        return None;
    }
    let bulk_in = iface
        .endpoints
        .iter()
        .find(|e| e.transfer_type() == 2 && e.is_in())?;
    let bulk_out = iface
        .endpoints
        .iter()
        .find(|e| e.transfer_type() == 2 && !e.is_in())?;
    let configs = [
        EndpointConfig {
            address: bulk_in.address,
            transfer: 2,
            max_packet: bulk_in.max_packet,
            interval: 0,
        },
        EndpointConfig {
            address: bulk_out.address,
            transfer: 2,
            max_packet: bulk_out.max_packet,
            interval: 0,
        },
    ];
    if !host::configure_endpoints(device.controller, device.slot, device.speed, &configs) {
        return None;
    }
    // Veri tamponu 64 KiB sınırını geçmemeli (tek TRB); +512 bayt CBW/CSW.
    let (dma, buffer) = Dma::alloc_aligned((MAX_TRANSFER + 4096) as u64, 128 * 1024)?;
    let mut disk = UsbDisk {
        controller: device.controller,
        slot: device.slot,
        interface: iface.number,
        bulk_in: bulk_in.address,
        bulk_out: bulk_out.address,
        index: 0,
        sectors: 0,
        block_size: 512,
        model: String::new(),
        io: Mutex::new(Io {
            _dma: dma,
            buffer,
            tag: 0,
        }),
        gone: AtomicBool::new(false),
        errors: AtomicU32::new(0),
    };
    let mut inquiry = [0u8; 36];
    if disk
        .command(&[0x12, 0, 0, 0, 36, 0], Direction::In(&mut inquiry))
        .is_ok()
    {
        let vendor = core::str::from_utf8(&inquiry[8..16]).unwrap_or("").trim();
        let product = core::str::from_utf8(&inquiry[16..32]).unwrap_or("").trim();
        disk.model = format!("{} {}", vendor, product).trim().into();
    }
    // Ortam hazır olana kadar TEST UNIT READY (ilk yanıt genellikle UNIT ATTENTION).
    let mut ready = false;
    for _ in 0..10 {
        if disk.command(&[0; 6], Direction::None) == Ok(0) {
            ready = true;
            break;
        }
        let _ = disk.request_sense();
        crate::sched::sleep_ms(100);
    }
    let mut capacity = [0u8; 8];
    if !ready
        || disk.command(
            &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            Direction::In(&mut capacity),
        ) != Ok(0)
    {
        crate::log!("usb: mass storage '{}' has no medium", device.name);
        return None;
    }
    let mut last = u32::from_be_bytes(capacity[0..4].try_into().ok()?) as u64;
    let mut block = u32::from_be_bytes(capacity[4..8].try_into().ok()?);
    if last == u32::MAX as u64 {
        // 2 TiB'tan büyük disk: READ CAPACITY(16).
        let mut long = [0u8; 32];
        let mut cdb = [0u8; 16];
        cdb[0] = 0x9e;
        cdb[1] = 0x10;
        cdb[13] = 32;
        if disk.command(&cdb, Direction::In(&mut long)) == Ok(0) {
            last = u64::from_be_bytes(long[0..8].try_into().ok()?);
            block = u32::from_be_bytes(long[8..12].try_into().ok()?);
        }
    }
    if !matches!(block, 512 | 1024 | 2048 | 4096) {
        crate::log!(
            "usb: '{}' uses {}-byte blocks (unsupported)",
            device.name,
            block
        );
        return None;
    }
    disk.sectors = last.saturating_add(1);
    disk.block_size = block;
    disk.index = NEXT_INDEX.fetch_add(1, Ordering::Relaxed);
    if disk.model.is_empty() {
        disk.model = device.name.clone();
    }
    let disk = Arc::new(disk);
    DISKS
        .lock()
        .push((device.controller, device.slot, disk.clone()));
    storage::add_disk(disk);
    Some("mass storage")
}

pub fn detach(device: &UsbDevice) {
    let removed: Vec<Arc<UsbDisk>> = {
        let mut list = DISKS.lock();
        let (gone, keep): (Vec<_>, Vec<_>) = list
            .drain(..)
            .partition(|(c, s, _)| *c == device.controller && *s == device.slot);
        *list = keep;
        gone.into_iter().map(|(_, _, d)| d).collect()
    };
    for disk in removed {
        disk.gone.store(true, Ordering::Relaxed);
        storage::remove_disk(&disk.name());
    }
}
