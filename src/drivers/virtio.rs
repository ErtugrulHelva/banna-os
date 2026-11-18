//! Virtio 1.x PCI ("modern") taşıma katmanı ve split virtqueue.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use super::pci::Device;
use crate::arch::tsc;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};

const CAP_VENDOR: u8 = 0x09;
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_ISR: u8 = 3;
const CFG_DEVICE: u8 = 4;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const FEATURE_VERSION_1: u64 = 1 << 32;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const MAX_QUEUE: u16 = 64;
const TIMEOUT_NS: u64 = 1_000_000_000;

pub struct Transport {
    common: *mut u8,
    notify: *mut u8,
    notify_multiplier: u32,
    isr: Option<*mut u8>,
    device: Option<*mut u8>,
}

unsafe impl Send for Transport {}

fn map_cap(device: &Device, cap: u8) -> Option<(*mut u8, u32)> {
    let bar = (device.read(cap + 4) & 0xff) as u8;
    let offset = device.read(cap + 8) as u64;
    let length = device.read(cap + 12) as u64;
    let base = device.bar(bar)?;
    let ptr = map_physical(base + offset, length.max(4), NO_CACHE).ok()?;
    Some((ptr, length as u32))
}

impl Transport {
    pub fn new(device: &Device) -> Option<Self> {
        device.enable();
        let mut common = None;
        let mut notify = None;
        let mut notify_multiplier = 0;
        let mut isr = None;
        let mut device_cfg = None;
        for cap in device.capabilities(CAP_VENDOR) {
            let kind = (device.read(cap) >> 24) as u8;
            match kind {
                CFG_COMMON => common = map_cap(device, cap).map(|m| m.0),
                CFG_NOTIFY => {
                    notify = map_cap(device, cap).map(|m| m.0);
                    notify_multiplier = device.read(cap + 16);
                }
                CFG_ISR => isr = map_cap(device, cap).map(|m| m.0),
                CFG_DEVICE => device_cfg = map_cap(device, cap).map(|m| m.0),
                _ => {}
            }
        }
        Some(Self {
            common: common?,
            notify: notify?,
            notify_multiplier,
            isr,
            device: device_cfg,
        })
    }

    /// Aygıta özgü yapılandırma alanı (ör. virtio-net MAC adresi).
    pub fn device_config(&self) -> Option<*mut u8> {
        self.device
    }

    /// ISR durum yazmacı (okumak kesmeyi onaylar).
    pub fn isr(&self) -> Option<*mut u8> {
        self.isr
    }

    fn r8(&self, off: usize) -> u8 {
        unsafe { read_volatile(self.common.add(off)) }
    }
    fn w8(&self, off: usize, v: u8) {
        unsafe { write_volatile(self.common.add(off), v) }
    }
    fn r16(&self, off: usize) -> u16 {
        unsafe { read_volatile(self.common.add(off) as *const u16) }
    }
    fn w16(&self, off: usize, v: u16) {
        unsafe { write_volatile(self.common.add(off) as *mut u16, v) }
    }
    fn r32(&self, off: usize) -> u32 {
        unsafe { read_volatile(self.common.add(off) as *const u32) }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { write_volatile(self.common.add(off) as *mut u32, v) }
    }
    fn w64(&self, off: usize, v: u64) {
        self.w32(off, v as u32);
        self.w32(off + 4, (v >> 32) as u32);
    }

    /// Sıfırlama + özellik anlaşması; `wanted` aygıtın sunduklarıyla kesiştirilir.
    pub fn negotiate(&self, wanted: u64) -> Result<u64, &'static str> {
        self.w8(0x14, 0);
        let deadline = tsc::now_ns() + TIMEOUT_NS;
        while self.r8(0x14) != 0 {
            if tsc::now_ns() > deadline {
                return Err("virtio reset timeout");
            }
        }
        self.w8(0x14, STATUS_ACKNOWLEDGE);
        self.w8(0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        self.w32(0x00, 0);
        let low = self.r32(0x04) as u64;
        self.w32(0x00, 1);
        let high = self.r32(0x04) as u64;
        let offered = low | high << 32;
        if offered & FEATURE_VERSION_1 == 0 {
            return Err("device is not virtio 1.0");
        }
        let accepted = offered & (wanted | FEATURE_VERSION_1);
        self.w32(0x08, 0);
        self.w32(0x0c, accepted as u32);
        self.w32(0x08, 1);
        self.w32(0x0c, (accepted >> 32) as u32);
        self.w8(
            0x14,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        if self.r8(0x14) & STATUS_FEATURES_OK == 0 {
            return Err("virtio feature negotiation failed");
        }
        Ok(accepted)
    }

    pub fn driver_ok(&self) {
        let status = self.r8(0x14);
        self.w8(0x14, status | STATUS_DRIVER_OK);
    }

    pub fn setup_queue(&self, index: u16) -> Result<Queue, &'static str> {
        self.w16(0x16, index);
        let max = self.r16(0x18);
        if max == 0 {
            return Err("virtqueue not available");
        }
        let size = max.min(MAX_QUEUE);
        self.w16(0x18, size);
        let memory = pmm::alloc_contiguous(2).ok_or("out of memory for virtqueue")?;
        unsafe { core::ptr::write_bytes(phys_to_virt(memory), 0, 2 * PAGE_SIZE as usize) };
        let desc = memory;
        let avail = memory + 16 * size as u64;
        let used = memory + PAGE_SIZE;
        self.w64(0x20, desc);
        self.w64(0x28, avail);
        self.w64(0x30, used);
        let notify_off = self.r16(0x1e) as usize;
        self.w16(0x1c, 1);
        Ok(Queue {
            size,
            desc,
            avail,
            used,
            notify: unsafe {
                self.notify
                    .add(notify_off * self.notify_multiplier as usize)
            } as *mut u16,
            index,
            next_avail: 0,
            last_used: 0,
        })
    }
}

pub struct Queue {
    size: u16,
    desc: u64,
    avail: u64,
    used: u64,
    notify: *mut u16,
    index: u16,
    next_avail: u16,
    last_used: u16,
}

unsafe impl Send for Queue {}

impl Queue {
    pub fn size(&self) -> u16 {
        self.size
    }

    /// `i` numaralı tanımlayıcıyı tek parça tampon olarak kullanılabilir halkaya koyar
    /// (`writable`: aygıt yazar). Aygıta haber vermek için `kick`.
    pub fn post(&mut self, i: u16, addr: u64, len: u32, writable: bool) {
        self.write_desc(i, addr, len, if writable { DESC_WRITE } else { 0 }, 0);
        let avail = phys_to_virt(self.avail);
        unsafe {
            let slot = 4 + 2 * (self.next_avail % self.size) as usize;
            write_volatile(avail.add(slot) as *mut u16, i);
            fence(Ordering::SeqCst);
            self.next_avail = self.next_avail.wrapping_add(1);
            write_volatile(avail.add(2) as *mut u16, self.next_avail);
        }
    }

    pub fn kick(&self) {
        fence(Ordering::SeqCst);
        unsafe { write_volatile(self.notify, self.index) };
    }

    /// Aygıtın bitirdiği sıradaki tanımlayıcı: (numara, yazılan bayt).
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        let used = phys_to_virt(self.used);
        let idx = unsafe { read_volatile(used.add(2) as *const u16) };
        if idx == self.last_used {
            return None;
        }
        fence(Ordering::SeqCst);
        let slot = 4 + 8 * (self.last_used % self.size) as usize;
        let (id, len) = unsafe {
            (
                read_volatile(used.add(slot) as *const u32),
                read_volatile(used.add(slot + 4) as *const u32),
            )
        };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id as u16, len))
    }

    fn write_desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let base = phys_to_virt(self.desc + 16 * i as u64);
        unsafe {
            write_volatile(base as *mut u64, addr);
            write_volatile(base.add(8) as *mut u32, len);
            write_volatile(base.add(12) as *mut u16, flags);
            write_volatile(base.add(14) as *mut u16, next);
        }
    }

    /// Okunacak `request` ve yazılacak `response` (fiziksel adres, uzunluk) zincirini
    /// gönderir ve tamamlanmasını yoklayarak bekler.
    pub fn submit(
        &mut self,
        request: (u64, u32),
        response: Option<(u64, u32)>,
    ) -> Result<(), &'static str> {
        match response {
            Some((addr, len)) => {
                self.write_desc(0, request.0, request.1, DESC_NEXT, 1);
                self.write_desc(1, addr, len, DESC_WRITE, 0);
            }
            None => self.write_desc(0, request.0, request.1, 0, 0),
        }
        let avail = phys_to_virt(self.avail);
        unsafe {
            let slot = 4 + 2 * (self.next_avail % self.size) as usize;
            write_volatile(avail.add(slot) as *mut u16, 0);
            fence(Ordering::SeqCst);
            self.next_avail = self.next_avail.wrapping_add(1);
            write_volatile(avail.add(2) as *mut u16, self.next_avail);
            fence(Ordering::SeqCst);
            write_volatile(self.notify, self.index);
        }
        let used_idx = unsafe { phys_to_virt(self.used).add(2) as *const u16 };
        let deadline = tsc::now_ns() + TIMEOUT_NS;
        while unsafe { read_volatile(used_idx) } == self.last_used {
            if tsc::now_ns() > deadline {
                return Err("virtqueue timeout");
            }
            core::hint::spin_loop();
        }
        self.last_used = self.last_used.wrapping_add(1);
        Ok(())
    }
}
