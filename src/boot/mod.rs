//! Önyükleyiciden gelen istek/yanıtlar.

pub mod limine;

use limine::*;

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static START_MARKER: StartMarker = StartMarker::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static BASE_REVISION: BaseRevision = BaseRevision::new(3);

#[used]
#[unsafe(link_section = ".requests")]
pub static STACK_SIZE: StackSizeRequest = StackSizeRequest::new(256 * 1024);

#[used]
#[unsafe(link_section = ".requests")]
pub static HHDM: HhdmRequest = HhdmRequest::hhdm();

#[used]
#[unsafe(link_section = ".requests")]
pub static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::framebuffer();

#[used]
#[unsafe(link_section = ".requests")]
pub static MEMMAP: MemmapRequest = MemmapRequest::memmap();

#[used]
#[unsafe(link_section = ".requests")]
pub static EXECUTABLE_ADDRESS: ExecutableAddressRequest =
    ExecutableAddressRequest::executable_address();

#[used]
#[unsafe(link_section = ".requests")]
pub static RSDP: RsdpRequest = RsdpRequest::rsdp();

#[used]
#[unsafe(link_section = ".requests")]
pub static MP: MpRequest = MpRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
pub static EXECUTABLE_FILE: ExecutableFileRequest = ExecutableFileRequest::executable_file();

#[used]
#[unsafe(link_section = ".requests")]
pub static EFI_SYSTEM_TABLE: EfiSystemTableRequest = EfiSystemTableRequest::efi_system_table();

#[used]
#[unsafe(link_section = ".requests")]
pub static EFI_MEMMAP: EfiMemmapRequest = EfiMemmapRequest::efi_memmap();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static END_MARKER: EndMarker = EndMarker::new();

/// Çekirdek komut satırı (limine.conf `cmdline:`).
pub fn cmdline() -> &'static str {
    let Some(response) = EXECUTABLE_FILE.response() else {
        return "";
    };
    unsafe {
        let Some(file) = response.file.as_ref() else {
            return "";
        };
        if file.cmdline.is_null() {
            return "";
        }
        let mut len = 0;
        while *file.cmdline.add(len) != 0 && len < 4096 {
            len += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(file.cmdline, len)).unwrap_or("")
    }
}

pub fn has_flag(flag: &str) -> bool {
    cmdline().split_whitespace().any(|word| word == flag)
}
