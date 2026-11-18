//! Duvar kağıdı yüzeyleri: hazır desenler ve düz renkler tüm çekirdeklerde paralel üretilir;
//! BMP/PPM görüntü dosyaları ayrı thread'de okunup ekrana ölçeklenir.

use alloc::string::String;

use super::prefs::Wallpaper;
use super::wallpaper_art::{self as art, PRESETS};
use crate::gfx::Surface;
use crate::sync::SpinLock;

/// Yüzeyi `f(x, y)` ile doldurur; `parallel` ise şeritler tüm çekirdeklere dağılır
/// (arka plan thread'leri compositor'ın işçilerini meşgul etmemek için seri çalışır).
fn generate(
    width: i32,
    height: i32,
    parallel: bool,
    f: &(dyn Fn(i32, i32) -> u32 + Sync),
) -> Surface {
    let mut surface = Surface::new(width, height, 0);
    const BAND: i32 = 32;
    let bands = ((height + BAND - 1) / BAND).max(0) as usize;
    let base = surface.pixels.as_mut_ptr() as usize;
    let len = surface.pixels.len();
    let work = |i: usize| {
        let y0 = i as i32 * BAND;
        let y1 = (y0 + BAND).min(height);
        // Her işçi ayrık satırlara yazar.
        let pixels = unsafe { core::slice::from_raw_parts_mut(base as *mut u32, len) };
        for y in y0..y1 {
            let row = &mut pixels[(y * width) as usize..((y + 1) * width) as usize];
            for (x, p) in row.iter_mut().enumerate() {
                *p = f(x as i32, y);
            }
        }
    };
    if parallel && width * height > 200_000 {
        crate::gfx::parallel::for_each(bands, &work);
    } else {
        (0..bands).for_each(work);
    }
    surface
}

fn dim(surface: &mut Surface, percent: u8) {
    if percent == 0 {
        return;
    }
    let keep = 256 - percent as u32 * 256 / 100;
    for p in surface.pixels.iter_mut() {
        let c = *p;
        *p = ((((c >> 16) & 0xff) * keep) >> 8) << 16
            | ((((c >> 8) & 0xff) * keep) >> 8) << 8
            | (((c & 0xff) * keep) >> 8);
    }
}

/// Hazır desen ya da düz renk; görüntü dosyaları için `None` (bkz. `request_image`).
pub fn render(choice: &Wallpaper, width: i32, height: i32, dim_percent: u8) -> Option<Surface> {
    let mut surface = match choice {
        Wallpaper::Preset(p) => {
            let p = *p % PRESETS.len() as u8;
            generate(width, height, true, &move |x, y| {
                art::pixel(p, x, y, width, height)
            })
        }
        Wallpaper::Solid(i) => {
            let i = *i;
            generate(width, height, true, &move |_, y| {
                art::solid_pixel(i, y, height)
            })
        }
        Wallpaper::Image(_) => return None,
    };
    dim(&mut surface, dim_percent);
    Some(surface)
}

/// Tamamlanan görüntü yüklemesi: (yol, genişlik, yükseklik, sonuç).
type Loaded = (String, i32, i32, Option<Surface>);
static LOADED: SpinLock<Option<Loaded>> = SpinLock::new(None);

/// Görüntü dosyasını arka planda yükler; bitince `take_image` sonucu verir.
pub fn request_image(path: &str, width: i32, height: i32, dim_percent: u8) {
    let path = String::from(path);
    crate::sched::spawn_detached("wallpaper", move || {
        let surface = crate::fs::read_all(&path)
            .ok()
            .and_then(|data| art::decode(&data))
            .map(|image| {
                let sample = art::cover_sampler(&image, width, height);
                let mut s = generate(width, height, false, &sample);
                dim(&mut s, dim_percent);
                s
            });
        if surface.is_none() {
            crate::log!("wallpaper: cannot read {}", path);
        }
        *LOADED.lock() = Some((path, width, height, surface));
        super::wake();
        0
    });
}

pub fn take_image() -> Option<Loaded> {
    LOADED.lock().take()
}
