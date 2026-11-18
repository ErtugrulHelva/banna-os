//! Çift tamponlu ekran: RAM'deki arka tampon + write-combining framebuffer.
//!
//! Çizimler arka tampona yapılır ve hasar gören dikdörtgenler `present` ile
//! ekrana kopyalanır; framebuffer belleği hiç okunmaz.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{Damage, Rect, Surface, cursor};
use crate::arch::tsc;
use crate::drivers::console;
use crate::drivers::framebuffer::Framebuffer;
use crate::kernel::initcall::Detail;
use crate::memory::paging::{self, NO_EXECUTE, WRITABLE, WRITE_COMBINING};
use crate::memory::{PAGE_SIZE, align_up};
use crate::sched::Mutex;

const WC_WINDOW: u64 = 0xffff_e000_0000_0000;

/// Donanım imlecini taşıyan/gizleyen GPU kancası.
pub type CursorHook = fn(Option<(i32, i32)>);

pub struct Display {
    fb: Framebuffer,
    pub back: Surface,
    damage: Damage,
    pub presented_frames: u64,
    cursor: Option<(i32, i32)>,
    cursor_dirty: bool,
    /// Ekrana kopyalanan bölgeyi GPU'ya bildirir (virtio-gpu gibi açık flush isteyen kartlar).
    flush: Option<fn(Rect)>,
    /// Donanım imleci varsa yazılımsal imleç bindirilmez.
    hw_cursor: Option<CursorHook>,
}

static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);
static FULL_FRAME_NS: AtomicU64 = AtomicU64::new(0);
static WRITE_COMBINED: AtomicU64 = AtomicU64::new(0);

impl Display {
    pub fn size(&self) -> (i32, i32) {
        (self.back.width, self.back.height)
    }

    pub fn mark(&mut self, rect: Rect) {
        self.damage.add(rect.intersect(&self.back.bounds()));
    }

    pub fn mark_all(&mut self) {
        let bounds = self.back.bounds();
        self.damage.add(bounds);
    }

    /// Yazılımsal imleci taşır: sahne yeniden çizilmez, yalnızca eski konum
    /// arka tampondan geri yüklenir ve imleç yeni konumda ekrana bindirilir.
    pub fn set_cursor(&mut self, position: Option<(i32, i32)>) {
        if self.cursor == position {
            return;
        }
        if let Some(hook) = self.hw_cursor {
            self.cursor = position;
            hook(position);
            return;
        }
        if let Some((x, y)) = self.cursor {
            self.mark(cursor::bounds(x, y));
        }
        self.cursor = position;
        self.cursor_dirty = true;
    }

    pub fn has_work(&self) -> bool {
        !self.damage.is_empty() || self.cursor_dirty
    }

    fn draw_cursor(&mut self, touched: &[Rect]) {
        let Some((x, y)) = self.cursor else { return };
        if self.hw_cursor.is_some() {
            return;
        }
        let bounds = cursor::bounds(x, y).intersect(&self.back.bounds());
        if bounds.is_empty()
            || (!self.cursor_dirty && !touched.iter().any(|r| !r.intersect(&bounds).is_empty()))
        {
            return;
        }
        let mut row = [0u32; cursor::WIDTH as usize];
        for py in bounds.y..bounds.bottom() {
            let width = bounds.w as usize;
            row[..width]
                .copy_from_slice(&self.back.row(py)[bounds.x as usize..bounds.right() as usize]);
            for (i, px) in row[..width].iter_mut().enumerate() {
                if let Some(color) =
                    cursor::pixel((bounds.x + i as i32 - x) as usize, (py - y) as usize)
                {
                    *px = color;
                }
            }
            self.fb
                .write_row(bounds.x as usize, py as usize, &row[..width]);
        }
    }

    /// Hasarlı bölgeleri ekrana kopyalar; kopyalanan piksel sayısını döndürür.
    pub fn present(&mut self) -> u64 {
        let bounds = self.back.bounds();
        let rects: alloc::vec::Vec<Rect> = self
            .damage
            .take()
            .iter()
            .map(|r| r.intersect(&bounds))
            .filter(|r| !r.is_empty())
            .collect();
        let bands = super::split_bands(&rects, 64);
        let pixels: u64 = bands.iter().map(|r| r.area() as u64).sum();
        let back = &self.back;
        let fb = &self.fb;
        let copy_band = |i: usize| {
            let r = bands[i];
            let mut fb = fb.clone();
            for y in r.y..r.bottom() {
                let row = back.row(y);
                fb.write_row(
                    r.x as usize,
                    y as usize,
                    &row[r.x as usize..r.right() as usize],
                );
            }
        };
        super::parallel::for_each(bands.len(), &copy_band);
        let cursor_moved = self.cursor_dirty;
        self.draw_cursor(&rects);
        self.cursor_dirty = false;
        if let Some(flush) = self.flush {
            let mut touched = rects.iter().fold(Rect::default(), |acc, r| acc.union(r));
            if cursor_moved && let Some((x, y)) = self.cursor {
                touched = touched.union(&cursor::bounds(x, y));
            }
            let touched = touched.intersect(&self.back.bounds());
            if !touched.is_empty() {
                flush(touched);
            }
        }
        if pixels > 0 || cursor_moved {
            self.presented_frames += 1;
        }
        pixels
    }
}

pub fn with<R>(f: impl FnOnce(&mut Display) -> R) -> Option<R> {
    DISPLAY.lock().as_mut().map(f)
}

pub fn is_ready() -> bool {
    FULL_FRAME_NS.load(Ordering::Relaxed) != 0
}

/// Tam ekran çizim + kopyalama süresi (ns); 60 FPS bütçesi 16,6 ms'dir.
pub fn full_frame_ns() -> u64 {
    FULL_FRAME_NS.load(Ordering::Relaxed)
}

pub fn write_combining() -> bool {
    WRITE_COMBINED.load(Ordering::Relaxed) != 0
}

static WC_MAPPED: AtomicU64 = AtomicU64::new(0);

/// Fiziksel framebuffer'ı write-combining penceresine eşler (önceki eşlemeyi kaldırır).
pub fn map_write_combining(phys: u64, size: usize) -> Option<*mut u8> {
    if !paging::write_combining_available() {
        return None;
    }
    let root = paging::kernel_root();
    let previous = WC_MAPPED.swap(0, Ordering::Relaxed);
    let mut page = 0;
    while page < previous {
        paging::unmap_page_in(root, WC_WINDOW + page);
        page += PAGE_SIZE;
    }
    let offset = phys & (PAGE_SIZE - 1);
    let phys_base = phys - offset;
    let length = align_up(size as u64 + offset, PAGE_SIZE);
    let mut page = 0;
    while page < length {
        paging::map_page_in(
            root,
            WC_WINDOW + page,
            phys_base + page,
            WRITABLE | NO_EXECUTE | WRITE_COMBINING,
        )
        .ok()?;
        page += PAGE_SIZE;
    }
    WC_MAPPED.store(length, Ordering::Relaxed);
    if previous > 0 {
        crate::arch::smp::flush_tlb_others();
    }
    WRITE_COMBINED.store(1, Ordering::Relaxed);
    Some((WC_WINDOW + offset) as *mut u8)
}

/// Ekran çıkışını yeni bir framebuffer'a (farklı çözünürlük ya da GPU) taşır.
pub fn switch_output(fb: Framebuffer, flush: Option<fn(Rect)>, hw_cursor: Option<CursorHook>) {
    let width = fb.width as i32;
    let height = fb.height as i32;
    {
        let mut guard = DISPLAY.lock();
        let mut display = Display {
            fb: fb.clone(),
            back: Surface::new(width, height, console::BLACK.argb() & 0x00ff_ffff),
            damage: Damage::default(),
            presented_frames: guard.as_ref().map(|d| d.presented_frames).unwrap_or(0),
            cursor: None,
            cursor_dirty: false,
            flush,
            hw_cursor,
        };
        display.mark_all();
        *guard = Some(display);
    }
    console::set_framebuffer(fb);
    console::attach_display();
    super::splash::redraw();
}

pub fn size() -> Option<(i32, i32)> {
    with(|d| d.size())
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let mut fb = console::framebuffer().ok_or("no framebuffer")?;
    if let Some(phys) = paging::translate(fb.base() as u64)
        && let Some(wc) = map_write_combining(phys, fb.size_bytes())
    {
        fb.rebase(wc);
    }

    let width = fb.width as i32;
    let height = fb.height as i32;
    let mut display = Display {
        fb,
        back: Surface::new(width, height, console::BLACK.argb() & 0x00ff_ffff),
        damage: Damage::default(),
        presented_frames: 0,
        cursor: None,
        cursor_dirty: false,
        flush: None,
        hw_cursor: None,
    };

    let workers = super::parallel::start();
    crate::kernel::initcall::self_test("gfx", super::fast::self_test())?;
    let start = tsc::now_ns();
    {
        let mut canvas = display.back.canvas();
        canvas.gradient_v(
            Rect::new(0, 0, width, height),
            console::BLACK.argb(),
            console::BLACK.argb(),
        );
    }
    display.mark_all();
    display.present();
    let elapsed = (tsc::now_ns() - start).max(1);
    FULL_FRAME_NS.store(elapsed, Ordering::Relaxed);

    *DISPLAY.lock() = Some(display);
    console::attach_display();
    super::splash::redraw();

    let fps = 1_000_000_000 / elapsed;
    crate::log!(
        "display: {}x{} back buffer, write-combining {}, full frame {} us (~{} fps), {} render worker(s)",
        width,
        height,
        if write_combining() { "on" } else { "off" },
        elapsed / 1000,
        fps,
        workers
    );
    let _ = write!(
        detail,
        "{}x{}, WC {}, full frame {} us",
        width,
        height,
        if write_combining() { "on" } else { "off" },
        elapsed / 1000
    );
    Ok(())
}

crate::initcall!(driver, 60, "Display (double-buffered)", init);
