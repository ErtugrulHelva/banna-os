//! Realtek RTL8139 (10/100) Ethernet modülü: halka alım tamponu, dört gönderim yuvası.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::ffi::c_void;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

use banna_kmod::{NicOps, Pci, api, dma_alloc, now_ns};

banna_kmod::module!("rtl8139", init);

const IDR0: usize = 0x00;
const TSD0: usize = 0x10;
const TSAD0: usize = 0x20;
const RBSTART: usize = 0x30;
const CR: usize = 0x37;
const CAPR: usize = 0x38;
const IMR: usize = 0x3c;
const ISR: usize = 0x3e;
const TCR: usize = 0x40;
const RCR: usize = 0x44;
const CONFIG1: usize = 0x52;
const MSR: usize = 0x58;

const CR_RESET: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const CR_BUFE: u8 = 1 << 0;
const ISR_ROK: u16 = 1 << 0;
const ISR_TOK: u16 = 1 << 2;
const ISR_RXOVW: u16 = 1 << 4;
const ISR_LINK: u16 = 1 << 5;
const ISR_FOVW: u16 = 1 << 6;
const TSD_OWN: u32 = 1 << 13;

/// 8 KiB halka + 16 bayt + taşma payı (WRAP kipi paketi halkanın sonundan taşırır).
const RX_RING: usize = 8192;
const RX_ALLOC: usize = RX_RING + 16 + 2048;
const TX_SLOTS: usize = 4;
const TX_BUFFER: usize = 2048;

static PENDING: AtomicU16 = AtomicU16::new(0);
static REGS: AtomicUsize = AtomicUsize::new(0);

struct Rtl8139 {
    regs: *mut u8,
    rx: *mut u8,
    rx_phys: u64,
    rx_offset: usize,
    tx: *mut u8,
    tx_phys: u64,
    tx_next: usize,
    mac: [u8; 6],
}

impl Rtl8139 {
    fn r8(&self, reg: usize) -> u8 {
        unsafe { read_volatile(self.regs.add(reg)) }
    }
    fn r32(&self, reg: usize) -> u32 {
        unsafe { read_volatile(self.regs.add(reg) as *const u32) }
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

    fn send(&mut self, frame: &[u8]) -> bool {
        if frame.len() > TX_BUFFER {
            return false;
        }
        let slot = self.tx_next;
        let status = TSD0 + slot * 4;
        // Yuva önceki gönderimi bitirmiş olmalı (OWN = DMA tamamlandı).
        let deadline = now_ns() + 5_000_000;
        while self.r32(status) & TSD_OWN == 0 && self.r32(status) != 0 {
            if now_ns() > deadline {
                return false;
            }
        }
        let len = frame.len().max(60);
        unsafe {
            let dst = self.tx.add(slot * TX_BUFFER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), dst, frame.len());
            core::ptr::write_bytes(dst.add(frame.len()), 0, len - frame.len());
        }
        self.w32(TSAD0 + slot * 4, (self.tx_phys + (slot * TX_BUFFER) as u64) as u32);
        self.w32(status, len as u32);
        self.tx_next = (slot + 1) % TX_SLOTS;
        true
    }

    fn receive(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.r8(CR) & CR_BUFE != 0 {
            return None;
        }
        let (status, len) = unsafe {
            let header = self.rx.add(self.rx_offset);
            (
                read_volatile(header as *const u16),
                read_volatile(header.add(2) as *const u16) as usize,
            )
        };
        if status & 1 == 0 || !(64..=1522).contains(&len) {
            // Bozuk başlık: alıcıyı sıfırla.
            self.w8(CR, CR_TE);
            self.w8(CR, CR_RE | CR_TE);
            self.w32(RBSTART, self.rx_phys as u32);
            self.rx_offset = 0;
            self.w16(CAPR, 0u16.wrapping_sub(16));
            return None;
        }
        let n = (len - 4).min(out.len());
        unsafe { core::ptr::copy_nonoverlapping(self.rx.add(self.rx_offset + 4), out.as_mut_ptr(), n) };
        self.rx_offset = (self.rx_offset + len + 4 + 3) & !3;
        self.rx_offset %= RX_RING;
        self.w16(CAPR, (self.rx_offset as u16).wrapping_sub(16));
        Some(n)
    }
}

extern "C" fn on_irq() {
    let regs = REGS.load(Ordering::Acquire);
    if regs == 0 {
        return;
    }
    let isr = unsafe { read_volatile((regs + ISR) as *const u16) };
    if isr != 0 && isr != 0xffff {
        unsafe { write_volatile((regs + ISR) as *mut u16, isr) };
        PENDING.fetch_or(isr, Ordering::Relaxed);
        (api().net_wake)();
    }
}

fn nic(ctx: *mut c_void) -> &'static mut Rtl8139 {
    unsafe { &mut *(ctx as *mut Rtl8139) }
}

extern "C" fn op_mac(ctx: *mut c_void, out: *mut [u8; 6]) {
    unsafe { *out = nic(ctx).mac };
}

extern "C" fn op_send(ctx: *mut c_void, data: *const u8, len: usize) -> bool {
    nic(ctx).send(unsafe { core::slice::from_raw_parts(data, len) })
}

extern "C" fn op_receive(ctx: *mut c_void, buf: *mut u8, cap: usize) -> isize {
    let out = unsafe { core::slice::from_raw_parts_mut(buf, cap) };
    nic(ctx).receive(out).map_or(-1, |n| n as isize)
}

extern "C" fn op_link(ctx: *mut c_void) -> u32 {
    let msr = nic(ctx).r8(MSR);
    let speed = if msr & (1 << 3) != 0 { 10 } else { 100 };
    (msr & (1 << 2) == 0) as u32 | 1 << 1 | speed << 16
}

extern "C" fn op_take_cause(_: *mut c_void) -> u32 {
    let cause = PENDING.swap(0, Ordering::Relaxed);
    (cause & ISR_LINK != 0) as u32 | ((cause & ISR_ROK != 0) as u32) << 1
}

const MODEL: &str = "Realtek RTL8139 (module)";

static OPS: NicOps = NicOps {
    model: MODEL.as_ptr(),
    model_len: MODEL.len(),
    mac: op_mac,
    send: op_send,
    receive: op_receive,
    link: op_link,
    take_cause: op_take_cause,
};

fn init() -> i32 {
    let Some(device) = Pci::find(0x10ec, 0x8139) else {
        return 1;
    };
    device.enable();
    // BAR1: bellek eşlemeli yazmaçlar.
    let base = device.bar(1);
    if base == 0 {
        return 2;
    }
    let regs = (api().map_mmio)(base, 0x100);
    let (Some(rx), Some(tx)) = (
        dma_alloc((RX_ALLOC as u64).div_ceil(4096)),
        dma_alloc((TX_SLOTS * TX_BUFFER) as u64 / 4096),
    ) else {
        return 3;
    };
    let mut nic = Box::new(Rtl8139 {
        regs,
        rx: rx.virt,
        rx_phys: rx.phys,
        rx_offset: 0,
        tx: tx.virt,
        tx_phys: tx.phys,
        tx_next: 0,
        mac: [0; 6],
    });
    nic.w8(CONFIG1, 0);
    nic.w8(CR, CR_RESET);
    let deadline = now_ns() + 100_000_000;
    while nic.r8(CR) & CR_RESET != 0 {
        if now_ns() > deadline {
            return 4;
        }
    }
    for i in 0..6 {
        nic.mac[i] = nic.r8(IDR0 + i);
    }
    nic.w32(RBSTART, rx.phys as u32);
    nic.w16(IMR, ISR_ROK | ISR_TOK | ISR_RXOVW | ISR_LINK | ISR_FOVW);
    // Kabul: fiziksel adres, çoklu yayın, yayın; WRAP; DMA sınırsız.
    nic.w32(RCR, 0x0e | (1 << 7) | (7 << 8));
    nic.w32(TCR, 6 << 8);
    nic.w8(CR, CR_RE | CR_TE);
    nic.w16(CAPR, 0u16.wrapping_sub(16));
    REGS.store(regs as usize, Ordering::Release);
    device.enable_interrupt(on_irq);
    let ctx = Box::into_raw(nic) as *mut c_void;
    if !(api().register_nic)(&OPS, ctx) {
        return 5;
    }
    0
}
