//! Kullanıcı programlarının pencereleri: oluşturma, piksel aktarımı ve olay kanalı.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{DESKTOP, Kind, Window, wake};
use crate::gfx::Rect;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const EVENT_MOUSE_MOVE: u32 = 1;
pub const EVENT_MOUSE_DOWN: u32 = 2;
pub const EVENT_MOUSE_UP: u32 = 3;
pub const EVENT_KEY: u32 = 4;
pub const EVENT_CLOSE: u32 = 5;
/// Fare tekerleği: `key` = adım (i32, aşağı pozitif), `x`/`y` imleç konumu.
pub const EVENT_WHEEL: u32 = 6;
/// Pencere yeniden boyutlandı: `x` × `y` yeni istemci alanı.
pub const EVENT_RESIZE: u32 = 7;
/// Sağ tık (bağlam menüsü): `x`/`y` istemci alanında.
pub const EVENT_CONTEXT: u32 = 8;
/// Ctrl+Shift+V: panodaki metni yapıştır.
pub const KEY_PASTE: u32 = 0xf710;

/// Karakter olmayan tuşların kodları (Unicode özel kullanım alanı).
pub const KEY_UP: u32 = 0xf700;
pub const KEY_DOWN: u32 = 0xf701;
pub const KEY_LEFT: u32 = 0xf702;
pub const KEY_RIGHT: u32 = 0xf703;
pub const KEY_HOME: u32 = 0xf704;
pub const KEY_END: u32 = 0xf705;
pub const KEY_DELETE: u32 = 0xf706;
pub const KEY_PAGE_UP: u32 = 0xf707;
pub const KEY_PAGE_DOWN: u32 = 0xf708;
pub const KEY_INSERT: u32 = 0xf709;
/// F1..F12 = KEY_F1..KEY_F1 + 11.
pub const KEY_F1: u32 = 0xf721;
/// Shift basılıyken gezinme tuşlarına eklenen bit.
pub const KEY_SHIFT: u32 = 0x1_0000;
/// Ctrl basılıyken gezinme tuşlarına eklenen bit (ör. Ctrl+← kelime başı).
pub const KEY_CTRL: u32 = 0x2_0000;

const ENODEV: i64 = -19;
const EBADF: i64 = -9;
const EINVAL: i64 = -22;
const MAX_EVENTS: usize = 256;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Event {
    pub kind: u32,
    pub x: i32,
    pub y: i32,
    pub button: u32,
    pub key: u32,
}

impl Event {
    pub fn mouse(kind: u32, x: i32, y: i32, button: u32) -> Self {
        Self {
            kind,
            x,
            y,
            button,
            key: 0,
        }
    }

    pub fn key(code: u32) -> Self {
        Self {
            kind: EVENT_KEY,
            key: code,
            ..Self::default()
        }
    }

    pub fn close() -> Self {
        Self {
            kind: EVENT_CLOSE,
            ..Self::default()
        }
    }
}

pub struct Channel {
    pub pid: u64,
    pub id: u64,
    events: SpinLock<VecDeque<Event>>,
    wait: WaitQueue,
    closed: AtomicBool,
}

impl Channel {
    pub fn push(&self, event: Event) {
        {
            let mut events = self.events.lock();
            if event.kind == EVENT_MOUSE_MOVE
                && let Some(last) = events.back_mut()
                && last.kind == EVENT_MOUSE_MOVE
            {
                *last = event;
            } else if events.len() < MAX_EVENTS {
                events.push_back(event);
            }
        }
        self.wait.wake_all();
        crate::kernel::pipe::notify();
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.wait.wake_all();
        crate::kernel::pipe::notify();
    }

    fn pending(&self) -> bool {
        !self.events.lock().is_empty() || self.closed.load(Ordering::Acquire)
    }
}

static CHANNELS: SpinLock<Vec<Arc<Channel>>> = SpinLock::new(Vec::new());

fn channel(pid: u64, id: u64) -> Option<Arc<Channel>> {
    CHANNELS
        .lock()
        .iter()
        .find(|c| c.pid == pid && c.id == id)
        .cloned()
}

pub fn create(pid: u64, width: i32, height: i32, title: &str) -> i64 {
    if !(16..=1920).contains(&width) || !(16..=1200).contains(&height) {
        return EINVAL;
    }
    let mut guard = DESKTOP.lock();
    let Some(desktop) = guard.as_mut() else {
        return ENODEV;
    };
    let mut window = Window::new(0, Kind::App(pid), title, 0, 0, width, height);
    window.surface.pixels.fill(0);
    let id = desktop.add_window(window);
    let channel = Arc::new(Channel {
        pid,
        id,
        events: SpinLock::new(VecDeque::new()),
        wait: WaitQueue::new(),
        closed: AtomicBool::new(false),
    });
    if let Some(window) = desktop.windows.iter_mut().find(|w| w.id == id) {
        window.channel = Some(channel.clone());
    }
    CHANNELS.lock().push(channel);
    drop(guard);
    wake();
    id as i64
}

pub fn present(pid: u64, id: u64, width: i32, height: i32, pixels: Vec<u32>) -> i64 {
    let mut guard = DESKTOP.lock();
    let Some(desktop) = guard.as_mut() else {
        return ENODEV;
    };
    let Some(window) = desktop
        .windows
        .iter_mut()
        .find(|w| w.id == id && w.kind == Kind::App(pid))
    else {
        return EBADF;
    };
    if window.surface.width != width || window.surface.height != height {
        return EINVAL;
    }
    window.surface.pixels = pixels;
    let client = window.client();
    if window.visible {
        desktop.damage.add(client);
    }
    drop(guard);
    wake();
    0
}

pub fn present_rect(pid: u64, id: u64, width: i32, height: i32, region: Rect, data: &[u32]) -> i64 {
    let mut guard = DESKTOP.lock();
    let Some(desktop) = guard.as_mut() else {
        return ENODEV;
    };
    let Some(window) = desktop
        .windows
        .iter_mut()
        .find(|w| w.id == id && w.kind == Kind::App(pid))
    else {
        return EBADF;
    };
    if window.surface.width != width || window.surface.height != height {
        return EINVAL;
    }
    let w = region.w as usize;
    for (i, row) in data.chunks_exact(w).enumerate() {
        let at = (region.y as usize + i) * width as usize + region.x as usize;
        window.surface.pixels[at..at + w].copy_from_slice(row);
    }
    let client = window.client();
    if window.visible {
        desktop.damage.add(Rect::new(
            client.x + region.x,
            client.y + region.y,
            region.w,
            region.h,
        ));
    }
    drop(guard);
    wake();
    0
}

pub fn next_event(pid: u64, id: u64, timeout_ms: u64) -> Result<Option<Event>, i64> {
    let channel = channel(pid, id).ok_or(EBADF)?;
    if timeout_ms > 0 {
        channel.wait.wait_timeout(|| channel.pending(), timeout_ms);
    }
    if let Some(event) = channel.events.lock().pop_front() {
        return Ok(Some(event));
    }
    if channel.closed.load(Ordering::Acquire) {
        return Ok(Some(Event::close()));
    }
    Ok(None)
}

/// `poll` için: pencerede bekleyen olay ya da kapanma var mı (pencere yoksa da hazır).
pub fn has_event(pid: u64, id: u64) -> bool {
    channel(pid, id).is_none_or(|c| c.pending())
}

pub fn close(pid: u64, id: u64) -> i64 {
    let Some(channel) = channel(pid, id) else {
        return EBADF;
    };
    CHANNELS.lock().retain(|c| !Arc::ptr_eq(c, &channel));
    if let Some(desktop) = DESKTOP.lock().as_mut() {
        desktop.remove_window(id);
    }
    wake();
    0
}

/// Ortak pano (metin, en fazla 64 KiB).
static CLIPBOARD: SpinLock<alloc::string::String> = SpinLock::new(alloc::string::String::new());

pub fn clipboard_set(text: &str) {
    let mut end = text.len().min(65536);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    *CLIPBOARD.lock() = alloc::string::String::from(&text[..end]);
}

pub fn clipboard_get() -> alloc::string::String {
    CLIPBOARD.lock().clone()
}

/// İşlem bittiğinde kanallarını temizler (pencereler compositor'da kaldırılır).
pub fn forget_process(pid: u64) {
    CHANNELS.lock().retain(|c| c.pid != pid);
}
