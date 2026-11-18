//! Dikey boşluk (vblank) beklemesi: kare kopyalaması ekranın taranmadığı aralıkta
//! başlar ve üstten aşağı taramanın önünde ilerlediği için yırtılma olmaz.
//!
//! Kaynaklar, açılışta ölçülüp doğrulanarak seçilir:
//! - Intel tümleşik grafik: etkin pipe'ın tarama satırı sayacı (PIPE_DSL),
//! - eski VGA giriş durum yazmacı (0x3DA, bit 3 = dikey geri dönüş).
//!
//! İkisi de tutarlı bir yenileme hızı göstermezse vsync kapalı kalır.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::arch::cpu::inb;
use crate::arch::tsc;
use crate::drivers::pci;
use crate::memory::paging::{NO_CACHE, map_physical};

const SOURCE_NONE: usize = 0;
const SOURCE_INTEL: usize = 1;
const SOURCE_VGA: usize = 2;

static SOURCE: AtomicUsize = AtomicUsize::new(SOURCE_NONE);
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Intel: PIPE_DSL yazmacının sanal adresi ve görünür satır sayısı.
static DSL: AtomicU64 = AtomicU64::new(0);
static ACTIVE_LINES: AtomicU32 = AtomicU32::new(0);
/// Ölçülen yenileme süresi (µs).
static PERIOD_US: AtomicU32 = AtomicU32::new(0);
static TIMEOUTS: AtomicU32 = AtomicU32::new(0);

const VGA_STATUS: u16 = 0x3da;
const VGA_RETRACE: u8 = 1 << 3;

fn read_dsl() -> u32 {
    let at = DSL.load(Ordering::Relaxed) as *const u32;
    unsafe { core::ptr::read_volatile(at) & 0x1fff }
}

fn in_vblank() -> bool {
    match SOURCE.load(Ordering::Relaxed) {
        SOURCE_INTEL => read_dsl() >= ACTIVE_LINES.load(Ordering::Relaxed),
        SOURCE_VGA => (unsafe { inb(VGA_STATUS) } & VGA_RETRACE) != 0,
        _ => true,
    }
}

/// Bir sonraki dikey boşluğa kadar bekler (en çok ~1,5 kare). Zaten boşluktaysa hemen döner.
pub fn wait() {
    if !active() {
        return;
    }
    let period = PERIOD_US.load(Ordering::Relaxed).clamp(4_000, 40_000) as u64;
    let deadline = tsc::now_ns() + period * 1_500;
    while !in_vblank() {
        if tsc::now_ns() >= deadline {
            // Sayaç durduysa (ör. panelin kendi kendine yenilemesi) her kare beklemek
            // takılmaya döner: art arda zaman aşımında vsync kapatılır.
            if TIMEOUTS.fetch_add(1, Ordering::Relaxed) + 1 >= 8 {
                SOURCE.store(SOURCE_NONE, Ordering::Relaxed);
                crate::log!("vsync: no vertical blank for 8 frames, disabled");
            }
            return;
        }
        core::hint::spin_loop();
    }
    TIMEOUTS.store(0, Ordering::Relaxed);
}

pub fn active() -> bool {
    SOURCE.load(Ordering::Relaxed) != SOURCE_NONE && ENABLED.load(Ordering::Relaxed)
}

pub fn available() -> bool {
    SOURCE.load(Ordering::Relaxed) != SOURCE_NONE
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Yenileme hızı (Hz); kaynak yoksa 0.
pub fn refresh_hz() -> u32 {
    match PERIOD_US.load(Ordering::Relaxed) {
        0 => 0,
        p => (1_000_000 + p / 2) / p,
    }
}

pub fn describe() -> &'static str {
    match SOURCE.load(Ordering::Relaxed) {
        SOURCE_INTEL => "Intel tarama satırı",
        SOURCE_VGA => "VGA geri dönüş biti",
        _ => "yok",
    }
}

/// `sample` 100 ms boyunca "boşlukta mı" sorusuna yanıt verir; boşluk başlangıçları
/// düzenli (30-240 Hz) ve boşluk karenin küçük bir kısmıysa yenileme süresini döndürür.
fn measure(sample: impl Fn() -> bool) -> Option<u32> {
    let start = tsc::now_ns();
    let end = start + 100_000_000;
    let mut last = sample();
    let mut edges: [u64; 32] = [0; 32];
    let mut count = 0;
    let (mut reads, mut high) = (0u64, 0u64);
    while tsc::now_ns() < end && count < edges.len() {
        let now = sample();
        reads += 1;
        high += now as u64;
        if now && !last {
            edges[count] = tsc::now_ns();
            count += 1;
        }
        last = now;
    }
    if count < 4 || reads == 0 || high * 3 > reads {
        return None;
    }
    let periods: alloc::vec::Vec<u64> = edges[..count].windows(2).map(|w| w[1] - w[0]).collect();
    let min = *periods.iter().min()?;
    let max = *periods.iter().max()?;
    let average = periods.iter().sum::<u64>() / periods.len() as u64;
    let plausible = (4_000_000..=34_000_000).contains(&average);
    // Kenarlar arası süre kareden kareye en çok %10 oynayabilir.
    (plausible && max - min <= average / 10).then_some((average / 1000) as u32)
}

/// Intel iGPU: etkin pipe'ı bulur (PIPECONF bit 31) ve tarama satırı sayacını doğrular.
fn probe_intel(height: u32) -> Option<u32> {
    let dev = pci::devices()
        .into_iter()
        .find(|d| d.vendor_id == 0x8086 && d.class == 0x03 && d.bus == 0)?;
    // Atom (VLV/CHV) ekran yazmaçları farklı adreste: desteklenmez.
    let base = dev.bar(0).filter(|&b| b != 0)?;
    let regs = map_physical(base, 0x80000, NO_CACHE).ok()?;
    let r = |offset: usize| unsafe { core::ptr::read_volatile(regs.add(offset) as *const u32) };
    for pipe in 0..3usize {
        let conf = r(0x70008 + pipe * 0x1000);
        if conf == u32::MAX || conf & (1 << 31) == 0 {
            continue;
        }
        DSL.store(
            regs as u64 + 0x70000 + pipe as u64 * 0x1000,
            Ordering::Relaxed,
        );
        // Görünür satır: TRANS_VTOTAL (pipe A ve eDP için ayrı) ya da çerçeve yüksekliği.
        let vtotal = [0x6000c + pipe * 0x1000, 0x6f00c]
            .iter()
            .map(|&o| r(o))
            .find(|&v| v != 0 && v != u32::MAX && (v & 0x1fff) + 1 >= 480)
            .map(|v| (v & 0x1fff) + 1);
        ACTIVE_LINES.store(vtotal.unwrap_or(height), Ordering::Relaxed);
        let active = ACTIVE_LINES.load(Ordering::Relaxed);
        if let Some(period) = measure(|| read_dsl() >= active) {
            crate::log!(
                "vsync: Intel {:04x} pipe {}, {} active lines, {} Hz",
                dev.device_id,
                (b'A' + pipe as u8) as char,
                active,
                (1_000_000 + period / 2) / period
            );
            return Some(period);
        }
        crate::log!("vsync: Intel pipe {} scanline counter not usable", pipe);
    }
    None
}

/// Ekran kurulduktan sonra bir kez çağrılır.
pub fn init(height: u32, emulated: bool) {
    // Öykünmede (QEMU/UTM) görüntü ana makinede birleştirilir: vsync anlamsız ve VGA
    // durum biti her okumada değişebilir.
    if emulated {
        return;
    }
    if let Some(period) = probe_intel(height) {
        PERIOD_US.store(period, Ordering::Relaxed);
        SOURCE.store(SOURCE_INTEL, Ordering::Relaxed);
        return;
    }
    if let Some(period) = measure(|| (unsafe { inb(VGA_STATUS) } & VGA_RETRACE) != 0) {
        PERIOD_US.store(period, Ordering::Relaxed);
        SOURCE.store(SOURCE_VGA, Ordering::Relaxed);
        crate::log!("vsync: VGA retrace bit, {} Hz", refresh_hz());
        return;
    }
    crate::log!("vsync: no vertical blank source, presenting without vsync");
}
