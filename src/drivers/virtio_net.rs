//! virtio-net (QEMU, UTM ve çoğu sanal makine): alım kuyruğu (0) ve gönderim kuyruğu (1).
//! Her çerçevenin önünde 12 baytlık virtio-net başlığı vardır.

use alloc::vec::Vec;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::nic::Nic;
use super::pci;
use super::virtio::{Queue, Transport};
use crate::arch::idt;
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};

const F_MAC: u64 = 1 << 5;
const F_STATUS: u64 = 1 << 16;
const HEADER: usize = 12;
const BUFFER: usize = 2048;

static ISR: AtomicUsize = AtomicUsize::new(0);
static LINK_EVENT: AtomicBool = AtomicBool::new(false);

pub struct VirtioNet {
    transport: Transport,
    rx: Queue,
    tx: Queue,
    rx_buffers: u64,
    tx_buffers: u64,
    tx_free: Vec<u16>,
    mac: [u8; 6],
    status: bool,
    irq: u8,
}

unsafe impl Send for VirtioNet {}

fn on_irq() {
    let isr = ISR.load(Ordering::Relaxed);
    if isr == 0 {
        return;
    }
    let cause = unsafe { read_volatile(isr as *const u8) };
    if cause != 0 {
        if cause & 2 != 0 {
            LINK_EVENT.store(true, Ordering::Relaxed);
        }
        crate::net::wake();
    }
}

impl VirtioNet {
    fn reclaim_tx(&mut self) {
        while let Some((id, _)) = self.tx.pop_used() {
            self.tx_free.push(id);
        }
    }
}

impl Nic for VirtioNet {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn model(&self) -> &'static str {
        "virtio-net"
    }
    fn irq(&self) -> u8 {
        self.irq
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if frame.len() + HEADER > BUFFER {
            return false;
        }
        self.reclaim_tx();
        let Some(i) = self.tx_free.pop() else {
            return false;
        };
        let buffer = self.tx_buffers + (i as usize * BUFFER) as u64;
        unsafe {
            let dst = phys_to_virt(buffer);
            core::ptr::write_bytes(dst, 0, HEADER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), dst.add(HEADER), frame.len());
        }
        self.tx
            .post(i, buffer, (HEADER + frame.len()) as u32, false);
        self.tx.kick();
        true
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        let (id, len) = self.rx.pop_used()?;
        let buffer = self.rx_buffers + (id as usize * BUFFER) as u64;
        let len = (len as usize).min(BUFFER);
        let frame = (len > HEADER).then(|| unsafe {
            core::slice::from_raw_parts(phys_to_virt(buffer).add(HEADER), len - HEADER).to_vec()
        });
        self.rx.post(id, buffer, BUFFER as u32, true);
        self.rx.kick();
        frame.or_else(|| self.receive())
    }

    fn link(&self) -> (bool, u32, bool) {
        let up = match (self.status, self.transport.device_config()) {
            (true, Some(cfg)) => (unsafe { read_volatile(cfg.add(6) as *const u16) } & 1) != 0,
            _ => true,
        };
        (up, 1000, true)
    }

    fn take_cause(&mut self) -> (bool, bool) {
        (LINK_EVENT.swap(false, Ordering::Relaxed), true)
    }
}

pub fn probe() -> Option<VirtioNet> {
    // Geçişli (0x1000) ya da modern (0x1041) virtio-net.
    let device = pci::find_id(0x1af4, 0x1041).or_else(|| pci::find_id(0x1af4, 0x1000))?;
    let transport = Transport::new(&device)?;
    let features = transport.negotiate(F_MAC | F_STATUS).ok()?;
    let mut rx = transport.setup_queue(0).ok()?;
    let mut tx = transport.setup_queue(1).ok()?;
    let rx_count = rx.size() as usize;
    let tx_count = tx.size() as usize;
    let rx_buffers = pmm::alloc_contiguous((rx_count * BUFFER) as u64 / PAGE_SIZE)?;
    let tx_buffers = pmm::alloc_contiguous((tx_count * BUFFER) as u64 / PAGE_SIZE)?;
    let mut mac = [0u8; 6];
    if features & F_MAC != 0
        && let Some(cfg) = transport.device_config()
    {
        for (i, b) in mac.iter_mut().enumerate() {
            *b = unsafe { read_volatile(cfg.add(i)) };
        }
    } else {
        mac = [0x52, 0x54, 0x00, 0xba, 0x00, 0x01];
    }
    for i in 0..rx_count {
        rx.post(
            i as u16,
            rx_buffers + (i * BUFFER) as u64,
            BUFFER as u32,
            true,
        );
    }
    transport.driver_ok();
    rx.kick();
    let _ = &mut tx;
    if let Some(isr) = transport.isr() {
        ISR.store(isr as usize, Ordering::Relaxed);
    }
    let irq = device.interrupt_line;
    if irq != 0 && irq < 16 {
        idt::register_irq(irq, on_irq);
    }
    Some(VirtioNet {
        transport,
        rx,
        tx,
        rx_buffers,
        tx_buffers,
        tx_free: (0..tx_count as u16).collect(),
        mac,
        status: features & F_STATUS != 0,
        irq,
    })
}
