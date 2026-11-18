//! Kullanılan UEFI yapıları ve protokolleri (yalnızca gereken alanlar).

use core::ffi::c_void;

pub type Handle = *mut c_void;
pub type Status = usize;
pub type Event = *mut c_void;

pub const SUCCESS: Status = 0;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

pub const LOADED_IMAGE: Guid = Guid(
    0x5b1b31a1,
    0x9562,
    0x11d2,
    [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const SIMPLE_FILE_SYSTEM: Guid = Guid(
    0x964e5b22,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const BLOCK_IO: Guid = Guid(
    0x964e5b21,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const DEVICE_PATH: Guid = Guid(
    0x09576e91,
    0x6d3f,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
pub const GRAPHICS_OUTPUT: Guid = Guid(
    0x9042a9de,
    0x23dc,
    0x4a38,
    [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
);
pub const EDID_ACTIVE: Guid = Guid(
    0xbd8c1056,
    0x9f36,
    0x44ec,
    [0x92, 0xa8, 0xa6, 0x33, 0x7f, 0x81, 0x79, 0x86],
);
pub const EDID_DISCOVERED: Guid = Guid(
    0x1c0c34f6,
    0xd380,
    0x41fa,
    [0xa0, 0x49, 0x8a, 0xd0, 0x6c, 0x1a, 0x66, 0xaa],
);

#[repr(C)]
pub struct EdidProtocol {
    pub size: u32,
    pub edid: *const u8,
}

pub const ACPI_20_TABLE: Guid = Guid(
    0x8868e871,
    0xe4f1,
    0x11d3,
    [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
);
pub const ACPI_10_TABLE: Guid = Guid(
    0xeb9d2d30,
    0x2d88,
    0x11d3,
    [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
);

#[repr(C)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut SimpleTextInput,
    pub console_out_handle: Handle,
    pub con_out: *mut SimpleTextOutput,
    pub standard_error_handle: Handle,
    pub std_err: *mut SimpleTextOutput,
    pub runtime_services: *mut c_void,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *const ConfigurationTable,
}

#[repr(C)]
pub struct ConfigurationTable {
    pub vendor_guid: Guid,
    pub vendor_table: *mut c_void,
}

#[repr(C)]
pub struct InputKey {
    pub scan_code: u16,
    pub unicode_char: u16,
}

#[repr(C)]
pub struct SimpleTextInput {
    pub reset: extern "efiapi" fn(*mut SimpleTextInput, bool) -> Status,
    pub read_key_stroke: extern "efiapi" fn(*mut SimpleTextInput, *mut InputKey) -> Status,
    pub wait_for_key: Event,
}

#[repr(C)]
pub struct SimpleTextOutput {
    pub reset: extern "efiapi" fn(*mut SimpleTextOutput, bool) -> Status,
    pub output_string: extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
    pub test_string: usize,
    pub query_mode: usize,
    pub set_mode: usize,
    pub set_attribute: extern "efiapi" fn(*mut SimpleTextOutput, usize) -> Status,
    pub clear_screen: extern "efiapi" fn(*mut SimpleTextOutput) -> Status,
    pub set_cursor_position: extern "efiapi" fn(*mut SimpleTextOutput, usize, usize) -> Status,
    pub enable_cursor: extern "efiapi" fn(*mut SimpleTextOutput, bool) -> Status,
    pub mode: *mut c_void,
}

#[repr(C)]
pub struct MemoryDescriptor {
    pub kind: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

pub const ALLOCATE_ANY_PAGES: u32 = 0;
pub const LOADER_DATA: u32 = 2;
/// Çekirdeğe devredilen ayırmaların etiketleri. Firmware'e LOADER_DATA olarak istenir:
/// bazı gerçek firmware'ler 0x80000000 üstü (işletim sistemi) bellek tiplerinde takılır.
pub const KERNEL_MEMORY: u32 = 0x8000_0001;
pub const HANDOFF_MEMORY: u32 = 0x8000_0002;
pub const BY_PROTOCOL: u32 = 2;
pub const EVT_TIMER: u32 = 0x8000_0000;
pub const TIMER_RELATIVE: u32 = 2;

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    pub raise_tpl: usize,
    pub restore_tpl: usize,
    pub allocate_pages: extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map: extern "efiapi" fn(
        *mut usize,
        *mut MemoryDescriptor,
        *mut usize,
        *mut usize,
        *mut u32,
    ) -> Status,
    pub allocate_pool: extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    pub free_pool: extern "efiapi" fn(*mut u8) -> Status,
    pub create_event:
        extern "efiapi" fn(u32, usize, *const c_void, *const c_void, *mut Event) -> Status,
    pub set_timer: extern "efiapi" fn(Event, u32, u64) -> Status,
    pub wait_for_event: extern "efiapi" fn(usize, *const Event, *mut usize) -> Status,
    pub signal_event: usize,
    pub close_event: extern "efiapi" fn(Event) -> Status,
    pub check_event: usize,
    pub install_protocol_interface: usize,
    pub reinstall_protocol_interface: usize,
    pub uninstall_protocol_interface: usize,
    pub handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: usize,
    pub register_protocol_notify: usize,
    pub locate_handle: usize,
    pub locate_device_path: usize,
    pub install_configuration_table: usize,
    pub load_image: usize,
    pub start_image: usize,
    pub exit: usize,
    pub unload_image: usize,
    pub exit_boot_services: extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: usize,
    pub stall: extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    pub connect_controller: usize,
    pub disconnect_controller: usize,
    pub open_protocol: usize,
    pub close_protocol: usize,
    pub open_protocol_information: usize,
    pub protocols_per_handle: usize,
    pub locate_handle_buffer:
        extern "efiapi" fn(u32, *const Guid, *const c_void, *mut usize, *mut *mut Handle) -> Status,
    pub locate_protocol: extern "efiapi" fn(*const Guid, *const c_void, *mut *mut c_void) -> Status,
}

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *mut c_void,
    pub reserved: *mut c_void,
    pub load_options_size: u32,
    pub load_options: *mut c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
}

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

pub const FILE_MODE_READ: u64 = 1;
pub const FILE_MODE_WRITE: u64 = 2;
pub const FILE_MODE_CREATE: u64 = 0x8000_0000_0000_0000;

#[repr(C)]
pub struct FileProtocol {
    pub revision: u64,
    pub open: extern "efiapi" fn(
        *mut FileProtocol,
        *mut *mut FileProtocol,
        *const u16,
        u64,
        u64,
    ) -> Status,
    pub close: extern "efiapi" fn(*mut FileProtocol) -> Status,
    pub delete: extern "efiapi" fn(*mut FileProtocol) -> Status,
    pub read: extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut u8) -> Status,
    pub write: extern "efiapi" fn(*mut FileProtocol, *mut usize, *const u8) -> Status,
    pub get_position: usize,
    pub set_position: extern "efiapi" fn(*mut FileProtocol, u64) -> Status,
    pub get_info: usize,
    pub set_info: usize,
    pub flush: extern "efiapi" fn(*mut FileProtocol) -> Status,
}

#[repr(C)]
pub struct BlockIoMedia {
    pub media_id: u32,
    pub removable_media: u8,
    pub media_present: u8,
    pub logical_partition: u8,
    pub read_only: u8,
    pub write_caching: u8,
    pub block_size: u32,
    pub io_align: u32,
    pub last_block: u64,
}

#[repr(C)]
pub struct BlockIo {
    pub revision: u64,
    pub media: *mut BlockIoMedia,
    pub reset: usize,
    pub read_blocks: extern "efiapi" fn(*mut BlockIo, u32, u64, usize, *mut u8) -> Status,
}

#[repr(C)]
pub struct DevicePathNode {
    pub kind: u8,
    pub sub_kind: u8,
    pub length: [u8; 2],
}

#[repr(C)]
pub struct PixelBitmask {
    pub red: u32,
    pub green: u32,
    pub blue: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct ModeInfo {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    pub pixel_format: u32,
    pub pixel_information: PixelBitmask,
    pub pixels_per_scan_line: u32,
}

#[repr(C)]
pub struct GopMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut ModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode:
        extern "efiapi" fn(*mut GraphicsOutput, u32, *mut usize, *mut *mut ModeInfo) -> Status,
    pub set_mode: extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    pub blt: usize,
    pub mode: *mut GopMode,
}
