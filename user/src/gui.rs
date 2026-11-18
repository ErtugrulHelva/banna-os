//! Masaüstü pencereleri: piksel tamponu ve olaylar.

use crate::sys::{self, Event};
use crate::{Vec, vec};

pub use crate::sys::{
    EVENT_CLOSE, EVENT_CONTEXT, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP,
    EVENT_NONE, EVENT_RESIZE, EVENT_WHEEL, KEY_CTRL, KEY_DELETE, KEY_DOWN, KEY_END, KEY_F1,
    KEY_HOME, KEY_INSERT, KEY_LEFT, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_PASTE, KEY_RIGHT, KEY_SHIFT,
    KEY_UP,
};

pub struct Window {
    id: u64,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    /// Compositor'daki son kare; `present` yalnızca farklı olan dikdörtgeni gönderir.
    shown: Vec<u32>,
}

/// 8 piksellik bloklar hâlinde karşılaştırır (SSE2 ile vektörleşir); ilk farklı bloğun başı.
fn first_difference(a: &[u32], b: &[u32]) -> Option<usize> {
    let mut i = 0;
    while i + 8 <= a.len() {
        let mut diff = 0;
        for k in 0..8 {
            diff |= a[i + k] ^ b[i + k];
        }
        if diff != 0 {
            return (i..i + 8).find(|&k| a[k] != b[k]);
        }
        i += 8;
    }
    (i..a.len()).find(|&k| a[k] != b[k])
}

fn last_difference(a: &[u32], b: &[u32]) -> Option<usize> {
    let mut end = a.len();
    while end >= 8 {
        let mut diff = 0;
        for k in end - 8..end {
            diff |= a[k] ^ b[k];
        }
        if diff != 0 {
            return (end - 8..end).rev().find(|&k| a[k] != b[k]);
        }
        end -= 8;
    }
    (0..end).rev().find(|&k| a[k] != b[k])
}

impl Window {
    pub fn new(title: &str, width: usize, height: usize) -> Result<Self, i64> {
        let id = sys::call(
            sys::WIN_CREATE,
            &[
                width as u64,
                height as u64,
                title.as_ptr() as u64,
                title.len() as u64,
            ],
        );
        if id < 0 {
            return Err(id);
        }
        Ok(Self {
            id: id as u64,
            width,
            height,
            pixels: vec![0; width * height],
            shown: Vec::new(),
        })
    }

    /// `io::poll` için pencere kimliği.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Piksel tamponunu pencereye kopyalar; compositor sonraki karede gösterir. Önceki
    /// kareden yalnızca değişen dikdörtgen gönderilir, hiç değişmediyse çağrı yapılmaz.
    pub fn present(&mut self) -> i64 {
        let (w, h) = (self.width, self.height);
        if self.shown.len() != w * h || w > 0xffff || h > 0xffff {
            let r = sys::call(
                sys::WIN_PRESENT,
                &[self.id, self.pixels.as_ptr() as u64, w as u64, h as u64],
            );
            if r >= 0 {
                self.shown.clone_from(&self.pixels);
            }
            return r;
        }
        let (mut top, mut bottom, mut left, mut right) = (h, 0, w, 0);
        for y in 0..h {
            let row = y * w..(y + 1) * w;
            let (a, b) = (&self.pixels[row.clone()], &self.shown[row]);
            let Some(first) = first_difference(a, b) else {
                continue;
            };
            let last = last_difference(a, b).unwrap_or(first);
            top = top.min(y);
            bottom = y + 1;
            left = left.min(first);
            right = right.max(last + 1);
        }
        if bottom == 0 {
            return 0;
        }
        let rect = (left as u64) << 48
            | (top as u64) << 32
            | ((right - left) as u64) << 16
            | (bottom - top) as u64;
        let r = sys::call(
            sys::WIN_PRESENT_RECT,
            &[
                self.id,
                self.pixels.as_ptr() as u64,
                (w as u64) << 32 | h as u64,
                rect,
            ],
        );
        if r >= 0 {
            for y in top..bottom {
                let row = y * w + left..y * w + right;
                self.shown[row.clone()].copy_from_slice(&self.pixels[row]);
            }
        }
        r
    }

    /// Olay bekler; `timeout_ms` = 0 beklemeden döner.
    pub fn event(&self, timeout_ms: u64) -> Option<Event> {
        let mut event = Event::default();
        let r = sys::call(
            sys::WIN_EVENT,
            &[self.id, &mut event as *mut _ as u64, timeout_ms],
        );
        (r > 0).then_some(event)
    }

    /// Piksel tamponuna masaüstüyle aynı tuvalle çizim.
    pub fn canvas(&mut self) -> crate::gfx::Canvas<'_> {
        let (w, h) = (self.width as i32, self.height as i32);
        // Tuval yalnızca bu pencerenin tamponuna, ödünç süresince yazar.
        unsafe {
            crate::gfx::Canvas::from_raw(
                self.pixels.as_mut_ptr(),
                w,
                h,
                crate::gfx::Rect::new(0, 0, w, h),
            )
        }
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        for row in y.min(self.height)..(y + h).min(self.height) {
            let start = row * self.width;
            let line = &mut self.pixels[start + x.min(self.width)..start + (x + w).min(self.width)];
            line.fill(color);
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        sys::call(sys::WIN_CLOSE, &[self.id]);
    }
}

impl Window {
    /// `EVENT_RESIZE` geldiğinde tamponu yeni boyuta getirir; eski içerik sol üstte kalır.
    /// Önceki boyutu döndürür.
    pub fn resize(&mut self, width: usize, height: usize) -> (usize, usize) {
        let (old_w, old_h) = (self.width, self.height);
        let (w, h) = (width.max(1), height.max(1));
        let mut pixels = vec![0; w * h];
        for y in 0..h.min(old_h) {
            let n = w.min(old_w);
            pixels[y * w..y * w + n].copy_from_slice(&self.pixels[y * old_w..y * old_w + n]);
        }
        self.width = w;
        self.height = h;
        self.pixels = pixels;
        (old_w, old_h)
    }
}

/// Ortak pano.
pub mod clipboard {
    use crate::{String, sys};

    pub fn set(text: &str) {
        sys::call(
            sys::CLIPBOARD,
            &[1, text.as_ptr() as u64, text.len() as u64],
        );
    }

    pub fn get() -> String {
        let mut buf = crate::vec![0u8; 4096];
        loop {
            let n = sys::call(
                sys::CLIPBOARD,
                &[0, buf.as_mut_ptr() as u64, buf.len() as u64],
            );
            if n < 0 {
                return String::new();
            }
            if n as usize <= buf.len() {
                buf.truncate(n as usize);
                return String::from_utf8_lossy(&buf).into_owned();
            }
            buf.resize(n as usize, 0);
        }
    }
}
