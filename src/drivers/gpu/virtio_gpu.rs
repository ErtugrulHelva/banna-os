//! virtio-gpu (2B): kendi tarama kaynağı, açık flush ve donanım imleci.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use crate::drivers::framebuffer::Framebuffer;
use crate::drivers::pci;
use crate::drivers::virtio::{Queue, Transport};
use crate::gfx::{Rect, cursor, display};
use crate::memory::paging::{self, NO_EXECUTE, WRITABLE};
use crate::memory::{PAGE_SIZE, align_up, phys_to_virt, pmm};
use crate::sched::Mutex;

const GET_DISPLAY_INFO: u32 = 0x0100;
const RESOURCE_CREATE_2D: u32 = 0x0101;
const RESOURCE_UNREF: u32 = 0x0102;
const SET_SCANOUT: u32 = 0x0103;
const RESOURCE_FLUSH: u32 = 0x0104;
const TRANSFER_TO_HOST_2D: u32 = 0x0105;
const RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESOURCE_DETACH_BACKING: u32 = 0x0107;
const UPDATE_CURSOR: u32 = 0x0300;
const MOVE_CURSOR: u32 = 0x0301;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

const FORMAT_B8G8R8A8: u32 = 1;
const FORMAT_B8G8R8X8: u32 = 2;
const SCANOUT_RESOURCE_BASE: u32 = 1;
const CURSOR_RESOURCE: u32 = 100;
const BACKING_WINDOW: u64 = 0xffff_e800_0000_0000;

struct Gpu {
    transport: Transport,
    control: Queue,
    cursor: Queue,
    request: u64,
    response: u64,
    resource: u32,
    width: u32,
    height: u32,
    backing: u64,
    backing_pages: u64,
}

static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

impl Gpu {
    fn write_words(&self, words: &[u32]) -> u32 {
        let ptr = phys_to_virt(self.request) as *mut u32;
        for (i, &w) in words.iter().enumerate() {
            unsafe { write_volatile(ptr.add(i), w) };
        }
        (words.len() * 4) as u32
    }

    fn command(&mut self, words: &[u32], response_len: u32) -> Result<u32, &'static str> {
        let len = self.write_words(words);
        unsafe { write_volatile(phys_to_virt(self.response) as *mut u32, 0) };
        self.control
            .submit((self.request, len), Some((self.response, response_len)))?;
        Ok(unsafe { read_volatile(phys_to_virt(self.response) as *const u32) })
    }

    fn command_ok(&mut self, words: &[u32]) -> Result<(), &'static str> {
        match self.command(words, 24)? {
            RESP_OK_NODATA => Ok(()),
            _ => Err("virtio-gpu command failed"),
        }
    }

    fn header(kind: u32) -> [u32; 6] {
        [kind, 0, 0, 0, 0, 0]
    }

    fn display_size(&mut self) -> Result<(u32, u32), &'static str> {
        let header = Self::header(GET_DISPLAY_INFO);
        if self.command(&header, 24 + 16 * 24)? != RESP_OK_DISPLAY_INFO {
            return Err("GET_DISPLAY_INFO failed");
        }
        let resp = phys_to_virt(self.response) as *const u32;
        let w = unsafe { read_volatile(resp.add(6 + 2)) };
        let h = unsafe { read_volatile(resp.add(6 + 3)) };
        Ok((w, h))
    }

    /// Yeni boyutta tarama kaynağı oluşturur, belleği bağlar ve ekrana atar.
    fn create_scanout(&mut self, width: u32, height: u32) -> Result<*mut u8, &'static str> {
        let bytes = width as u64 * height as u64 * 4;
        let pages = align_up(bytes, PAGE_SIZE) / PAGE_SIZE;
        let backing = pmm::alloc_contiguous(pages).ok_or("no contiguous memory for the scanout")?;
        let root = paging::kernel_root();
        for page in 0..pages.max(self.backing_pages) {
            paging::unmap_page_in(root, BACKING_WINDOW + page * PAGE_SIZE);
        }
        for page in 0..pages {
            paging::map_page_in(
                root,
                BACKING_WINDOW + page * PAGE_SIZE,
                backing + page * PAGE_SIZE,
                WRITABLE | NO_EXECUTE,
            )
            .map_err(|_| "cannot map the scanout")?;
        }
        if self.backing_pages > 0 {
            crate::arch::smp::flush_tlb_others();
        }
        unsafe { core::ptr::write_bytes(BACKING_WINDOW as *mut u8, 0, bytes as usize) };

        let old = (self.resource, self.backing, self.backing_pages);
        let resource = SCANOUT_RESOURCE_BASE + (self.resource % 2);
        let mut words = Vec::from(Self::header(RESOURCE_CREATE_2D));
        words.extend_from_slice(&[resource, FORMAT_B8G8R8X8, width, height]);
        self.command_ok(&words)?;
        let mut words = Vec::from(Self::header(RESOURCE_ATTACH_BACKING));
        words.extend_from_slice(&[
            resource,
            1,
            backing as u32,
            (backing >> 32) as u32,
            bytes as u32,
            0,
        ]);
        self.command_ok(&words)?;
        let mut words = Vec::from(Self::header(SET_SCANOUT));
        words.extend_from_slice(&[0, 0, width, height, 0, resource]);
        self.command_ok(&words)?;

        if old.0 != 0 {
            let mut words = Vec::from(Self::header(RESOURCE_DETACH_BACKING));
            words.extend_from_slice(&[old.0, 0]);
            let _ = self.command_ok(&words);
            let mut words = Vec::from(Self::header(RESOURCE_UNREF));
            words.extend_from_slice(&[old.0, 0]);
            let _ = self.command_ok(&words);
            for i in 0..old.2 {
                pmm::free_frame(old.1 + i * PAGE_SIZE);
            }
        }
        self.resource = resource;
        self.width = width;
        self.height = height;
        self.backing = backing;
        self.backing_pages = pages;
        Ok(BACKING_WINDOW as *mut u8)
    }

    fn flush(&mut self, r: Rect) -> Result<(), &'static str> {
        let (x, y, w, h) = (r.x as u32, r.y as u32, r.w as u32, r.h as u32);
        let offset = (y as u64 * self.width as u64 + x as u64) * 4;
        let mut words = Vec::from(Self::header(TRANSFER_TO_HOST_2D));
        words.extend_from_slice(&[
            x,
            y,
            w,
            h,
            offset as u32,
            (offset >> 32) as u32,
            self.resource,
            0,
        ]);
        self.command_ok(&words)?;
        let mut words = Vec::from(Self::header(RESOURCE_FLUSH));
        words.extend_from_slice(&[x, y, w, h, self.resource, 0]);
        self.command_ok(&words)
    }

    fn cursor_command(
        &mut self,
        kind: u32,
        x: i32,
        y: i32,
        resource: u32,
    ) -> Result<(), &'static str> {
        let mut words = Vec::from(Self::header(kind));
        words.extend_from_slice(&[0, x as u32, y as u32, 0, resource, 0, 0, 0]);
        let len = self.write_words(&words);
        self.cursor.submit((self.request, len), None)
    }

    fn setup_cursor(&mut self) -> Result<(), &'static str> {
        let image = cursor::argb_image();
        let page = pmm::alloc_contiguous(4).ok_or("no memory for the cursor")?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                image.as_ptr(),
                phys_to_virt(page) as *mut u32,
                image.len(),
            );
        }
        let mut words = Vec::from(Self::header(RESOURCE_CREATE_2D));
        words.extend_from_slice(&[CURSOR_RESOURCE, FORMAT_B8G8R8A8, 64, 64]);
        self.command_ok(&words)?;
        let mut words = Vec::from(Self::header(RESOURCE_ATTACH_BACKING));
        words.extend_from_slice(&[
            CURSOR_RESOURCE,
            1,
            page as u32,
            (page >> 32) as u32,
            64 * 64 * 4,
            0,
        ]);
        self.command_ok(&words)?;
        let mut words = Vec::from(Self::header(TRANSFER_TO_HOST_2D));
        words.extend_from_slice(&[0, 0, 64, 64, 0, 0, CURSOR_RESOURCE, 0]);
        self.command_ok(&words)
    }
}

fn flush_hook(rect: Rect) {
    if let Some(gpu) = GPU.lock().as_mut() {
        let _ = gpu.flush(rect);
    }
}

fn cursor_hook(position: Option<(i32, i32)>) {
    if let Some(gpu) = GPU.lock().as_mut() {
        let _ = match position {
            Some((x, y)) => gpu.cursor_command(UPDATE_CURSOR, x, y, CURSOR_RESOURCE),
            None => gpu.cursor_command(UPDATE_CURSOR, 0, 0, 0),
        };
    }
}

static CURSOR_SHOWN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn move_hook(position: Option<(i32, i32)>) {
    use core::sync::atomic::Ordering;
    match position {
        Some((x, y)) if CURSOR_SHOWN.load(Ordering::Relaxed) => {
            if let Some(gpu) = GPU.lock().as_mut() {
                let _ = gpu.cursor_command(MOVE_CURSOR, x, y, CURSOR_RESOURCE);
            }
        }
        Some(_) => {
            CURSOR_SHOWN.store(true, Ordering::Relaxed);
            cursor_hook(position);
        }
        None => {
            CURSOR_SHOWN.store(false, Ordering::Relaxed);
            cursor_hook(None);
        }
    }
}

pub fn detect() -> bool {
    pci::find_id(0x1af4, 0x1050).is_some()
}

/// Kartı başlatır ve ekranı virtio-gpu tarama kaynağına taşır; (genişlik, yükseklik).
pub fn init(preferred: Option<(u32, u32)>) -> Result<(u32, u32), &'static str> {
    let device = pci::find_id(0x1af4, 0x1050).ok_or("no virtio-gpu")?;
    let transport = Transport::new(&device).ok_or("virtio-gpu capabilities missing")?;
    transport.negotiate(0)?;
    let control = transport.setup_queue(0)?;
    let cursor_queue = transport.setup_queue(1)?;
    transport.driver_ok();
    let request = pmm::alloc_dma_frame().ok_or("out of memory")?;
    let response = pmm::alloc_dma_frame().ok_or("out of memory")?;
    let mut gpu = Gpu {
        transport,
        control,
        cursor: cursor_queue,
        request,
        response,
        resource: 0,
        width: 0,
        height: 0,
        backing: 0,
        backing_pages: 0,
    };
    let (w, h) = match preferred {
        Some(size) => size,
        None => gpu
            .display_size()
            .ok()
            .filter(|&(w, h)| w > 0 && h > 0)
            .unwrap_or((1280, 800)),
    };
    let base = gpu.create_scanout(w, h)?;
    let hw_cursor = gpu.setup_cursor().is_ok();
    let _ = &gpu.transport;
    *GPU.lock() = Some(gpu);
    let fb = Framebuffer::xrgb(base, w as usize, h as usize, w as usize * 4);
    display::switch_output(
        fb,
        Some(flush_hook),
        hw_cursor.then_some(move_hook as fn(Option<(i32, i32)>)),
    );
    if hw_cursor {
        cursor_hook(None);
    }
    Ok((w, h))
}

pub fn set_mode(width: u32, height: u32) -> Result<(), &'static str> {
    let base = {
        let mut guard = GPU.lock();
        let gpu = guard.as_mut().ok_or("virtio-gpu is not active")?;
        gpu.create_scanout(width, height)?
    };
    let fb = Framebuffer::xrgb(base, width as usize, height as usize, width as usize * 4);
    display::switch_output(fb, Some(flush_hook), Some(move_hook));
    Ok(())
}
