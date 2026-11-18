//! Realtek RTL8169/8111/8168 (gigabit) ve RTL8101/8102 (10/100) Ethernet: tanımlayıcı
//! halkaları (16 bayt: opts1, opts2, adres). Masaüstü kartlarının çoğu bu ailedendir.
//!
//! QEMU bu kartı taklit etmez; sürücü veri sayfasındaki temel dizileri izler (PHY yaması
//! ve firmware yüklemesi yok; çoğu sürümde bunlarsız çalışır).

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use super::nic::Nic;
use super::pci;
use crate::arch::tsc;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sync::SpinLock;

const IDR0: usize = 0x00;
const MAR0: usize = 0x08;
const DTCCR: usize = 0x10;
const TNPDS: usize = 0x20;
const CR: usize = 0x37;
const TPPOLL: usize = 0x38;
const IMR: usize = 0x3c;
const ISR: usize = 0x3e;
const TCR: usize = 0x40;
const RCR: usize = 0x44;
const CR9346: usize = 0x50;
const CONFIG2: usize = 0x53;
const CONFIG5: usize = 0x56;
const PHY_STATUS: usize = 0x6c;
const RMS: usize = 0xda;
const CPLUS: usize = 0xe0;
const RDSAR: usize = 0xe4;
const MTPS: usize = 0xec;
const PHYAR: usize = 0x60;
const PMCH: usize = 0x6f;
const GPHY_OCP: usize = 0xb8;
const ERIDR: usize = 0x70;
const ERIAR: usize = 0x74;
const DLLPR: usize = 0xd0;
const MISC: usize = 0xf0;
const MISC_1: usize = 0xf2;

/// TxConfig: dinamik FIFO (8168E-VL ve sonrası; yoksa gönderim takılır).
const TXCFG_AUTO_FIFO: u32 = 1 << 7;
const ERIAR_WRITE: u32 = 1 << 31;
const ERI_MASK_0011: u32 = 0x3 << 12;
const ERI_MASK_1111: u32 = 0xf << 12;

/// MISC: açıkken (firmware/sıfırlama sonrası) MAC hiç çerçeve almaz (8168G ve sonrası).
const RXDV_GATED_EN: u32 = 1 << 19;
/// PHY yazmaçları (MII).
const BMCR: u32 = 0;
const BMCR_ANENABLE: u16 = 1 << 12;
const BMCR_ANRESTART: u16 = 1 << 9;
const BMCR_PDOWN: u16 = 1 << 11;
/// 8168G ve sonrası standart PHY yazmaçlarına OCP üzerinden erişir.
const OCP_STD_PHY_BASE: u32 = 0xa400;

const CR_RESET: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const OWN: u32 = 1 << 31;
const EOR: u32 = 1 << 30;
const FS: u32 = 1 << 29;
const LS: u32 = 1 << 28;
const RX_RES: u32 = 1 << 21;
const INT_ROK: u16 = 1 << 0;
const INT_RER: u16 = 1 << 1;
const INT_TOK: u16 = 1 << 2;
const INT_TER: u16 = 1 << 3;
const INT_RDU: u16 = 1 << 4;
const INT_LINK: u16 = 1 << 5;
const INT_FOVW: u16 = 1 << 6;

const RX_COUNT: usize = 128;
const TX_COUNT: usize = 64;
const BUFFER: usize = 2048;

const SUPPORTED: &[(u16, u16, &str)] = &[
    (0x10ec, 0x8168, "Realtek RTL8111/8168"),
    (0x10ec, 0x8161, "Realtek RTL8111/8168"),
    (0x10ec, 0x8169, "Realtek RTL8169"),
    (0x10ec, 0x8167, "Realtek RTL8169SC"),
    (0x10ec, 0x8136, "Realtek RTL8101/8102"),
    (0x1186, 0x4300, "D-Link DGE-528T"),
    (0x1186, 0x4302, "D-Link DGE-530T"),
    (0x16ec, 0x0116, "USRobotics 997902"),
];

static PENDING: AtomicU16 = AtomicU16::new(0);
static IRQS: AtomicU64 = AtomicU64::new(0);
static REGS: SpinLock<usize> = SpinLock::new(0);

pub struct Rtl8169 {
    regs: *mut u8,
    rx_ring: u64,
    tx_ring: u64,
    rx_buffers: u64,
    tx_buffers: u64,
    rx_next: usize,
    tx_next: usize,
    mac: [u8; 6],
    model: &'static str,
    irq: u8,
    rx_errors: u64,
    tally: u64,
    reports: u32,
    frames: u64,
}

unsafe impl Send for Rtl8169 {}

impl Rtl8169 {
    fn r8(&self, reg: usize) -> u8 {
        unsafe { read_volatile(self.regs.add(reg)) }
    }
    fn r32(&self, reg: usize) -> u32 {
        unsafe { read_volatile(self.regs.add(reg) as *const u32) }
    }
    /// Yonga sürümü (TxConfig XID; Linux r8169 ile aynı maske).
    fn xid(&self) -> u16 {
        ((self.r32(TCR) >> 20) & 0xfcf) as u16
    }

    /// 8168G (XID 0x4c0) ve sonrası: OCP ile PHY erişimi, RX kapısı, PLL gücü.
    fn modern(&self) -> bool {
        self.xid() >= 0x4c0
    }

    /// 8168E-VL/8168F ve sonrası: ERI yazmaçları, kalıcı MAC ERI 0xe0'da, otomatik FIFO.
    fn evl_up(&self) -> bool {
        let x = self.xid();
        self.modern()
            || x & 0x7c8 == 0x2c8
            || matches!(x & 0x7cf, 0x480 | 0x481)
            || x & 0x7c8 == 0x488
    }

    /// 8168H (XID 0x540/0x541): ek güç tasarrufu bitleri kapatılır.
    fn is_8168h(&self) -> bool {
        matches!(self.xid() & 0x7cf, 0x540 | 0x541)
    }

    fn eri_read(&self, addr: u32) -> u32 {
        self.w32(ERIAR, ERI_MASK_1111 | addr);
        if self.wait_flag(ERIAR, true) {
            self.r32(ERIDR)
        } else {
            !0
        }
    }

    fn eri_write(&self, addr: u32, mask: u32, value: u32) {
        self.w32(ERIDR, value);
        self.w32(ERIAR, ERIAR_WRITE | mask | addr);
        if !self.wait_flag(ERIAR, false) {
            crate::log!("rtl8169: ERI write {:#x} timed out", addr);
        }
    }

    fn eri_modify(&self, addr: u32, clear: u32, set: u32) {
        let value = (self.eri_read(addr) & !clear) | set;
        self.eri_write(addr, ERI_MASK_1111, value);
    }

    /// Linux r8169 rtl_hw_start_8168g/h'nin ağ trafiği için gerekli kısmı: FIFO
    /// eşikleri, paket filtresi sıfırlama, RX kapısı, PCIe güç tasarrufu kapalı.
    fn start_8168g(&self) {
        self.eri_write(0xc8, ERI_MASK_1111, 0x08 << 16 | 0x02);
        self.eri_write(0xe8, ERI_MASK_1111, 0x10 << 16 | 0x06);
        self.eri_modify(0xdc, 1, 0);
        self.eri_modify(0xdc, 0, 1);
        if self.is_8168h() {
            self.eri_modify(0xdc, 0, 0x1c);
            self.eri_write(0x5f0, ERI_MASK_0011, 0x4f87);
        } else {
            self.eri_write(0x2f8, ERI_MASK_0011, 0x1d8f);
        }
        self.w32(MISC, self.r32(MISC) & !RXDV_GATED_EN);
        self.eri_write(0xc0, ERI_MASK_0011, 0);
        self.eri_write(0xb8, ERI_MASK_0011, 0);
        if self.is_8168h() {
            self.w8(DLLPR, self.r8(DLLPR) & !(1 << 6 | 1 << 7));
            self.w8(MISC_1, self.r8(MISC_1) & !(1 << 6));
        }
        self.eri_modify(0x1b0, 1 << 12, 0);
        // ASPM + CLKREQ kapalı: L1'e giren kart alımı durdurabiliyor.
        self.w8(CONFIG5, self.r8(CONFIG5) & !1);
        self.w8(CONFIG2, self.r8(CONFIG2) & !(1 << 7));
    }

    /// 8168E-VL ve sonrasında asıl MAC ERI'dedir; IDR sıfırlamadan sonra boş kalabilir.
    fn read_mac(&mut self) {
        let idr: [u8; 6] = core::array::from_fn(|i| self.r8(IDR0 + i));
        let mut mac = idr;
        if self.evl_up() {
            let low = self.eri_read(0xe0).to_le_bytes();
            let high = self.eri_read(0xe4).to_le_bytes();
            let eri = [low[0], low[1], low[2], low[3], high[0], high[1]];
            if valid_mac(&eri) {
                mac = eri;
            }
        }
        if !valid_mac(&mac) {
            crate::log!("rtl8169: no valid MAC in IDR/ERI, using a random local one");
            let r = crate::net::random_u32().to_le_bytes();
            mac = [0x02, 0xba, r[0], r[1], r[2], r[3]];
        }
        self.mac = mac;
        if mac != idr {
            self.w8(CR9346, 0xc0);
            self.w32(IDR0 + 4, u16::from_le_bytes([mac[4], mac[5]]) as u32);
            let _ = self.r32(IDR0 + 4);
            self.w32(IDR0, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
            let _ = self.r32(IDR0);
            self.w8(CR9346, 0);
        }
    }

    /// Donanım sayaçları (DTCCR ile DMA'ya dökülür).
    fn tally(&self) -> Option<[u64; 6]> {
        self.w32(DTCCR + 4, (self.tally >> 32) as u32);
        self.w32(DTCCR, self.tally as u32 | 1 << 3);
        let deadline = tsc::now_ns() + 10_000_000;
        while self.r32(DTCCR) & 1 << 3 != 0 {
            if tsc::now_ns() > deadline {
                return None;
            }
            core::hint::spin_loop();
        }
        let p = phys_to_virt(self.tally);
        unsafe {
            let r64 = |o: usize| read_volatile(p.add(o) as *const u64);
            let r32 = |o: usize| read_volatile(p.add(o) as *const u32) as u64;
            let r16 = |o: usize| read_volatile(p.add(o) as *const u16) as u64;
            Some([r64(0), r64(8), r64(16), r32(24), r16(28), r64(48)])
        }
    }

    fn wait_flag(&self, reg: usize, set: bool) -> bool {
        let deadline = tsc::now_ns() + 20_000_000;
        while (self.r32(reg) & (1 << 31) != 0) != set {
            if tsc::now_ns() > deadline {
                return false;
            }
            core::hint::spin_loop();
        }
        true
    }

    fn phy_read(&self, reg: u32) -> Option<u16> {
        if self.modern() {
            let addr = OCP_STD_PHY_BASE + reg * 2;
            self.w32(GPHY_OCP, addr << 15);
            self.wait_flag(GPHY_OCP, true)
                .then(|| self.r32(GPHY_OCP) as u16)
        } else {
            self.w32(PHYAR, (reg & 0x1f) << 16);
            self.wait_flag(PHYAR, true).then(|| self.r32(PHYAR) as u16)
        }
    }

    fn phy_write(&self, reg: u32, value: u16) -> bool {
        if self.modern() {
            let addr = OCP_STD_PHY_BASE + reg * 2;
            self.w32(GPHY_OCP, 1 << 31 | addr << 15 | value as u32);
            self.wait_flag(GPHY_OCP, false)
        } else {
            self.w32(PHYAR, 1 << 31 | (reg & 0x1f) << 16 | value as u32);
            self.wait_flag(PHYAR, false)
        }
    }

    /// UEFI, ağdan açılış kullanılmadıysa PLL'yi ve PHY'yi kapalı bırakabilir: bağlantı
    /// hiç kurulmaz. Linux r8169'daki gibi açılır ve otomatik anlaşma yeniden başlatılır.
    fn power_up(&self) {
        if self.modern() {
            self.w8(PMCH, self.r8(PMCH) | 0xc0);
        }
        match self.phy_read(BMCR) {
            Some(bmcr) => {
                let value = (bmcr & !BMCR_PDOWN) | BMCR_ANENABLE | BMCR_ANRESTART;
                if !self.phy_write(BMCR, value) {
                    crate::log!("rtl8169: PHY write timed out");
                }
            }
            None => crate::log!("rtl8169: PHY read timed out"),
        }
    }

    fn w8(&self, reg: usize, v: u8) {
        unsafe { write_volatile(self.regs.add(reg), v) }
    }
    fn w16(&self, reg: usize, v: u16) {
        unsafe { write_volatile(self.regs.add(reg) as *mut u16, v) }
    }
    fn w32(&self, reg: usize, v: u32) {
        unsafe { write_volatile(self.regs.add(reg) as *mut u32, v) }
    }

    fn desc(ring: u64, i: usize) -> *mut u32 {
        phys_to_virt(ring + (i * 16) as u64) as *mut u32
    }

    fn arm_rx(&self, i: usize) {
        let d = Self::desc(self.rx_ring, i);
        let eor = if i == RX_COUNT - 1 { EOR } else { 0 };
        unsafe {
            write_volatile(d.add(1), 0);
            write_volatile(d.add(2) as *mut u64, self.rx_buffers + (i * BUFFER) as u64);
            core::sync::atomic::fence(Ordering::Release);
            write_volatile(d, OWN | eor | BUFFER as u32);
        }
    }
}

fn on_irq() {
    let regs = *REGS.lock();
    if regs == 0 {
        return;
    }
    let isr = unsafe { read_volatile((regs + ISR) as *const u16) };
    if isr != 0 && isr != 0xffff {
        IRQS.fetch_add(1, Ordering::Relaxed);
        unsafe { write_volatile((regs + ISR) as *mut u16, isr) };
        PENDING.fetch_or(isr, Ordering::Relaxed);
        crate::net::wake();
    }
}

impl Nic for Rtl8169 {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn model(&self) -> &'static str {
        self.model
    }
    fn irq(&self) -> u8 {
        self.irq
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if frame.len() > BUFFER {
            return false;
        }
        let i = self.tx_next;
        let d = Self::desc(self.tx_ring, i);
        if unsafe { read_volatile(d) } & OWN != 0 {
            return false;
        }
        let buffer = self.tx_buffers + (i * BUFFER) as u64;
        let len = frame.len().max(60);
        unsafe {
            let dst = phys_to_virt(buffer);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
            core::ptr::write_bytes(dst.add(frame.len()), 0, len - frame.len());
            write_volatile(d.add(1), 0);
            write_volatile(d.add(2) as *mut u64, buffer);
            core::sync::atomic::fence(Ordering::Release);
            let eor = if i == TX_COUNT - 1 { EOR } else { 0 };
            write_volatile(d, OWN | eor | FS | LS | len as u32);
        }
        self.tx_next = (i + 1) % TX_COUNT;
        self.w8(TPPOLL, 0x40);
        true
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        let i = self.rx_next;
        let d = Self::desc(self.rx_ring, i);
        let opts = unsafe { read_volatile(d) };
        if opts & OWN != 0 {
            return None;
        }
        core::sync::atomic::fence(Ordering::Acquire);
        let len = (opts & 0x3fff) as usize;
        let ok = opts & RX_RES == 0 && opts & FS != 0 && opts & LS != 0 && len > 4;
        let frame = ok.then(|| unsafe {
            core::slice::from_raw_parts(
                phys_to_virt(self.rx_buffers + (i * BUFFER) as u64),
                (len - 4).min(BUFFER),
            )
            .to_vec()
        });
        if !ok {
            self.rx_errors += 1;
        } else {
            self.frames += 1;
            if self.frames == 200 && IRQS.load(Ordering::Relaxed) == 0 {
                crate::log!("rtl8169: no interrupts after 200 frames, running on polling");
            }
        }
        self.arm_rx(i);
        self.rx_next = (i + 1) % RX_COUNT;
        // Hatalı tanımlayıcıyı atla, sıradakine bak.
        frame.or_else(|| self.receive())
    }

    fn link(&self) -> (bool, u32, bool) {
        let s = self.r8(PHY_STATUS);
        let speed = if s & (1 << 4) != 0 {
            1000
        } else if s & (1 << 3) != 0 {
            100
        } else {
            10
        };
        (s & (1 << 1) != 0, speed, s & 1 != 0)
    }

    fn take_cause(&mut self) -> (bool, bool) {
        let cause = PENDING.swap(0, Ordering::Relaxed);
        if cause & (INT_RER | INT_RDU | INT_FOVW) != 0 {
            self.rx_errors += 1;
        }
        let _ = INT_TER;
        (cause & INT_LINK != 0, cause & INT_ROK != 0)
    }

    fn rx_errors(&self) -> u64 {
        self.rx_errors
    }

    fn hardware_errors(&self) -> (u32, u32) {
        match self.tally() {
            Some([_, _, _, rx_err, missed, _]) => (rx_err as u32, missed as u32),
            None => (0, 0),
        }
    }

    fn diagnostics(&mut self) -> Option<alloc::string::String> {
        if self.reports >= 8 {
            return None;
        }
        self.reports += 1;
        let stuck = (0..TX_COUNT)
            .filter(|&i| unsafe { read_volatile(Self::desc(self.tx_ring, i)) } & OWN != 0)
            .count();
        let counters = match self.tally() {
            Some([tx, rx, tx_err, rx_err, missed, bcast]) => alloc::format!(
                "tx {} rx {} (bcast {}) tx_err {} rx_err {} missed {}",
                tx,
                rx,
                bcast,
                tx_err,
                rx_err,
                missed
            ),
            None => alloc::string::String::from("tally timed out"),
        };
        Some(alloc::format!(
            "rtl8169: {} | irqs {} | tx desc pending {} rx_next {} | CR {:#x} RCR {:#x} TCR {:#x} MISC {:#x} PHY {:#x} IMR {:#x}",
            counters,
            IRQS.load(Ordering::Relaxed),
            stuck,
            self.rx_next,
            self.r8(CR),
            self.r32(RCR),
            self.r32(TCR),
            self.r32(MISC),
            self.r8(PHY_STATUS),
            unsafe { read_volatile(self.regs.add(IMR) as *const u16) },
        ))
    }
}

fn valid_mac(mac: &[u8; 6]) -> bool {
    mac[0] & 1 == 0 && mac.iter().any(|&b| b != 0)
}

/// PCIe ASPM (Link Control bit 0-1) kapatılır ve kart D0'a alınır.
fn wake_device(device: &pci::Device) {
    if let Some(&pm) = device.capabilities(0x01).first() {
        let pmcsr = device.read(pm + 4);
        if pmcsr & 3 != 0 {
            device.write(pm + 4, pmcsr & !3);
            let deadline = tsc::now_ns() + 10_000_000;
            while tsc::now_ns() < deadline {
                core::hint::spin_loop();
            }
        }
    }
    if let Some(&express) = device.capabilities(0x10).first() {
        let control = device.read(express + 0x10);
        if control & 3 != 0 {
            device.write(express + 0x10, control & !3);
        }
    }
}

pub fn probe() -> Option<Rtl8169> {
    let (device, model) = pci::devices().into_iter().find_map(|d| {
        SUPPORTED
            .iter()
            .find(|(v, id, _)| *v == d.vendor_id && *id == d.device_id)
            .map(|(_, _, m)| (d, *m))
    })?;
    wake_device(&device);
    device.enable();
    // Bellek eşlemeli yazmaçlar: 8169'da BAR1, 8168/8101'de BAR2 (64 bit).
    let base = [1u8, 2]
        .iter()
        .filter_map(|&i| {
            let low = device.read(0x10 + i * 4);
            (low & 1 == 0 && low != 0).then(|| device.bar(i)).flatten()
        })
        .find(|&b| b != 0)?;
    let regs = map_physical(base, 0x100, NO_CACHE).ok()?;
    let rx_ring = pmm::alloc_contiguous(1)?;
    let tx_ring = pmm::alloc_contiguous(1)?;
    let rx_buffers = pmm::alloc_contiguous((RX_COUNT * BUFFER) as u64 / PAGE_SIZE)?;
    let tx_buffers = pmm::alloc_contiguous((TX_COUNT * BUFFER) as u64 / PAGE_SIZE)?;
    let tally = pmm::alloc_contiguous(1)?;
    unsafe {
        core::ptr::write_bytes(phys_to_virt(tally), 0, PAGE_SIZE as usize);
        core::ptr::write_bytes(phys_to_virt(rx_ring), 0, PAGE_SIZE as usize);
        core::ptr::write_bytes(phys_to_virt(tx_ring), 0, PAGE_SIZE as usize);
    }
    let mut nic = Rtl8169 {
        regs,
        rx_ring,
        tx_ring,
        rx_buffers,
        tx_buffers,
        rx_next: 0,
        tx_next: 0,
        mac: [0; 6],
        model,
        irq: device.interrupt_line,
        rx_errors: 0,
        tally,
        reports: 0,
        frames: 0,
    };
    nic.w16(IMR, 0);
    nic.w8(CR, CR_RESET);
    let deadline = tsc::now_ns() + 100_000_000;
    while nic.r8(CR) & CR_RESET != 0 {
        if tsc::now_ns() > deadline {
            crate::log!("rtl8169: reset timed out");
            return None;
        }
    }
    nic.read_mac();
    let modern = nic.modern();
    nic.power_up();
    for i in 0..RX_COUNT {
        nic.arm_rx(i);
    }
    let last_tx = Rtl8169::desc(tx_ring, TX_COUNT - 1);
    unsafe { write_volatile(last_tx, EOR) };

    nic.w8(CR9346, 0xc0);
    if modern {
        nic.start_8168g();
    }
    nic.w16(CPLUS, 0);
    nic.w16(RMS, BUFFER as u16 - 1);
    nic.w8(MTPS, 0x3b);
    nic.w32(TNPDS, tx_ring as u32);
    nic.w32(TNPDS + 4, (tx_ring >> 32) as u32);
    nic.w32(RDSAR, rx_ring as u32);
    nic.w32(RDSAR + 4, (rx_ring >> 32) as u32);
    nic.w8(CR, CR_RE | CR_TE);
    // Fiziksel adres + çoklu yayın + yayın; 8168G+ için Linux'taki RX128_INT_EN |
    // RX_MULTI_EN | RX_EARLY_OFF | sınırsız DMA, eskilerde eşik yok + sınırsız DMA.
    let rx_config = if modern {
        (1 << 15) | (1 << 14) | (1 << 11) | (7 << 8)
    } else {
        (7 << 13) | (7 << 8)
    };
    nic.w32(RCR, rx_config | 0x0e);
    let auto_fifo = if nic.evl_up() { TXCFG_AUTO_FIFO } else { 0 };
    nic.w32(TCR, (3 << 24) | (7 << 8) | auto_fifo);
    nic.w32(MAR0, 0xffff_ffff);
    nic.w32(MAR0 + 4, 0xffff_ffff);
    nic.w8(CR9346, 0);
    *REGS.lock() = regs as usize;
    device.enable_interrupt(on_irq);
    nic.w16(ISR, 0xffff);
    nic.w16(
        IMR,
        INT_ROK | INT_RER | INT_TOK | INT_TER | INT_RDU | INT_LINK | INT_FOVW,
    );
    let _ = nic.r32(RCR);
    let (up, speed, _) = nic.link();
    let m = nic.mac;
    crate::log!(
        "rtl8169: {} XID {:#05x}{} MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, link {}",
        model,
        nic.xid(),
        if modern { " (8168G+)" } else { "" },
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        if up {
            alloc::format!("up {} Mb/s", speed)
        } else {
            alloc::string::String::from("down (autoneg)")
        }
    );
    Some(nic)
}
