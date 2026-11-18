//! Statik ELF64 (x86_64, ET_EXEC) yükleyici ve işlem başlatma.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::fs::{self, FsError};
use crate::memory::paging::{self, NO_EXECUTE, USER, USER_SPACE_END, WRITABLE};
use crate::memory::{PAGE_SIZE, align_down, align_up, phys_to_virt, pmm};
use crate::sched::{self, Completion, Process, UserStart};

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
pub const STACK_TOP: u64 = 0x0000_7fff_ff00_0000;
const STACK_PAGES: u64 = 32;
const MAX_IMAGE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecError {
    Fs(FsError),
    NotElf,
    Unsupported,
    BadSegment,
    OutOfMemory,
    TooLarge,
}

impl ExecError {
    pub fn errno(self) -> i64 {
        match self {
            ExecError::Fs(e) => e.errno(),
            ExecError::OutOfMemory => -12,
            ExecError::TooLarge => -7,
            _ => -8,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            ExecError::Fs(e) => e.message(),
            ExecError::NotElf => "not an ELF executable",
            ExecError::Unsupported => "unsupported ELF (needs static x86_64 ET_EXEC)",
            ExecError::BadSegment => "invalid program segment",
            ExecError::OutOfMemory => "out of memory",
            ExecError::TooLarge => "arguments or image too large",
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

struct Segment {
    offset: u64,
    vaddr: u64,
    file_size: u64,
    mem_size: u64,
    flags: u32,
}

fn parse(image: &[u8]) -> Result<(u64, Vec<Segment>), ExecError> {
    if image.len() < 64 || &image[0..4] != b"\x7fELF" {
        return Err(ExecError::NotElf);
    }
    if image[4] != 2
        || image[5] != 1
        || u16_at(image, 18) != Some(0x3e)
        || u16_at(image, 16) != Some(2)
    {
        return Err(ExecError::Unsupported);
    }
    let entry = u64_at(image, 24).ok_or(ExecError::NotElf)?;
    let phoff = u64_at(image, 32).ok_or(ExecError::NotElf)? as usize;
    let phentsize = u16_at(image, 54).ok_or(ExecError::NotElf)? as usize;
    let phnum = u16_at(image, 56).ok_or(ExecError::NotElf)? as usize;
    let mut segments = Vec::new();
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        if u32_at(image, at).ok_or(ExecError::NotElf)? != PT_LOAD {
            continue;
        }
        let segment = Segment {
            flags: u32_at(image, at + 4).ok_or(ExecError::NotElf)?,
            offset: u64_at(image, at + 8).ok_or(ExecError::NotElf)?,
            vaddr: u64_at(image, at + 16).ok_or(ExecError::NotElf)?,
            file_size: u64_at(image, at + 32).ok_or(ExecError::NotElf)?,
            mem_size: u64_at(image, at + 40).ok_or(ExecError::NotElf)?,
        };
        let end = segment
            .vaddr
            .checked_add(segment.mem_size)
            .ok_or(ExecError::BadSegment)?;
        if segment.vaddr < PAGE_SIZE
            || end > USER_SPACE_END
            || segment.file_size > segment.mem_size
            || segment.offset + segment.file_size > image.len() as u64
        {
            return Err(ExecError::BadSegment);
        }
        segments.push(segment);
    }
    if segments.is_empty()
        || !segments
            .iter()
            .any(|s| entry >= s.vaddr && entry < s.vaddr + s.mem_size)
    {
        return Err(ExecError::BadSegment);
    }
    Ok((entry, segments))
}

/// Kullanıcı adres alanına bayt kopyalar (sayfaların eşli olduğu varsayılır).
fn copy_into(root: u64, mut vaddr: u64, mut data: &[u8]) -> Result<(), ExecError> {
    while !data.is_empty() {
        let phys = paging::translate_in(root, vaddr).ok_or(ExecError::BadSegment)?;
        let room = (PAGE_SIZE - (vaddr % PAGE_SIZE)) as usize;
        let take = room.min(data.len());
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), phys_to_virt(phys), take) };
        vaddr += take as u64;
        data = &data[take..];
    }
    Ok(())
}

fn map_range(root: u64, start: u64, end: u64, flags: u64) -> Result<(), ExecError> {
    let mut page = align_down(start, PAGE_SIZE);
    while page < align_up(end, PAGE_SIZE) {
        if paging::translate_in(root, page).is_none() {
            let frame = pmm::alloc_zeroed_frame().ok_or(ExecError::OutOfMemory)?;
            if paging::map_page_in(root, page, frame, flags | USER).is_err() {
                pmm::free_frame(frame);
                return Err(ExecError::OutOfMemory);
            }
        }
        page += PAGE_SIZE;
    }
    Ok(())
}

/// Argümanları yığının tepesine yerleştirir; (rsp, argv) döndürür.
fn build_stack(root: u64, args: &[String]) -> Result<(u64, u64), ExecError> {
    let total: usize = args.iter().map(|a| a.len()).sum::<usize>() + args.len() * 16 + 64;
    if total > (STACK_PAGES as usize - 1) * PAGE_SIZE as usize / 2 {
        return Err(ExecError::TooLarge);
    }
    let mut sp = STACK_TOP;
    let mut pairs: Vec<[u64; 2]> = Vec::with_capacity(args.len());
    for arg in args {
        sp -= arg.len() as u64;
        copy_into(root, sp, arg.as_bytes())?;
        pairs.push([sp, arg.len() as u64]);
    }
    sp = align_down(sp, 16);
    let table_bytes = pairs.len() * 16;
    sp -= table_bytes as u64;
    sp = align_down(sp, 16);
    let argv = sp;
    let mut raw = Vec::with_capacity(table_bytes);
    for [ptr, len] in &pairs {
        raw.extend_from_slice(&ptr.to_le_bytes());
        raw.extend_from_slice(&len.to_le_bytes());
    }
    copy_into(root, argv, &raw)?;
    sp -= 8;
    copy_into(root, sp, &0u64.to_le_bytes())?;
    Ok((sp, argv))
}

/// Yeni bir adres alanına yüklenmiş, ring 3'e girmeye hazır program.
pub struct Image {
    pub root: u64,
    pub entry: u64,
    pub stack: u64,
    pub argc: u64,
    pub argv: u64,
}

/// Programı yeni bir adres alanına yükler; hata olursa alan serbest bırakılır.
pub fn load(path: &str, args: &[String]) -> Result<Image, ExecError> {
    let meta = fs::stat(path).map_err(ExecError::Fs)?;
    if meta.kind != fs::Kind::File {
        return Err(ExecError::Fs(FsError::IsDirectory));
    }
    if meta.size as usize > MAX_IMAGE {
        return Err(ExecError::TooLarge);
    }
    let image = fs::read_all(path).map_err(ExecError::Fs)?;
    // `#!yorumlayıcı`: betik ve .banna dosyaları yorumlayıcıyla çalışır (tek düzey).
    if let Some(rest) = image.strip_prefix(b"#!") {
        let line_end = rest
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(rest.len())
            .min(255);
        let line = core::str::from_utf8(&rest[..line_end]).map_err(|_| ExecError::NotElf)?;
        let mut words = line.split_whitespace();
        let interpreter = words.next().ok_or(ExecError::NotElf)?;
        if interpreter.as_bytes().first() != Some(&b'/') || path == interpreter {
            return Err(ExecError::NotElf);
        }
        let mut new_args = alloc::vec![String::from(interpreter)];
        new_args.extend(words.map(String::from));
        new_args.push(String::from(path));
        new_args.extend(args.iter().skip(1).cloned());
        let image = fs::read_all(interpreter).map_err(ExecError::Fs)?;
        if image.starts_with(b"#!") {
            return Err(ExecError::NotElf);
        }
        return load(interpreter, &new_args);
    }
    let (entry, segments) = parse(&image)?;
    let root = paging::create_address_space().ok_or(ExecError::OutOfMemory)?;
    match populate(root, &image, &segments, args) {
        Ok((stack, argv)) => Ok(Image {
            root,
            entry,
            stack,
            argc: args.len() as u64,
            argv,
        }),
        Err(e) => {
            paging::destroy_address_space(root);
            Err(e)
        }
    }
}

fn populate(
    root: u64,
    image: &[u8],
    segments: &[Segment],
    args: &[String],
) -> Result<(u64, u64), ExecError> {
    for segment in segments {
        let mut flags = 0;
        if segment.flags & PF_W != 0 {
            flags |= WRITABLE;
        }
        if segment.flags & PF_X == 0 {
            flags |= NO_EXECUTE;
        }
        map_range(root, segment.vaddr, segment.vaddr + segment.mem_size, flags)?;
        let data = &image[segment.offset as usize..(segment.offset + segment.file_size) as usize];
        copy_into(root, segment.vaddr, data)?;
    }
    map_range(
        root,
        STACK_TOP - STACK_PAGES * PAGE_SIZE,
        STACK_TOP,
        WRITABLE | NO_EXECUTE,
    )?;
    build_stack(root, args)
}

pub fn spawn(path: &str, args: &[String]) -> Result<(Arc<Process>, Arc<Completion>), ExecError> {
    spawn_with_uid(path, args, None)
}

/// Sistem servisi olarak (ör. root) başlatır; `uid` yoksa oturum kullanıcısı.
pub fn spawn_with_uid(
    path: &str,
    args: &[String],
    uid: Option<u32>,
) -> Result<(Arc<Process>, Arc<Completion>), ExecError> {
    let image = load(path, args)?;
    let name = path.rsplit('/').next().unwrap_or(path);
    let process = Process::with_root(name, image.root);
    if let Some(uid) = uid {
        process.set_uid(uid);
    }
    let (_, completion) = sched::spawn_user(
        process.clone(),
        UserStart::Enter {
            entry: image.entry,
            stack: image.stack,
            arg0: image.argc,
            arg1: image.argv,
        },
        None,
    )
    .ok_or(ExecError::OutOfMemory)?;
    sched::process::set_completion(process.pid, completion.clone());
    crate::log!(
        "exec: {} as pid {} (entry {:#x})",
        path,
        process.pid,
        image.entry
    );
    Ok((process, completion))
}
