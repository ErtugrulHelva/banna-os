//! Ses: HD Audio çıkışı, yazılım mikseri, bip ve `/dev/dsp`.
//!
//! Örnekler 48 kHz, 16 bit, stereo (s16le, sol-sağ dönüşümlü). Mikser thread'i DMA
//! tamponunu donanımın çalma konumunun ~40 ms önünde tutar; kuyruk boşsa sessizlik
//! yazar. HDA yoksa bip, PC hoparlöründen (PIT kanal 2) çalınır.

pub mod hda;

use alloc::collections::VecDeque;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::cpu;
use crate::kernel::initcall::Detail;
use crate::sched::{Mutex, WaitQueue};
use crate::sync::SpinLock;

pub const RATE: u32 = 48_000;
/// Mikserin donanımın önünde tuttuğu ses (bayt): 40 ms.
const LEAD_BYTES: usize = (RATE as usize * 4) / 25;
/// Kuyrukta bekleyebilecek en fazla örnek (1 sn stereo).
const QUEUE_LIMIT: usize = RATE as usize * 2;

static DEVICE: Mutex<Option<hda::Hda>> = Mutex::new(None);
static QUEUE: SpinLock<VecDeque<i16>> = SpinLock::new(VecDeque::new());
static SPACE: WaitQueue = WaitQueue::new();
static VOLUME: AtomicU32 = AtomicU32::new(80);
static PRESENT: AtomicBool = AtomicBool::new(false);
/// USB ses aygıtı takılı: kuyruğu o çeker, HD Audio sessizlik çalar.
static USB_OUTPUT: AtomicBool = AtomicBool::new(false);

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed) || USB_OUTPUT.load(Ordering::Relaxed)
}

pub fn set_usb_output(on: bool) {
    USB_OUTPUT.store(on, Ordering::Relaxed);
    if !on {
        QUEUE.lock().clear();
    }
    SPACE.wake_all();
}

/// USB isochronous tamponunu kuyruktan doldurur (s16le stereo); kuyruk boşsa sessizlik.
pub fn fill_usb(buf: &mut [u8]) {
    let gain = VOLUME.load(Ordering::Relaxed) as i32;
    let mut queue = QUEUE.lock();
    for pair in buf.as_chunks_mut::<2>().0 {
        let sample = queue
            .pop_front()
            .map_or(0, |s| (s as i32 * gain / 100) as i16);
        pair.copy_from_slice(&sample.to_le_bytes());
    }
    let room = queue.len() < QUEUE_LIMIT;
    drop(queue);
    if room {
        SPACE.wake_all();
    }
}

pub fn volume() -> u32 {
    VOLUME.load(Ordering::Relaxed)
}

pub fn set_volume(percent: u32) {
    let percent = percent.min(100);
    VOLUME.store(percent, Ordering::Relaxed);
    if let Some(dev) = DEVICE.lock().as_mut() {
        dev.set_volume(percent);
    }
}

/// s16le stereo örnekleri kuyruğa ekler; yer açılana kadar bekler. Yazılan bayt sayısı.
pub fn write(bytes: &[u8]) -> usize {
    if !present() {
        return bytes.len();
    }
    let samples = bytes.len() / 2;
    let mut done = 0;
    while done < samples {
        SPACE.wait_until(|| QUEUE.lock().len() < QUEUE_LIMIT || crate::kernel::pipe::interrupted());
        if crate::kernel::pipe::interrupted() {
            break;
        }
        let mut queue = QUEUE.lock();
        let room = QUEUE_LIMIT.saturating_sub(queue.len());
        let take = room.min(samples - done);
        for i in done..done + take {
            queue.push_back(i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]));
        }
        done += take;
    }
    done * 2
}

/// Kuyruktaki sesin bitmesini bekler.
pub fn drain() {
    while !QUEUE.lock().is_empty() && present() {
        crate::sched::sleep_ms(5);
    }
}

/// Kare dalga bip (HD Audio varsa onunla, yoksa PC hoparlöründen); bitene kadar bekler.
pub fn beep(frequency: u32, ms: u32) {
    let frequency = frequency.clamp(20, 20_000);
    if !present() {
        pc_speaker(frequency, ms);
        return;
    }
    let total = RATE * ms / 1000;
    let period = RATE / frequency;
    let mut pcm = alloc::vec::Vec::with_capacity(total as usize * 4);
    for i in 0..total {
        // Başta ve sonda kısa yumuşatma (tık sesi olmasın).
        let edge = (i.min(total - i) as i32 * 16).min(8000);
        let value: i16 = if (i % period) < period / 2 {
            edge as i16
        } else {
            -(edge as i16)
        };
        pcm.extend_from_slice(&value.to_le_bytes());
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    write(&pcm);
    drain();
}

fn pc_speaker(frequency: u32, ms: u32) {
    let divisor = (1_193_182 / frequency) as u16;
    unsafe {
        cpu::outb(0x43, 0xb6);
        cpu::outb(0x42, divisor as u8);
        cpu::outb(0x42, (divisor >> 8) as u8);
        let gate = cpu::inb(0x61);
        cpu::outb(0x61, gate | 3);
        crate::sched::sleep_ms(ms as u64);
        cpu::outb(0x61, gate & !3);
    }
}

/// Mikser: tamponu çalma konumunun önünde tutar, sesi seviye ile ölçekler.
fn mixer() -> i64 {
    let mut written = 0usize;
    let mut last = 0usize;
    let mut played_total = 0usize;
    loop {
        crate::sched::sleep_ms(5);
        let mut guard = DEVICE.lock();
        let Some(dev) = guard.as_mut() else { return 0 };
        let position = dev.position();
        played_total += (position + hda::BUFFER_BYTES - last) % hda::BUFFER_BYTES;
        last = position;
        if written < played_total {
            // Gecikme oldu (yetersiz besleme): yazma noktası çalma konumuna çekilir.
            written = played_total;
        }
        let gain = VOLUME.load(Ordering::Relaxed) as i32;
        let base = dev.buffer.virt as *mut i16;
        let usb = USB_OUTPUT.load(Ordering::Relaxed);
        let mut queue = QUEUE.lock();
        while written < played_total + LEAD_BYTES {
            let offset = written % hda::BUFFER_BYTES;
            let sample = if usb {
                0
            } else {
                queue
                    .pop_front()
                    .map_or(0, |s| (s as i32 * gain / 100) as i16)
            };
            unsafe { base.add(offset / 2).write_volatile(sample) };
            written += 2;
        }
        let empty = queue.len() < QUEUE_LIMIT;
        drop(queue);
        drop(guard);
        if empty {
            SPACE.wake_all();
        }
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    if crate::boot::has_flag("noaudio") {
        let _ = write!(detail, "disabled (noaudio), PC speaker only");
        return Ok(());
    }
    let Some(mut dev) = hda::probe() else {
        let _ = write!(detail, "no HD Audio, PC speaker only");
        return Ok(());
    };
    dev.set_volume(volume());
    let outputs = dev.outputs.join(", ");
    *DEVICE.lock() = Some(dev);
    PRESENT.store(true, Ordering::Relaxed);
    crate::sched::spawn_detached("mixer", mixer).ok_or("could not start the mixer")?;
    let _ = write!(detail, "HD Audio, {}", outputs);
    Ok(())
}

crate::initcall!(driver, 55, "Audio (HD Audio)", init);
