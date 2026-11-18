//! Ekran kartları: firmware framebuffer'ı (GOP/VBE), Bochs/QEMU VGA ve virtio-gpu.

pub mod bga;
pub mod virtio_gpu;
pub mod vsync;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::gfx::display;
use crate::kernel::initcall::Detail;
use crate::sync::SpinLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Adapter {
    Firmware,
    Bga,
    VirtioGpu,
}

static ADAPTER: SpinLock<Adapter> = SpinLock::new(Adapter::Firmware);

pub fn adapter() -> Adapter {
    *ADAPTER.lock()
}

/// Yükleyicinin komut satırında bildirdiği UEFI GOP kipleri (`gop=1920x1080,...`).
pub fn firmware_modes() -> Vec<(u32, u32)> {
    crate::boot::cmdline()
        .split_whitespace()
        .find_map(|w| w.strip_prefix("gop="))
        .map(|list| list.split(',').filter_map(parse_mode).collect())
        .unwrap_or_default()
}

/// Monitörün EDID'deki doğal çözünürlüğü (`native=WxH`).
pub fn native_mode() -> Option<(u32, u32)> {
    crate::boot::cmdline()
        .split_whitespace()
        .find_map(|w| w.strip_prefix("native="))
        .and_then(parse_mode)
}

fn parse_mode(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// UEFI kipleri yalnızca önyüklemede kurulabilir: seçim ESP'ye yazılır, yükleyici
/// sonraki açılışta uygular.
pub const RESTART_REQUIRED: &str = "applies after restart";
const DISPLAY_CONF: &str = "/boot/boot/display.conf";

pub fn describe() -> String {
    let (w, h) = display::size().unwrap_or((0, 0));
    match adapter() {
        Adapter::Firmware if !firmware_modes().is_empty() => {
            format!("UEFI GOP {}x{} ({} kip)", w, h, firmware_modes().len())
        }
        Adapter::Firmware => format!("firmware framebuffer {}x{} (GOP/VBE, fixed mode)", w, h),
        Adapter::Bga => format!("Bochs/QEMU VGA {}x{} (mode setting)", w, h),
        Adapter::VirtioGpu => format!("virtio-gpu {}x{} (2D, hardware cursor)", w, h),
    }
}

pub fn modes() -> Vec<(u32, u32)> {
    match adapter() {
        Adapter::Firmware => {
            let modes = firmware_modes();
            if modes.is_empty() {
                display::size()
                    .map(|(w, h)| alloc::vec![(w as u32, h as u32)])
                    .unwrap_or_default()
            } else {
                modes
            }
        }
        Adapter::Bga | Adapter::VirtioGpu => bga::MODES.to_vec(),
    }
}

/// Çözünürlüğü değiştirir; masaüstü açıksa önce kapatılmalıdır.
pub fn set_mode(width: u32, height: u32) -> Result<(), &'static str> {
    // UEFI kipi yalnızca kaydedilir; çalışan ekran değişmez.
    if crate::gui::is_running() && adapter() != Adapter::Firmware {
        return Err("close the desktop first");
    }
    set_mode_live(width, height)
}

/// Masaüstü çalışırken compositor'ın kendisi çağırır (ekran ondan sonra yeniden kurulur).
pub fn set_mode_live(width: u32, height: u32) -> Result<(), &'static str> {
    if !(320..=4096).contains(&width) || !(200..=2160).contains(&height) {
        return Err("unsupported resolution");
    }
    match adapter() {
        Adapter::Firmware => {
            if !firmware_modes().contains(&(width, height)) {
                return Err("the firmware framebuffer cannot change modes");
            }
            crate::fs::write_all(DISPLAY_CONF, format!("{}x{}\n", width, height).as_bytes())
                .map_err(|_| "cannot write /boot/boot/display.conf")?;
            crate::log!("gpu: {}x{} saved, applies after restart", width, height);
            Err(RESTART_REQUIRED)
        }
        Adapter::Bga => bga::set_mode(&bga::detect().ok_or("adapter vanished")?, width, height),
        Adapter::VirtioGpu => virtio_gpu::set_mode(width, height),
    }
}

/// Kart çözünürlük ayarına izin veriyorsa açılışta kurulan kip.
pub const DEFAULT_MODE: (u32, u32) = (2560, 1440);
/// Yazılımsal öykünmede (UTM/QEMU TCG) piksel başına maliyet yüksek: %44 daha az piksel.
pub const EMULATED_MODE: (u32, u32) = (1920, 1080);

/// Açılış kipi; Ayarlar'da kaydedilmiş çözünürlük masaüstü başlarken uygulanır.
pub fn default_mode() -> (u32, u32) {
    if crate::arch::cpuinfo::emulated() {
        EMULATED_MODE
    } else {
        DEFAULT_MODE
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    if !display::is_ready() {
        let _ = write!(detail, "no display");
        return Ok(());
    }
    let current = display::size().map(|(w, h)| (w as u32, h as u32));
    let mode = default_mode();
    if virtio_gpu::detect() {
        match virtio_gpu::init(Some(mode)).or_else(|_| virtio_gpu::init(current)) {
            Ok(_) => *ADAPTER.lock() = Adapter::VirtioGpu,
            Err(e) => crate::log!("gpu: virtio-gpu init failed: {}", e),
        }
    } else if let Some(device) = bga::detect() {
        *ADAPTER.lock() = Adapter::Bga;
        if current != Some(mode)
            && let Err(e) = bga::set_mode(&device, mode.0, mode.1)
        {
            crate::log!("gpu: cannot switch to {}x{}: {}", mode.0, mode.1, e);
        }
    }
    if adapter() == Adapter::Firmware
        && let Some((_, h)) = display::size()
    {
        vsync::init(h as u32, crate::arch::cpuinfo::emulated());
    }
    crate::log!("gpu: {}", describe());
    let _ = write!(detail, "{}", describe());
    Ok(())
}

crate::initcall!(driver, 62, "GPU", init);
