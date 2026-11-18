//! Banna OS UEFI önyükleyicisi.
//!
//! EFI bölümündeki `\boot\banna.cfg` ayarlarını okur, menü gösterir (Normal / Kurtarma /
//! Metin konsolu / Önceki sürüm), çekirdeği (`\boot\banna_os`, ELF) yükler ve çekirdeğe Limine önyükleme
//! protokolünün (base revision 3) kullandığı yanıtları sağlar: HHDM, bellek haritası,
//! framebuffer, RSDP, yığın, komut satırı ve çekirdek adresleri. Kök bölüm, yükleyicinin
//! bulunduğu diskteki Linux kök tipi GPT bölümüdür (`root=PARTUUID=`).

#![no_std]
#![no_main]

extern crate alloc;

mod efi;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ffi::c_void;
use core::fmt::Write;
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use efi::*;

const KERNEL_PATH: &str = "\\boot\\banna_os";
/// Sistem güncellemesinden önceki çekirdek (`banna-update` bırakır).
const PREVIOUS_KERNEL_PATH: &str = "\\boot\\banna_os.old";
const CONFIG_PATH: &str = "\\boot\\banna.cfg";
const HHDM: u64 = 0xffff_8000_0000_0000;
const PAGE: u64 = 4096;
const STACK_SIZE: u64 = 256 * 1024;
const TABLE_POOL_PAGES: usize = 1024;
const DATA_POOL_SIZE: usize = 256 * 1024;
const MAX_MEMMAP: usize = 512;
const LOG_PATH: &str = "\\boot\\loader.log";
const DISPLAY_PATH: &str = "\\boot\\display.conf";
const MAX_WIDTH: u32 = 2560;
const MAX_HEIGHT: u32 = 1600;

static SYSTEM: AtomicPtr<SystemTable> = AtomicPtr::new(null_mut());
static EXITED: AtomicBool = AtomicBool::new(false);

fn system() -> &'static SystemTable {
    unsafe { &*SYSTEM.load(Ordering::Relaxed) }
}

fn boot() -> &'static BootServices {
    unsafe { &*system().boot_services }
}

// ---- bellek ayırıcı (yalnızca ExitBootServices öncesi) ----

struct PoolAllocator;

unsafe impl GlobalAlloc for PoolAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if EXITED.load(Ordering::Relaxed) {
            return null_mut();
        }
        let size = layout.size() + layout.align() + 8;
        let mut raw = null_mut();
        if (boot().allocate_pool)(LOADER_DATA, size, &mut raw) != SUCCESS {
            return null_mut();
        }
        let aligned = (raw as usize + 8).next_multiple_of(layout.align().max(8));
        unsafe { ((aligned - 8) as *mut usize).write(raw as usize) };
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _: Layout) {
        if EXITED.load(Ordering::Relaxed) {
            return;
        }
        let raw = unsafe { ((ptr as usize - 8) as *const usize).read() };
        (boot().free_pool)(raw as *mut u8);
    }
}

#[global_allocator]
static ALLOCATOR: PoolAllocator = PoolAllocator;

// ---- konsol ve seri port ----

fn serial_byte(b: u8) {
    unsafe {
        for _ in 0..10_000 {
            let status: u8;
            asm!("in al, dx", out("al") status, in("dx") 0x3fdu16, options(nomem, nostack));
            if status & 0x20 != 0 {
                break;
            }
        }
        asm!("out dx, al", in("dx") 0x3f8u16, in("al") b, options(nomem, nostack));
    }
}

/// Açılış günlüğü dosyası (ESP'de `\boot\loader.log`); gerçek donanımda takılmaları bulmak için.
static LOG_FILE: AtomicPtr<FileProtocol> = AtomicPtr::new(null_mut());

fn open_log_file(image: Handle) {
    let Some(root) = open_root(image) else { return };
    let name = utf16(LOG_PATH);
    let mut file = null_mut();
    let rw = FILE_MODE_READ | FILE_MODE_WRITE;
    unsafe {
        if ((*root).open)(root, &mut file, name.as_ptr(), rw, 0) == SUCCESS {
            ((*file).delete)(file);
        }
        if ((*root).open)(root, &mut file, name.as_ptr(), rw | FILE_MODE_CREATE, 0) == SUCCESS {
            LOG_FILE.store(file, Ordering::Relaxed);
        }
        ((*root).close)(root);
    }
}

fn close_log_file() {
    let file = LOG_FILE.swap(null_mut(), Ordering::Relaxed);
    if !file.is_null() {
        unsafe { ((*file).close)(file) };
    }
}

/// Seri porta ve açılış günlüğüne tanı satırı.
fn log(text: &str) {
    for b in "loader: ".bytes().chain(text.bytes()).chain(*b"\r\n") {
        serial_byte(b);
    }
    let file = LOG_FILE.load(Ordering::Relaxed);
    if !file.is_null() && !EXITED.load(Ordering::Relaxed) {
        for part in [text.as_bytes(), b"\r\n"] {
            let mut size = part.len();
            unsafe { ((*file).write)(file, &mut size, part.as_ptr()) };
        }
        unsafe { ((*file).flush)(file) };
    }
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if EXITED.load(Ordering::Relaxed) {
            return Ok(());
        }
        let out = system().con_out;
        let mut buf = [0u16; 129];
        let mut n = 0;
        let flush = |buf: &mut [u16; 129], n: &mut usize| {
            buf[*n] = 0;
            unsafe { ((*out).output_string)(out, buf.as_ptr()) };
            *n = 0;
        };
        for c in s.chars() {
            if c == '\n' {
                buf[n] = '\r' as u16;
                n += 1;
            }
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                buf[n] = *u;
                n += 1;
            }
            if n >= 124 {
                flush(&mut buf, &mut n);
            }
        }
        flush(&mut buf, &mut n);
        Ok(())
    }
}

macro_rules! print {
    ($($arg:tt)*) => { let _ = write!(Console, $($arg)*); };
}

macro_rules! println {
    ($($arg:tt)*) => { let _ = writeln!(Console, $($arg)*); };
}

/// Ekrana ve günlüğe açılış adımı (takılırsa son adım ekranda kalır).
fn step(text: &str) {
    println!("  {}", text);
    log(text);
}

fn set_color(attribute: usize) {
    let out = system().con_out;
    unsafe { ((*out).set_attribute)(out, attribute) };
}

fn halt(message: &str) -> ! {
    log(message);
    set_color(0x0c);
    println!("\nHATA: {}", message);
    set_color(0x07);
    println!("Bilgisayari kapatip yeniden deneyin.");
    loop {
        unsafe { asm!("hlt") };
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut text = String::new();
    let _ = write!(text, "{}", info);
    halt(&text)
}

// ---- protokoller ----

fn protocol<T>(handle: Handle, guid: &Guid) -> Option<*mut T> {
    let mut out: *mut c_void = null_mut();
    ((boot().handle_protocol)(handle, guid, &mut out) == SUCCESS && !out.is_null())
        .then_some(out as *mut T)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Yükleyicinin bulunduğu bölümden bir dosyayı okur.
fn open_root(image: Handle) -> Option<*mut FileProtocol> {
    let loaded: *mut LoadedImage = protocol(image, &LOADED_IMAGE)?;
    let device = unsafe { (*loaded).device_handle };
    let fs: *mut SimpleFileSystem = protocol(device, &SIMPLE_FILE_SYSTEM)?;
    let mut root = null_mut();
    (unsafe { ((*fs).open_volume)(fs, &mut root) } == SUCCESS).then_some(root)
}

fn read_file(image: Handle, path: &str) -> Option<Vec<u8>> {
    let root = open_root(image)?;
    let name = utf16(path);
    let mut file = null_mut();
    let status = unsafe { ((*root).open)(root, &mut file, name.as_ptr(), FILE_MODE_READ, 0) };
    unsafe { ((*root).close)(root) };
    if status != SUCCESS {
        return None;
    }
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    loop {
        let mut size = chunk.len();
        if unsafe { ((*file).read)(file, &mut size, chunk.as_mut_ptr()) } != SUCCESS {
            unsafe { ((*file).close)(file) };
            return None;
        }
        if size == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..size]);
    }
    unsafe { ((*file).close)(file) };
    Some(data)
}

// ---- ayarlar ve menü ----

struct Config {
    timeout: u64,
    cmdline: String,
}

fn read_config(image: Handle) -> Config {
    let mut config = Config {
        timeout: 3,
        cmdline: String::new(),
    };
    let Some(data) = read_file(image, CONFIG_PATH) else {
        return config;
    };
    let text = String::from_utf8_lossy(&data);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "timeout" => config.timeout = value.trim().parse().unwrap_or(3),
                "cmdline" => config.cmdline = String::from(value.trim()),
                _ => {}
            }
        }
    }
    config
}

const ENTRIES: [(&str, &str); 4] = [
    ("Banna OS", ""),
    ("Banna OS - Kurtarma kipi", "recovery"),
    ("Banna OS - Metin konsolu (masaustu yok)", "console"),
    ("Banna OS - Onceki surum (guncelleme oncesi)", ""),
];
const PREVIOUS_ENTRY: usize = 3;

const SCAN_UP: u16 = 1;
const SCAN_DOWN: u16 = 2;
const SCAN_ESC: u16 = 0x17;

fn draw_menu(selected: usize, live: bool, countdown: Option<u64>) {
    let out = system().con_out;
    unsafe { ((*out).clear_screen)(out) };
    set_color(0x0b);
    println!("\n  BANNA OS{}\n", if live { "  (Live USB)" } else { "" });
    for (i, (name, _)) in ENTRIES.iter().enumerate() {
        if i == selected {
            set_color(0x70);
            println!("  > {}. {:<44}", i + 1, name);
        } else {
            set_color(0x07);
            println!("    {}. {:<44}", i + 1, name);
        }
    }
    set_color(0x08);
    println!("\n  Oklar ile sec, Enter ile baslat. B: kurtarma kipi.");
    if let Some(seconds) = countdown {
        print!("  {} saniye icinde baslatilacak...", seconds);
    }
    set_color(0x07);
}

fn read_key() -> Option<InputKey> {
    let input = system().con_in;
    let mut key = InputKey {
        scan_code: 0,
        unicode_char: 0,
    };
    (unsafe { ((*input).read_key_stroke)(input, &mut key) } == SUCCESS).then_some(key)
}

/// Menüyü gösterir; seçilen girdinin dizinini döndürür.
fn menu(timeout: u64, live: bool) -> usize {
    let mut selected = 0;
    let mut remaining = Some(timeout);
    let mut timer: Event = null_mut();
    (boot().create_event)(EVT_TIMER, 0, null(), null(), &mut timer);
    draw_menu(selected, live, remaining);
    if timeout == 0 {
        // Gizli menü: tuşa basılmamışsa doğrudan başlat.
        (boot().stall)(300_000);
        match read_key() {
            Some(_) => remaining = None,
            None => return 0,
        }
        draw_menu(selected, live, remaining);
    }
    loop {
        (boot().set_timer)(timer, TIMER_RELATIVE, 10_000_000);
        let events = [unsafe { (*system().con_in).wait_for_key }, timer];
        let mut index = 0;
        (boot().wait_for_event)(2, events.as_ptr(), &mut index);
        if index == 1 {
            if let Some(seconds) = remaining {
                if seconds <= 1 {
                    return selected;
                }
                remaining = Some(seconds - 1);
                draw_menu(selected, live, remaining);
            }
            continue;
        }
        let Some(key) = read_key() else { continue };
        remaining = None;
        match (key.scan_code, key.unicode_char) {
            (SCAN_UP, _) => selected = (selected + ENTRIES.len() - 1) % ENTRIES.len(),
            (SCAN_DOWN, _) => selected = (selected + 1) % ENTRIES.len(),
            (SCAN_ESC, _) => selected = 0,
            (_, 13) => return selected,
            (_, c) if (b'1' as u16..=b'4' as u16).contains(&c) => {
                return (c - b'1' as u16) as usize;
            }
            (_, c) if c == b'b' as u16 || c == b'B' as u16 => return 1,
            _ => {}
        }
        draw_menu(selected, live, remaining);
    }
}

// ---- grafik ----

struct Display {
    base: u64,
    width: u32,
    height: u32,
    pitch: u32,
    shifts: (u8, u8, u8),
}

fn mask_shift(mask: u32) -> u8 {
    if mask == 0 {
        0
    } else {
        mask.trailing_zeros() as u8
    }
}

/// Monitörün doğal çözünürlüğü: EDID'nin ilk ayrıntılı zamanlama tanımlayıcısı.
fn native_resolution() -> Option<(u32, u32)> {
    for guid in [&EDID_ACTIVE, &EDID_DISCOVERED] {
        let mut raw: *mut c_void = null_mut();
        if (boot().locate_protocol)(guid, null(), &mut raw) != SUCCESS || raw.is_null() {
            continue;
        }
        let edid = unsafe { &*(raw as *const EdidProtocol) };
        if edid.size < 128 || edid.edid.is_null() {
            continue;
        }
        let b = unsafe { core::slice::from_raw_parts(edid.edid, 128) };
        if b[0..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
            continue;
        }
        let d = &b[54..72];
        if d[0] == 0 && d[1] == 0 {
            continue;
        }
        let w = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
        let h = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
        if w >= 640 && h >= 480 {
            return Some((w, h));
        }
    }
    None
}

/// GOP kipini seçer: `\boot\display.conf`'taki tercih ("1920x1080"), yoksa monitörün
/// doğal çözünürlüğü, o da yoksa ≤ 2560x1600 en büyük kip. Çekirdeğe kip listesi
/// `gop=` ve `native=` olarak komut satırında iletilir.
fn setup_display(image: Handle) -> (Option<Display>, String) {
    let mut extra = String::new();
    let mut raw: *mut c_void = null_mut();
    if (boot().locate_protocol)(&GRAPHICS_OUTPUT, null(), &mut raw) != SUCCESS {
        return (None, extra);
    }
    let gop = raw as *mut GraphicsOutput;
    let usable = |info: &ModeInfo| info.pixel_format <= 2;
    let wanted = read_file(image, DISPLAY_PATH).and_then(|data| {
        let text = String::from_utf8_lossy(&data);
        let (w, h) = text.trim().split_once('x')?;
        Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?))
    });
    let native = native_resolution();
    unsafe {
        let mode = &*(*gop).mode;
        let mut modes: Vec<(u32, u32, u32)> = Vec::new();
        for m in 0..mode.max_mode {
            let mut size = 0;
            let mut info = null_mut();
            if ((*gop).query_mode)(gop, m, &mut size, &mut info) != SUCCESS || info.is_null() {
                continue;
            }
            let info = &*info;
            let (w, h) = (info.horizontal_resolution, info.vertical_resolution);
            if usable(info) && w >= 640 && h >= 480 && w <= 4096 && h <= 2160 {
                if !modes.iter().any(|&(_, mw, mh)| (mw, mh) == (w, h)) {
                    modes.push((m, w, h));
                }
            }
        }
        modes.sort_by_key(|&(_, w, h)| core::cmp::Reverse(w as u64 * h as u64));
        let find = |want: Option<(u32, u32)>| {
            want.and_then(|(w, h)| modes.iter().find(|&&(_, mw, mh)| (mw, mh) == (w, h)))
        };
        let fits = |w: u32, h: u32| w <= MAX_WIDTH && h <= MAX_HEIGHT && w >= 1024;
        let current = &*mode.info;
        let current_size = (current.horizontal_resolution, current.vertical_resolution);
        let target = find(wanted)
            .or_else(|| find(native.filter(|&(w, h)| fits(w, h))))
            .copied()
            .or_else(|| {
                if usable(current) && fits(current_size.0, current_size.1) {
                    None
                } else {
                    modes.iter().find(|&&(_, w, h)| fits(w, h)).copied()
                }
            });
        if let Some((m, w, h)) = target
            && (w, h) != current_size
        {
            if ((*gop).set_mode)(gop, m) != SUCCESS {
                log(&alloc::format!("display: set_mode {}x{} failed", w, h));
            }
        }
        if !modes.is_empty() {
            extra.push_str(" gop=");
            for (i, (_, w, h)) in modes.iter().enumerate() {
                if i > 0 {
                    extra.push(',');
                }
                let _ = write!(extra, "{}x{}", w, h);
            }
        }
        if let Some((w, h)) = native {
            let _ = write!(extra, " native={}x{}", w, h);
        }
        log(&alloc::format!(
            "display: {} GOP mode(s), native {:?}, wanted {:?}",
            modes.len(),
            native,
            wanted
        ));
        let mode = &*(*gop).mode;
        let info = &*mode.info;
        let shifts = match info.pixel_format {
            0 => (0, 8, 16),
            1 => (16, 8, 0),
            2 => (
                mask_shift(info.pixel_information.red),
                mask_shift(info.pixel_information.green),
                mask_shift(info.pixel_information.blue),
            ),
            _ => return (None, extra),
        };
        (
            Some(Display {
                base: mode.frame_buffer_base,
                width: info.horizontal_resolution,
                height: info.vertical_resolution,
                pitch: info.pixels_per_scan_line * 4,
                shifts,
            }),
            extra,
        )
    }
}

// ---- kök bölümü bulma ----

const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const LINUX_ROOT_TYPE: [u8; 16] = [
    0xe3, 0xbc, 0x68, 0x4f, 0xcd, 0xe8, 0xb1, 0x4d, 0x96, 0xe7, 0xfb, 0xca, 0xf9, 0x84, 0xb7, 0x09,
];

fn guid_text(b: &[u8]) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3],
        b[2],
        b[1],
        b[0],
        b[5],
        b[4],
        b[7],
        b[6],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    );
    s
}

/// Yükleyicinin bölümünün GPT kimliği (aygıt yolundaki sabit disk düğümü).
fn boot_partition_guid(image: Handle) -> Option<[u8; 16]> {
    let loaded: *mut LoadedImage = protocol(image, &LOADED_IMAGE)?;
    let mut node: *const u8 = protocol::<u8>(unsafe { (*loaded).device_handle }, &DEVICE_PATH)?;
    for _ in 0..64 {
        let header = unsafe { &*(node as *const DevicePathNode) };
        let length = u16::from_le_bytes(header.length) as usize;
        if header.kind == 0x7f || length < 4 {
            return None;
        }
        if header.kind == 4 && header.sub_kind == 1 && length >= 42 {
            let signature_type = unsafe { *node.add(41) };
            if signature_type == 2 {
                let mut guid = [0u8; 16];
                guid.copy_from_slice(unsafe { core::slice::from_raw_parts(node.add(24), 16) });
                return Some(guid);
            }
        }
        node = unsafe { node.add(length) };
    }
    None
}

fn read_blocks(io: *mut BlockIo, lba: u64, bytes: usize) -> Option<Vec<u8>> {
    let media = unsafe { &*(*io).media };
    let block = media.block_size as usize;
    let size = bytes.div_ceil(block) * block;
    let mut buf = vec![0u8; size + 4096];
    let offset = buf.as_ptr().align_offset(4096);
    let status =
        unsafe { ((*io).read_blocks)(io, media.media_id, lba, size, buf.as_mut_ptr().add(offset)) };
    (status == SUCCESS).then(|| buf[offset..offset + size].to_vec())
}

/// Yükleyicinin diskindeki Linux kök bölümünün benzersiz kimliği.
fn find_root(image: Handle) -> Option<[u8; 16]> {
    let esp = boot_partition_guid(image)?;
    let mut count = 0;
    let mut handles: *mut Handle = null_mut();
    if (boot().locate_handle_buffer)(BY_PROTOCOL, &BLOCK_IO, null(), &mut count, &mut handles)
        != SUCCESS
    {
        return None;
    }
    for i in 0..count {
        let handle = unsafe { *handles.add(i) };
        let Some(io) = protocol::<BlockIo>(handle, &BLOCK_IO) else {
            continue;
        };
        let media = unsafe { &*(*io).media };
        if media.logical_partition != 0 || media.media_present == 0 {
            continue;
        }
        let block = media.block_size as u64;
        let Some(header) = read_blocks(io, 1, 512) else {
            continue;
        };
        if &header[0..8] != b"EFI PART" {
            continue;
        }
        let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
        let entry_count = u32::from_le_bytes(header[80..84].try_into().unwrap()).min(256) as usize;
        let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
        if entry_size < 128 || block == 0 {
            continue;
        }
        let Some(table) = read_blocks(io, entries_lba, entry_count * entry_size) else {
            continue;
        };
        let entries: Vec<&[u8]> = table.chunks(entry_size).take(entry_count).collect();
        if !entries.iter().any(|e| e[16..32] == esp) {
            continue;
        }
        let root = entries
            .iter()
            .find(|e| e[0..16] == LINUX_ROOT_TYPE)
            .or_else(|| {
                entries
                    .iter()
                    .find(|e| e[0..16] != ESP_TYPE && e[0..16] != [0; 16])
            })?;
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&root[16..32]);
        return Some(guid);
    }
    None
}

// ---- çekirdek ----

struct Segment {
    vaddr: u64,
    memsz: u64,
    writable: bool,
    executable: bool,
}

struct Kernel {
    phys: u64,
    virt: u64,
    span: u64,
    entry: u64,
    segments: Vec<Segment>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Çekirdeğe devredilen ayırmalar: (adres, uzunluk, etiket); bellek haritasında ayrı gösterilir.
static mut TAGGED: [(u64, u64, u32); 8] = [(0, 0, 0); 8];
static mut TAGGED_COUNT: usize = 0;

fn allocate_pages(kind: u32, pages: usize) -> u64 {
    let mut address = 0u64;
    let firmware_kind = if kind >= KERNEL_MEMORY {
        LOADER_DATA
    } else {
        kind
    };
    if (boot().allocate_pages)(ALLOCATE_ANY_PAGES, firmware_kind, pages, &mut address) != SUCCESS {
        halt("bellek ayrilamadi");
    }
    if kind >= KERNEL_MEMORY {
        unsafe {
            if TAGGED_COUNT < 8 {
                TAGGED[TAGGED_COUNT] = (address, pages as u64 * PAGE, kind);
                TAGGED_COUNT += 1;
            }
        }
    }
    unsafe { core::ptr::write_bytes(address as *mut u8, 0, pages * PAGE as usize) };
    address
}

fn valid_kernel(image: &[u8]) -> bool {
    image.len() >= 64 && &image[0..4] == b"\x7fELF" && image[4] == 2 && u16_at(image, 18) == 0x3e
}

fn load_kernel(image: &[u8]) -> Kernel {
    if !valid_kernel(image) {
        halt("cekirdek gecerli bir x86_64 ELF degil");
    }
    let entry = u64_at(image, 24);
    let phoff = u64_at(image, 32) as usize;
    let phentsize = u16_at(image, 54) as usize;
    let phnum = u16_at(image, 56) as usize;
    let mut loads = Vec::new();
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        if u32_at(image, at) != 1 {
            continue;
        }
        loads.push((
            u32_at(image, at + 4),
            u64_at(image, at + 8),
            u64_at(image, at + 16),
            u64_at(image, at + 32),
            u64_at(image, at + 40),
        ));
    }
    let low = loads.iter().map(|l| l.2).min().unwrap_or(0) & !(PAGE - 1);
    let high = loads
        .iter()
        .map(|l| l.2 + l.4)
        .max()
        .unwrap_or(0)
        .next_multiple_of(PAGE);
    if loads.is_empty() || high <= low {
        halt("cekirdekte yuklenecek bolum yok");
    }
    let span = high - low;
    let phys = allocate_pages(KERNEL_MEMORY, (span / PAGE) as usize);
    let mut segments = Vec::new();
    for &(flags, offset, vaddr, filesz, memsz) in &loads {
        let dest = (phys + (vaddr - low)) as *mut u8;
        let data = &image[offset as usize..(offset + filesz) as usize];
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len()) };
        segments.push(Segment {
            vaddr,
            memsz,
            writable: flags & 2 != 0,
            executable: flags & 1 != 0,
        });
    }
    Kernel {
        phys,
        virt: low,
        span,
        entry,
        segments,
    }
}

// ---- sayfa tabloları ----

const PRESENT: u64 = 1;
const WRITABLE: u64 = 2;
const HUGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;

struct Pool {
    next: u64,
    end: u64,
}

impl Pool {
    fn page(&mut self) -> u64 {
        if self.next >= self.end {
            halt("sayfa tablosu alani bitti");
        }
        let page = self.next;
        self.next += PAGE;
        page
    }
}

struct Paging {
    pml4: u64,
    pool: Pool,
    nx: bool,
    gigabyte: bool,
}

fn table(phys: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(phys as *mut [u64; 512]) }
}

impl Paging {
    /// `depth` seviye inip ara tabloyu döndürür (gerekirse oluşturur); yol üzerinde
    /// zaten büyük bir sayfa varsa adres eşlidir ve `None` döner.
    fn walk(&mut self, virt: u64, depth: usize) -> Option<u64> {
        let mut current = self.pml4;
        for level in 0..depth {
            let index = ((virt >> (39 - level * 9)) & 0x1ff) as usize;
            let entry = table(current)[index];
            current = if entry & PRESENT == 0 {
                let new = self.pool.page();
                table(current)[index] = new | PRESENT | WRITABLE;
                new
            } else if entry & HUGE != 0 {
                return None;
            } else {
                entry & 0x000f_ffff_ffff_f000
            };
        }
        Some(current)
    }

    fn map(&mut self, virt: u64, phys: u64, size: u64, flags: u64) {
        let flags = if self.nx { flags } else { flags & !NO_EXECUTE };
        let index = |shift: u64| ((virt >> shift) & 0x1ff) as usize;
        let (depth, shift, huge) = match size {
            0x4000_0000 => (1, 30, HUGE),
            0x20_0000 => (2, 21, HUGE),
            _ => (3, 12, 0),
        };
        let Some(t) = self.walk(virt, depth) else {
            return;
        };
        if table(t)[index(shift)] & PRESENT == 0 || huge == 0 {
            table(t)[index(shift)] = phys | flags | huge | PRESENT;
        }
    }

    /// Fiziksel [start, end) aralığını HHDM'ye eşler (1 GiB ya da 2 MiB sayfalarla).
    fn map_hhdm(&mut self, start: u64, end: u64) {
        let mut addr = start & !0x1f_ffff;
        while addr < end {
            if self.gigabyte && addr % 0x4000_0000 == 0 && end - addr >= 0x4000_0000 {
                self.map(HHDM + addr, addr, 0x4000_0000, WRITABLE | NO_EXECUTE);
                addr += 0x4000_0000;
            } else {
                self.map(HHDM + addr, addr, 0x20_0000, WRITABLE | NO_EXECUTE);
                addr += 0x20_0000;
            }
        }
    }

    fn identity(&mut self, start: u64, len: u64) {
        let mut page = start & !(PAGE - 1);
        while page < start + len {
            self.map(page, page, PAGE, WRITABLE);
            page += PAGE;
        }
    }
}

fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let r = core::arch::x86_64::__cpuid(leaf);
    (r.eax, r.ebx, r.ecx, r.edx)
}

// ---- Limine yanıtları ----

const COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];
const BASE_REVISION_MAGIC: [u64; 2] = [0xf9562b2d5c95a6c8, 0x6a7b384944536bdc];
const STACK_ID: [u64; 2] = [0x224ef0460a8e8926, 0xe1cb0fc25f46ea3d];
const HHDM_ID: [u64; 2] = [0x48dcf1cb8ad2b852, 0x63984e959a98244b];
const FRAMEBUFFER_ID: [u64; 2] = [0x9d5827dcd881dd75, 0xa3148604f6fab11b];
const MEMMAP_ID: [u64; 2] = [0x67cf3d9d378a806f, 0xe304acdfc50c3c62];
const EXECUTABLE_ADDRESS_ID: [u64; 2] = [0x71ba76863cc55f63, 0xb2644a48c516a487];
const RSDP_ID: [u64; 2] = [0xc5e77b6b397e7b43, 0x27637845accdcf3c];
const EXECUTABLE_FILE_ID: [u64; 2] = [0xad97e90e83f1ed67, 0x31eb5d1c5ff23b69];
const EFI_SYSTEM_TABLE_ID: [u64; 2] = [0x5ceba5163eaaf6d6, 0x0a6981610cf65fcc];
const EFI_MEMMAP_ID: [u64; 2] = [0x7df62a431d6872d5, 0xa4fcdfb3e57306c8];
/// Çekirdeğe verilen UEFI çalışma zamanı bölgeleri için alan (ResetSystem çağrısı).
const RUNTIME_MAP_SIZE: u64 = 16 * 1024;
const EFI_MEMORY_RUNTIME: u64 = 1 << 63;

/// Çekirdeğe verilen yapıların tutulduğu alan (fiziksel; çekirdek HHDM'den görür).
struct Data {
    base: u64,
    used: u64,
    size: u64,
}

impl Data {
    fn alloc(&mut self, size: u64) -> u64 {
        let at = (self.base + self.used).next_multiple_of(16);
        if at + size > self.base + self.size {
            halt("devir alani bitti");
        }
        self.used = at + size - self.base;
        at
    }

    fn put(&mut self, words: &[u64]) -> u64 {
        let at = self.alloc(words.len() as u64 * 8);
        for (i, w) in words.iter().enumerate() {
            unsafe { ((at + i as u64 * 8) as *mut u64).write(*w) };
        }
        at
    }

    fn string(&mut self, text: &str) -> u64 {
        let at = self.alloc(text.len() as u64 + 1);
        unsafe {
            core::ptr::copy_nonoverlapping(text.as_ptr(), at as *mut u8, text.len());
            ((at + text.len() as u64) as *mut u8).write(0);
        }
        at
    }
}

fn virt(phys: u64) -> u64 {
    phys + HHDM
}

/// Çekirdek imajındaki istekleri bulur: (id, isteğin fiziksel adresi).
fn find_requests(kernel: &Kernel) -> Vec<([u64; 2], u64)> {
    let mut found = Vec::new();
    let mut at = kernel.phys;
    let end = kernel.phys + kernel.span - 32;
    while at < end {
        let w = unsafe { core::slice::from_raw_parts(at as *const u64, 4) };
        if w[0] == COMMON_MAGIC[0] && w[1] == COMMON_MAGIC[1] {
            found.push(([w[2], w[3]], at));
        } else if w[0] == BASE_REVISION_MAGIC[0] && w[1] == BASE_REVISION_MAGIC[1] && w[2] <= 3 {
            // Desteklenen revizyonda üçüncü sözcük sıfırlanır.
            unsafe { ((at + 16) as *mut u64).write(0) };
        }
        at += 8;
    }
    found
}

fn respond(requests: &[([u64; 2], u64)], id: [u64; 2], response_phys: u64) {
    for (found, at) in requests {
        if *found == id {
            unsafe { ((at + 40) as *mut u64).write(virt(response_phys)) };
        }
    }
}

fn memory_kind(efi_kind: u32) -> u64 {
    match efi_kind {
        1..=4 | 7 => 0,
        9 => 2,
        10 => 3,
        8 => 4,
        KERNEL_MEMORY => 6,
        HANDOFF_MEMORY => 5,
        _ => 1,
    }
}

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: Handle, system_table: *mut SystemTable) -> Status {
    SYSTEM.store(system_table, Ordering::Relaxed);
    (boot().set_watchdog_timer)(0, 0, 0, null());
    open_log_file(image);
    log("Banna OS UEFI loader");

    let config = read_config(image);
    let live = config.cmdline.split_whitespace().any(|w| w == "live");
    let choice = menu(config.timeout, live);
    let out = system().con_out;
    unsafe { ((*out).clear_screen)(out) };
    println!("Banna OS yukleniyor...\n");
    step(ENTRIES[choice].0);
    step("Kok bolum araniyor");

    let mut cmdline = config.cmdline.clone();
    let extra = ENTRIES[choice].1;
    if !extra.is_empty() {
        cmdline.push(' ');
        cmdline.push_str(extra);
    }
    match find_root(image) {
        Some(guid) => {
            cmdline.push_str(" root=PARTUUID=");
            cmdline.push_str(&guid_text(&guid));
        }
        None => log("root partition not found, kernel will search"),
    }
    let mut cmdline = String::from(cmdline.trim());
    let mut line = String::from("cmdline: ");
    line.push_str(&cmdline);
    log(&line);

    // Önceki sürüm seçildiyse ya da yeni çekirdek okunamıyor/bozuksa eski çekirdek denenir.
    let (first, second) = if choice == PREVIOUS_ENTRY {
        (PREVIOUS_KERNEL_PATH, KERNEL_PATH)
    } else {
        (KERNEL_PATH, PREVIOUS_KERNEL_PATH)
    };
    let (kernel_path, kernel_file) = match read_file(image, first).filter(|k| valid_kernel(k)) {
        Some(k) => (first, k),
        None => {
            log("kernel unreadable or invalid, trying the other kernel");
            match read_file(image, second).filter(|k| valid_kernel(k)) {
                Some(k) => {
                    println!("Uyari: {} kullanilamadi, {} baslatiliyor.", first, second);
                    (second, k)
                }
                None => halt("\\boot\\banna_os bulunamadi"),
            }
        }
    };
    log(&alloc::format!(
        "kernel: {} ({} bytes)",
        kernel_path,
        kernel_file.len()
    ));
    step(&alloc::format!(
        "Cekirdek okundu ({} KiB), bellege yerlestiriliyor",
        kernel_file.len() / 1024
    ));
    let kernel = load_kernel(&kernel_file);
    drop(kernel_file);
    step("Ekran kuruluyor");
    let (display, display_args) = setup_display(image);
    cmdline.push_str(&display_args);
    match &display {
        Some(d) => step(&alloc::format!(
            "Ekran {}x{}, cerceve arabellegi {:#x}",
            d.width,
            d.height,
            d.base
        )),
        None => step("Dogrusal cerceve arabellegi yok: cekirdek ekrana cizemeyecek"),
    }

    let rsdp = unsafe {
        let st = system();
        let tables =
            core::slice::from_raw_parts(st.configuration_table, st.number_of_table_entries);
        tables
            .iter()
            .find(|t| t.vendor_guid == ACPI_20_TABLE)
            .or_else(|| tables.iter().find(|t| t.vendor_guid == ACPI_10_TABLE))
            .map_or(0, |t| t.vendor_table as u64)
    };

    step("Sayfa tablolari hazirlaniyor");
    // Çekirdeğe devredilen alanlar.
    let stack = allocate_pages(HANDOFF_MEMORY, (STACK_SIZE / PAGE) as usize);
    let tables_base = allocate_pages(HANDOFF_MEMORY, TABLE_POOL_PAGES);
    let mut data = Data {
        base: allocate_pages(HANDOFF_MEMORY, DATA_POOL_SIZE / PAGE as usize),
        used: 0,
        size: DATA_POOL_SIZE as u64,
    };

    // Sayfa tabloları: HHDM, çekirdek ve geçiş kodu için kimlik eşlemesi.
    let nx = cpuid(0x8000_0000).0 >= 0x8000_0001 && cpuid(0x8000_0001).3 & (1 << 20) != 0;
    let gigabyte = cpuid(0x8000_0000).0 >= 0x8000_0001 && cpuid(0x8000_0001).3 & (1 << 26) != 0;
    let mut paging = Paging {
        pml4: 0,
        pool: Pool {
            next: tables_base,
            end: tables_base + TABLE_POOL_PAGES as u64 * PAGE,
        },
        nx,
        gigabyte,
    };
    paging.pml4 = paging.pool.page();

    let mut map_size = 0usize;
    let mut key = 0usize;
    let mut desc_size = 0usize;
    let mut desc_version = 0u32;
    (boot().get_memory_map)(
        &mut map_size,
        null_mut(),
        &mut key,
        &mut desc_size,
        &mut desc_version,
    );
    let map_capacity = map_size + 64 * desc_size.max(48);
    let map_buffer = allocate_pages(LOADER_DATA, map_capacity.div_ceil(PAGE as usize) + 1);
    let mut size = map_capacity;
    if (boot().get_memory_map)(
        &mut size,
        map_buffer as *mut MemoryDescriptor,
        &mut key,
        &mut desc_size,
        &mut desc_version,
    ) != SUCCESS
    {
        halt("bellek haritasi alinamadi");
    }
    paging.map_hhdm(0, 0x1_0000_0000);
    for i in 0..size / desc_size {
        let d = unsafe { &*((map_buffer + (i * desc_size) as u64) as *const MemoryDescriptor) };
        let end = d.physical_start + d.number_of_pages * PAGE;
        if end > 0x1_0000_0000 {
            paging.map_hhdm(d.physical_start.max(0x1_0000_0000), end);
        }
    }
    if let Some(d) = &display {
        let fb_size = d.pitch as u64 * d.height as u64;
        paging.map_hhdm(d.base, d.base + fb_size);
    }
    for segment in &kernel.segments {
        let mut flags = 0;
        if segment.writable {
            flags |= WRITABLE;
        }
        if !segment.executable {
            flags |= NO_EXECUTE;
        }
        let first = segment.vaddr & !(PAGE - 1);
        let last = (segment.vaddr + segment.memsz).next_multiple_of(PAGE);
        let mut page = first;
        while page < last {
            paging.map(page, kernel.phys + (page - kernel.virt), PAGE, flags);
            page += PAGE;
        }
    }
    let jump_address = jump_to_kernel as *const () as u64;
    paging.identity(jump_address, 2 * PAGE);
    let mut gdtr = [0u8; 10];
    unsafe { asm!("sgdt [{}]", in(reg) gdtr.as_mut_ptr(), options(nostack)) };
    let gdt_limit = u16::from_le_bytes([gdtr[0], gdtr[1]]) as u64;
    let gdt_base = u64::from_le_bytes(gdtr[2..10].try_into().unwrap());
    paging.identity(gdt_base, gdt_limit + 1);

    // Yanıtlar.
    let requests = find_requests(&kernel);
    respond(&requests, STACK_ID, data.put(&[0]));
    respond(&requests, HHDM_ID, data.put(&[0, HHDM]));
    respond(
        &requests,
        EXECUTABLE_ADDRESS_ID,
        data.put(&[0, kernel.phys, kernel.virt]),
    );
    if rsdp != 0 {
        respond(&requests, RSDP_ID, data.put(&[0, rsdp]));
    }
    let path = data.string("boot():/boot/banna_os");
    let cmdline_phys = data.string(&cmdline);
    let file = data.put(&[
        0,
        virt(kernel.phys),
        kernel.span,
        virt(path),
        virt(cmdline_phys),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    respond(&requests, EXECUTABLE_FILE_ID, data.put(&[0, virt(file)]));
    if let Some(d) = &display {
        let fb = data.alloc(128);
        unsafe {
            core::ptr::write_bytes(fb as *mut u8, 0, 128);
            (fb as *mut u64).write(virt(d.base));
            ((fb + 8) as *mut u64).write(d.width as u64);
            ((fb + 16) as *mut u64).write(d.height as u64);
            ((fb + 24) as *mut u64).write(d.pitch as u64);
            ((fb + 32) as *mut u16).write(32);
            let bytes = (fb + 34) as *mut u8;
            let (r, g, b) = d.shifts;
            for (i, v) in [1u8, 8, r, 8, g, 8, b].iter().enumerate() {
                bytes.add(i).write(*v);
            }
        }
        let list = data.put(&[virt(fb)]);
        respond(&requests, FRAMEBUFFER_ID, data.put(&[0, 1, virt(list)]));
    }
    let memmap_entries = data.alloc(MAX_MEMMAP as u64 * 24);
    let memmap_pointers = data.alloc(MAX_MEMMAP as u64 * 8);
    let memmap_response = data.put(&[0, 0, virt(memmap_pointers)]);
    respond(&requests, MEMMAP_ID, memmap_response);
    let runtime_map = data.alloc(RUNTIME_MAP_SIZE);
    let efi_memmap_response = data.put(&[0, 0, 0, 0, 0]);
    respond(&requests, EFI_MEMMAP_ID, efi_memmap_response);
    respond(
        &requests,
        EFI_SYSTEM_TABLE_ID,
        data.put(&[0, SYSTEM.load(Ordering::Relaxed) as u64]),
    );

    let mut message = String::from("kernel at ");
    let _ = write!(
        message,
        "{:#x} ({} KiB), entry {:#x}, {} requests answered",
        kernel.phys,
        kernel.span / 1024,
        kernel.entry,
        requests.len()
    );
    log(&message);
    let stack_top = virt(stack + STACK_SIZE - 16);
    let entry = kernel.entry;
    let pml4 = paging.pml4;

    step("Cekirdege geciliyor");
    close_log_file();
    // ExitBootServices: harita anahtarı değiştiyse bir kez daha alınır.
    let mut exited = false;
    for _ in 0..4 {
        let mut size = map_capacity;
        if (boot().get_memory_map)(
            &mut size,
            map_buffer as *mut MemoryDescriptor,
            &mut key,
            &mut desc_size,
            &mut desc_version,
        ) != SUCCESS
        {
            continue;
        }
        map_size = size;
        if (boot().exit_boot_services)(image, key) == SUCCESS {
            exited = true;
            break;
        }
    }
    if !exited {
        halt("ExitBootServices basarisiz");
    }
    EXITED.store(true, Ordering::Relaxed);

    // Bellek haritası: sıralı, bitişik aynı türler birleşik (ayırma yapılmaz).
    let entries = memmap_entries as *mut [u64; 3];
    let mut count = 0usize;
    for i in 0..map_size / desc_size {
        let d = unsafe { &*((map_buffer + (i * desc_size) as u64) as *const MemoryDescriptor) };
        let (base, length, kind) = (
            d.physical_start,
            d.number_of_pages * PAGE,
            memory_kind(d.kind),
        );
        if length == 0 || count >= MAX_MEMMAP {
            continue;
        }
        let mut j = count;
        while j > 0 && unsafe { (*entries.add(j - 1))[0] } > base {
            unsafe { *entries.add(j) = *entries.add(j - 1) };
            j -= 1;
        }
        unsafe { *entries.add(j) = [base, length, kind] };
        count += 1;
    }
    // Etiketli ayırmalar içinde bulundukları LOADER_DATA girdisinden ayrılır.
    for t in 0..unsafe { TAGGED_COUNT } {
        let (start, length, tag) = unsafe { TAGGED[t] };
        let end = start + length;
        let Some(i) = (0..count).find(|&i| {
            let e = unsafe { *entries.add(i) };
            e[0] <= start && end <= e[0] + e[1]
        }) else {
            continue;
        };
        let e = unsafe { *entries.add(i) };
        let pieces = [
            [e[0], start - e[0], e[2]],
            [start, length, memory_kind(tag)],
            [end, e[0] + e[1] - end, e[2]],
        ];
        let mut kept = [[0u64; 3]; 3];
        let mut n = 0;
        for p in pieces {
            if p[1] > 0 {
                kept[n] = p;
                n += 1;
            }
        }
        let pieces = &kept[..n];
        let extra = n - 1;
        if count + extra > MAX_MEMMAP {
            continue;
        }
        let mut j = count;
        while j > i + 1 {
            j -= 1;
            unsafe { *entries.add(j + extra) = *entries.add(j) };
        }
        for (k, p) in pieces.iter().enumerate() {
            unsafe { *entries.add(i + k) = *p };
        }
        count += extra;
    }
    let mut merged = 0usize;
    for i in 0..count {
        let e = unsafe { *entries.add(i) };
        if merged > 0 {
            let last = unsafe { &mut *entries.add(merged - 1) };
            if last[2] == e[2] && last[0] + last[1] == e[0] {
                last[1] += e[1];
                continue;
            }
        }
        unsafe { *entries.add(merged) = e };
        merged += 1;
    }
    for i in 0..merged {
        unsafe {
            ((memmap_pointers + i as u64 * 8) as *mut u64)
                .write(virt(memmap_entries + i as u64 * 24))
        };
    }
    unsafe { ((memmap_response + 8) as *mut u64).write(merged as u64) };

    // Yalnızca çalışma zamanı bölgeleri (fiziksel adresleriyle) çekirdeğe kalır.
    let mut runtime_bytes = 0u64;
    for i in 0..map_size / desc_size {
        let at = map_buffer + (i * desc_size) as u64;
        let d = unsafe { &*(at as *const MemoryDescriptor) };
        if d.attribute & EFI_MEMORY_RUNTIME == 0
            || runtime_bytes + desc_size as u64 > RUNTIME_MAP_SIZE
        {
            continue;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                at as *const u8,
                (runtime_map + runtime_bytes) as *mut u8,
                desc_size,
            )
        };
        runtime_bytes += desc_size as u64;
    }
    unsafe {
        let words = efi_memmap_response as *mut u64;
        words.add(1).write(virt(runtime_map));
        words.add(2).write(runtime_bytes);
        words.add(3).write(desc_size as u64);
        words.add(4).write(desc_version as u64);
    }

    if nx {
        unsafe {
            let (low, high): (u32, u32);
            asm!("rdmsr", in("ecx") 0xc000_0080u32, out("eax") low, out("edx") high);
            let efer = ((high as u64) << 32 | low as u64) | (1 << 11);
            asm!("wrmsr", in("ecx") 0xc000_0080u32, in("eax") efer as u32, in("edx") (efer >> 32) as u32);
        }
    }
    unsafe { jump_to_kernel(pml4, stack_top, entry) }
}

/// Yeni sayfa tablolarına geçer ve çekirdeğe atlar; kimlik eşlemeli sayfada çalışır.
#[unsafe(naked)]
unsafe extern "sysv64" fn jump_to_kernel(pml4: u64, stack: u64, entry: u64) -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov cr3, rdi",
        "mov rsp, rsi",
        "push 0",
        "xor ebp, ebp",
        "jmp rdx",
    )
}
