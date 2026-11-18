//! Intel 8254x / 82574 (e1000/e1000e) Gigabit Ethernet: DMA halkaları, bağlantı
//! durumu kesmesi ve donanım hata sayaçları.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

use super::pci::{self, Device};
use crate::arch::tsc;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sync::SpinLock;

const CTRL: usize = 0x0000;
const STATUS: usize = 0x0008;
const EERD: usize = 0x0014;
const ICR: usize = 0x00c0;
const IMS: usize = 0x00d0;
const IMC: usize = 0x00d8;
const RCTL: usize = 0x0100;
const TCTL: usize = 0x0400;
const TIPG: usize = 0x0410;
const RDBAL: usize = 0x2800;
const RDBAH: usize = 0x2804;
const RDLEN: usize = 0x2808;
const RDH: usize = 0x2810;
const RDT: usize = 0x2818;
const TDBAL: usize = 0x3800;
const TDBAH: usize = 0x3804;
const TDLEN: usize = 0x3808;
const TDH: usize = 0x3810;
const TDT: usize = 0x3818;
const MTA: usize = 0x5200;
const RAL: usize = 0x5400;
const RAH: usize = 0x5404;
const CRCERRS: usize = 0x4000;
const MPC: usize = 0x4010;

const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
const CTRL_ASDE: u32 = 1 << 5;
const STATUS_FD: u32 = 1 << 0;
const STATUS_LU: u32 = 1 << 1;
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
const INT_LSC: u32 = 1 << 2;
const INT_RXDMT0: u32 = 1 << 4;
const INT_RXO: u32 = 1 << 6;
const INT_RXT0: u32 = 1 << 7;
const TX_EOP: u8 = 1 << 0;
const TX_IFCS: u8 = 1 << 1;
const TX_RS: u8 = 1 << 3;
const DD: u8 = 1 << 0;
const RX_EOP: u8 = 1 << 1;

const RX_COUNT: usize = 256;
const TX_COUNT: usize = 256;
const BUFFER: usize = 2048;

const SUPPORTED: &[(u16, &str)] = &[
    (0x100e, "Intel 82540EM"),
    (0x100f, "Intel 82545EM"),
    (0x1004, "Intel 82543GC"),
    (0x10d3, "Intel 82574L"),
    (0x10f5, "Intel 82567LM"),
    (0x153a, "Intel I217-LM"),
    (0x153b, "Intel I217-V"),
    (0x1502, "Intel 82579LM"),
    (0x1503, "Intel 82579V"),
    (0x155a, "Intel I218-LM"),
    (0x1559, "Intel I218-V"),
    (0x15a0, "Intel I218-LM"),
    (0x15a1, "Intel I218-V"),
    (0x156f, "Intel I219-LM"),
    (0x1570, "Intel I219-V"),
    (0x15b7, "Intel I219-LM"),
    (0x15b8, "Intel I219-V"),
    (0x15bb, "Intel I219-LM"),
    (0x15bc, "Intel I219-V"),
    (0x15d7, "Intel I219-LM"),
    (0x15d8, "Intel I219-V"),
    (0x15e3, "Intel I219-LM"),
    (0x15d6, "Intel I219-V"),
    (0x0d4e, "Intel I219-LM"),
    (0x0d4f, "Intel I219-V"),
    (0x0d4c, "Intel I219-LM"),
    (0x0d4d, "Intel I219-V"),
    (0x150c, "Intel 82583V"),
    (0x105e, "Intel 82571EB"),
    (0x107d, "Intel 82572EI"),
    (0x10b9, "Intel 82572EI"),
];

pub struct E1000 {
    regs: *mut u8,
    rx_ring: u64,
    tx_ring: u64,
    rx_buffers: u64,
    tx_buffers: u64,
    rx_next: usize,
    tx_next: usize,
    pub mac: [u8; 6],
    pub model: &'static str,
    pub irq: u8,
    pub rx_errors: u64,
    pub tx_dropped: u64,
}

unsafe impl Send for E1000 {}

static PENDING_CAUSE: AtomicU32 = AtomicU32::new(0);
static DEVICE_REGS: SpinLock<usize> = SpinLock::new(0);

impl E1000 {
    fn read(&self, reg: usize) -> u32 {
        unsafe { read_volatile(self.regs.add(reg) as *const u32) }
    }

    fn write(&self, reg: usize, value: u32) {
        unsafe { write_volatile(self.regs.add(reg) as *mut u32, value) }
    }

    fn read_eeprom(&self, word: u8, shift: u32, done: u32) -> Option<u16> {
        self.write(EERD, 1 | ((word as u32) << shift));
        let deadline = tsc::now_ns() + 10_000_000;
        loop {
            let value = self.read(EERD);
            if value & done != 0 {
                return Some((value >> 16) as u16);
            }
            if tsc::now_ns() > deadline {
                return None;
            }
        }
    }

    fn read_mac(&self) -> [u8; 6] {
        let high = self.read(RAH);
        if high & (1 << 31) != 0 {
            let low = self.read(RAL);
            return [
                low as u8,
                (low >> 8) as u8,
                (low >> 16) as u8,
                (low >> 24) as u8,
                high as u8,
                (high >> 8) as u8,
            ];
        }
        let mut mac = [0u8; 6];
        for (shift, done) in [(8, 1 << 4), (2, 1 << 1)] {
            let words: Option<Vec<u16>> =
                (0..3).map(|i| self.read_eeprom(i, shift, done)).collect();
            if let Some(words) = words {
                for (i, w) in words.iter().enumerate() {
                    mac[i * 2] = *w as u8;
                    mac[i * 2 + 1] = (*w >> 8) as u8;
                }
                return mac;
            }
        }
        mac
    }

    fn rx_desc(&self, i: usize) -> *mut u8 {
        phys_to_virt(self.rx_ring + (i * 16) as u64)
    }

    fn tx_desc(&self, i: usize) -> *mut u8 {
        phys_to_virt(self.tx_ring + (i * 16) as u64)
    }

    fn link_state(&self) -> (bool, u32, bool) {
        let status = self.read(STATUS);
        let speed = match (status >> 6) & 0x3 {
            0 => 10,
            1 => 100,
            _ => 1000,
        };
        (status & STATUS_LU != 0, speed, status & STATUS_FD != 0)
    }

    fn send_frame(&mut self, frame: &[u8]) -> bool {
        if frame.len() > BUFFER {
            self.tx_dropped += 1;
            return false;
        }
        let i = self.tx_next;
        let desc = self.tx_desc(i);
        let status = unsafe { read_volatile(desc.add(12)) };
        if status & DD == 0 {
            self.tx_dropped += 1;
            return false;
        }
        let buffer = self.tx_buffers + (i * BUFFER) as u64;
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), phys_to_virt(buffer), frame.len());
            write_volatile(desc as *mut u64, buffer);
            write_volatile(desc.add(8) as *mut u16, frame.len().max(60) as u16);
            write_volatile(desc.add(11), TX_EOP | TX_IFCS | TX_RS);
            write_volatile(desc.add(12), 0);
        }
        self.tx_next = (i + 1) % TX_COUNT;
        self.write(TDT, self.tx_next as u32);
        true
    }

    fn receive_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            let i = self.rx_next;
            let desc = self.rx_desc(i);
            let status = unsafe { read_volatile(desc.add(12)) };
            if status & DD == 0 {
                return None;
            }
            let length = unsafe { read_volatile(desc.add(8) as *const u16) } as usize;
            let errors = unsafe { read_volatile(desc.add(13)) };
            let frame = if errors == 0 && status & RX_EOP != 0 && length <= BUFFER {
                let buffer = self.rx_buffers + (i * BUFFER) as u64;
                let mut data = alloc::vec![0u8; length];
                unsafe {
                    core::ptr::copy_nonoverlapping(phys_to_virt(buffer), data.as_mut_ptr(), length)
                };
                Some(data)
            } else {
                self.rx_errors += 1;
                None
            };
            unsafe { write_volatile(desc.add(12), 0) };
            self.write(RDT, i as u32);
            self.rx_next = (i + 1) % RX_COUNT;
            if frame.is_some() {
                return frame;
            }
        }
    }

    fn errors(&self) -> (u32, u32) {
        (self.read(CRCERRS), self.read(MPC))
    }
}

fn on_irq() {
    let regs = *DEVICE_REGS.lock();
    if regs != 0 {
        let cause = unsafe { read_volatile((regs + ICR) as *const u32) };
        PENDING_CAUSE.fetch_or(cause, Ordering::Relaxed);
    }
    crate::net::wake();
}

impl super::nic::Nic for E1000 {
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
        self.send_frame(frame)
    }
    fn receive(&mut self) -> Option<Vec<u8>> {
        self.receive_frame()
    }
    fn link(&self) -> (bool, u32, bool) {
        self.link_state()
    }
    fn take_cause(&mut self) -> (bool, bool) {
        take_cause()
    }
    fn hardware_errors(&self) -> (u32, u32) {
        self.errors()
    }
    fn rx_errors(&self) -> u64 {
        self.rx_errors
    }
}

/// Kesmeden beri biriken nedenler (bağlantı değişti mi?).
fn take_cause() -> (bool, bool) {
    let cause = PENDING_CAUSE.swap(0, Ordering::Relaxed);
    (
        cause & INT_LSC != 0,
        cause & (INT_RXT0 | INT_RXO | INT_RXDMT0) != 0,
    )
}

pub fn probe() -> Option<(E1000, Device)> {
    let (device, model) = pci::devices().into_iter().find_map(|d| {
        if d.vendor_id != 0x8086 {
            return None;
        }
        SUPPORTED
            .iter()
            .find(|(id, _)| *id == d.device_id)
            .map(|(_, m)| (d, *m))
    })?;
    device.enable();
    let base = device.bar(0)?;
    let regs = map_physical(base, 0x20000, NO_CACHE).ok()?;
    let rx_ring = pmm::alloc_dma_frame()?;
    let tx_ring = pmm::alloc_dma_frame()?;
    let rx_buffers = pmm::alloc_contiguous((RX_COUNT * BUFFER) as u64 / PAGE_SIZE)?;
    let tx_buffers = pmm::alloc_contiguous((TX_COUNT * BUFFER) as u64 / PAGE_SIZE)?;
    let mut nic = E1000 {
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
        tx_dropped: 0,
    };

    nic.write(IMC, u32::MAX);
    nic.write(CTRL, nic.read(CTRL) | CTRL_RST);
    let deadline = tsc::now_ns() + 100_000_000;
    while nic.read(CTRL) & CTRL_RST != 0 && tsc::now_ns() < deadline {}
    tsc::busy_wait_us(1000);
    nic.write(IMC, u32::MAX);
    nic.read(ICR);
    nic.write(CTRL, nic.read(CTRL) | CTRL_SLU | CTRL_ASDE);
    nic.mac = nic.read_mac();
    for i in 0..128 {
        nic.write(MTA + i * 4, 0);
    }

    for i in 0..RX_COUNT {
        let desc = nic.rx_desc(i);
        unsafe { write_volatile(desc as *mut u64, rx_buffers + (i * BUFFER) as u64) };
    }
    nic.write(RDBAL, rx_ring as u32);
    nic.write(RDBAH, (rx_ring >> 32) as u32);
    nic.write(RDLEN, (RX_COUNT * 16) as u32);
    nic.write(RDH, 0);
    nic.write(RDT, (RX_COUNT - 1) as u32);
    nic.write(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

    for i in 0..TX_COUNT {
        unsafe { write_volatile(nic.tx_desc(i).add(12), DD) };
    }
    nic.write(TDBAL, tx_ring as u32);
    nic.write(TDBAH, (tx_ring >> 32) as u32);
    nic.write(TDLEN, (TX_COUNT * 16) as u32);
    nic.write(TDH, 0);
    nic.write(TDT, 0);
    nic.write(TCTL, TCTL_EN | TCTL_PSP | (0x10 << 4) | (0x40 << 12));
    nic.write(TIPG, 0x0060_200a);

    *DEVICE_REGS.lock() = regs as usize;
    device.enable_interrupt(on_irq);
    nic.write(IMS, INT_LSC | INT_RXT0 | INT_RXO | INT_RXDMT0);
    Some((nic, device))
}
