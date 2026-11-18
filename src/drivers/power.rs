//! Kapatma, yeniden başlatma ve ACPI olayları: güç düğmesi (SCI), kapak, ekran
//! parlaklığı (ACPI _BCL/_BCM) ve aşırı ısınma koruması (`acpid` thread'i).

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::cpu::{self, inw, outb, outw};
use crate::kernel::initcall::Detail;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::sched::WaitQueue;

use super::acpi;

const SLP_EN: u16 = 1 << 13;

pub fn shutdown() -> ! {
    crate::log!("power: shutting down");
    crate::storage::nvme::shutdown();
    let sleep_type = crate::aml::sleep_type(5).or_else(|| acpi::s5().map(|s| (s.a, s.b)));
    if sleep_type.is_some() {
        crate::aml::prepare_to_sleep(5);
    }
    let efi = EfiReset::prepare();
    cpu::disable_interrupts();

    if let (Some(fadt), Some((a, b))) = (acpi::fadt(), sleep_type) {
        unsafe {
            if fadt.pm1a_control != 0 {
                outw(fadt.pm1a_control as u16, ((a as u16) << 10) | SLP_EN);
            }
            if fadt.pm1b_control != 0 {
                outw(fadt.pm1b_control as u16, ((b as u16) << 10) | SLP_EN);
            }
        }
        for _ in 0..10_000_000 {
            core::hint::spin_loop();
        }
        crate::log!("power: ACPI S5 did not power off");
    }
    if let Some(efi) = efi {
        efi.call(EFI_RESET_SHUTDOWN);
    }

    unsafe {
        outw(0x604, 0x2000);
        outw(0xb004, 0x2000);
    }
    crate::log::emergency(format_args!(
        "\nIt is now safe to turn off your computer.\n"
    ));
    cpu::halt_forever()
}

pub fn reboot() -> ! {
    crate::log!("power: rebooting");
    crate::storage::nvme::shutdown();
    // Bellek alanındaki sıfırlama yazmacı kesmeler kapanmadan eşlenir (sayfa tablosu kilidi).
    let reset = acpi::fadt().and_then(|fadt| {
        let reg = fadt.reset_register?;
        let target = match reg.space {
            0 => ResetTarget::Memory(map_physical(reg.address, 1, NO_CACHE).ok()?),
            1 => ResetTarget::Port(reg.address as u16),
            _ => return None,
        };
        Some((target, fadt.reset_value))
    });
    let efi = EfiReset::prepare();
    cpu::disable_interrupts();

    if let Some((target, value)) = reset {
        match target {
            ResetTarget::Memory(ptr) => unsafe { ptr.write_volatile(value) },
            ResetTarget::Port(port) => unsafe { outb(port, value) },
        }
        wait_ms(100);
    }

    // UEFI firmware'in kendi yeniden başlatması (modern PC'lerde en güvenilir yol).
    if let Some(efi) = efi {
        efi.call(EFI_RESET_COLD);
    }

    unsafe {
        outb(0xcf9, 0x02);
        wait_ms(1);
        outb(0xcf9, 0x06);
    }
    wait_ms(100);

    for _ in 0..100_000 {
        if unsafe { cpu::inb(0x64) } & 0x02 == 0 {
            break;
        }
    }
    unsafe { outb(0x64, 0xfe) };
    wait_ms(100);

    let empty = [0u64; 2];
    unsafe {
        core::arch::asm!("lidt [{}]", "int3", in(reg) empty.as_ptr(), options(noreturn));
    }
}

const EFI_RESET_COLD: u32 = 0;
const EFI_RESET_SHUTDOWN: u32 = 2;

enum ResetTarget {
    Memory(*mut u8),
    Port(u16),
}

fn wait_ms(ms: u64) {
    crate::arch::tsc::busy_wait_us(ms * 1000);
}

/// UEFI çalışma zamanı `ResetSystem`: firmware bölgeleri fiziksel adresleriyle yeni bir
/// sayfa tablosunda kimlik eşlenir (çekirdeğin üst yarısı aynen kalır).
struct EfiReset {
    root: u64,
    function: u64,
}

impl EfiReset {
    const RUNTIME: u64 = 1 << 63;
    /// Kimlik eşlenecek en çok sayfa (4 GiB).
    const PAGE_BUDGET: u64 = 1 << 20;

    fn prepare() -> Option<Self> {
        use crate::memory::{hhdm_offset, paging, phys_to_virt, pmm};
        let table = crate::boot::EFI_SYSTEM_TABLE.response()?;
        let map = crate::boot::EFI_MEMMAP.response()?;
        let hhdm = hhdm_offset();
        let phys = |a: u64| if a >= hhdm { a - hhdm } else { a };
        // Firmware bölgeleri HHDM'de olmayabilir (Limine yalnızca RAM'i eşler).
        let read = |at: u64| {
            map_physical(at, 8, 0)
                .ok()
                .map(|p| unsafe { (p as *const u64).read_unaligned() })
                .unwrap_or(0)
        };
        let system_table = phys(table.address);
        if system_table == 0 || map.memmap.is_null() || map.desc_size < 40 {
            return None;
        }
        // EFI_SYSTEM_TABLE.RuntimeServices (+88) → ResetSystem (+104).
        let runtime = phys(read(system_table + 88));
        if runtime == 0 {
            return None;
        }
        let function = read(runtime + 104);
        if function == 0 {
            return None;
        }
        let root = pmm::alloc_zeroed_frame()?;
        unsafe {
            let kernel = phys_to_virt(paging::kernel_root()) as *const u64;
            let new = phys_to_virt(root) as *mut u64;
            for i in 256..512 {
                new.add(i).write(kernel.add(i).read());
            }
        }
        let memmap = phys(map.memmap as u64);
        let mut budget = Self::PAGE_BUDGET;
        let mut covered = false;
        for i in 0..map.memmap_size / map.desc_size {
            let at = memmap + i * map.desc_size;
            let kind = read(at) as u32;
            let (start, pages, attribute) = (read(at + 8), read(at + 24), read(at + 32));
            if attribute & Self::RUNTIME == 0 || start >= 1 << 47 {
                continue;
            }
            // 11/12: bellek eşlemeli G/Ç (ör. SPI flaş, sıfırlama yazmacı).
            let flags = if kind == 11 || kind == 12 {
                paging::WRITABLE | NO_CACHE | paging::NO_EXECUTE
            } else {
                paging::WRITABLE
            };
            let count = pages.min(budget);
            for p in 0..count {
                let address = start + p * 4096;
                let _ = paging::map_page_in(root, address, address, flags);
                covered |= (address..address + 4096).contains(&function);
            }
            budget -= count;
        }
        if !covered {
            crate::log!(
                "power: EFI ResetSystem {:#x} outside runtime regions",
                function
            );
            return None;
        }
        Some(Self { root, function })
    }

    fn call(&self, kind: u32) {
        let reset: extern "efiapi" fn(u32, usize, usize, *const u8) =
            unsafe { core::mem::transmute(self.function as usize) };
        let previous = cpu::read_cr3();
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) self.root, options(nostack)) };
        // Dönerse çekirdek tablosuna geri geçilir.
        reset(kind, 0, 0, core::ptr::null());
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) previous, options(nostack)) };
    }
}

const PWRBTN: u16 = 1 << 8;
/// Bu sıcaklıkta (°C) sistem kendini korumak için kapanır.
const CRITICAL_CELSIUS: i32 = 100;

static POWER_BUTTON: AtomicBool = AtomicBool::new(false);
static EVENTS: WaitQueue = WaitQueue::new();
static PM1_EVENT: AtomicU32 = AtomicU32::new(0);
static BRIGHTNESS: AtomicU32 = AtomicU32::new(100);
static LID_OPEN: AtomicBool = AtomicBool::new(true);
/// Kapak kapanınca: 0 ekranı kapat, 1 hiçbir şey, 2 bilgisayarı kapat.
static LID_ACTION: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
/// Güç düğmesi: 0 kapat, 1 hiçbir şey.
static POWER_BUTTON_ACTION: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

pub fn set_lid_action(action: u8) {
    LID_ACTION.store(action, Ordering::Relaxed);
}

pub fn set_power_button_action(action: u8) {
    POWER_BUTTON_ACTION.store(action, Ordering::Relaxed);
}

fn on_sci() {
    let block = PM1_EVENT.load(Ordering::Relaxed) as u16;
    if block == 0 {
        return;
    }
    let status = unsafe { inw(block) };
    if status & PWRBTN != 0 {
        unsafe { outw(block, PWRBTN) };
        POWER_BUTTON.store(true, Ordering::Release);
        EVENTS.wake_all();
    }
}

/// PM1 sabit olaylarından güç düğmesini açar; GPE'ler kapatılır (kapak, AC ve batarya
/// yoklamayla izlenir).
fn enable_events(fadt: &acpi::Fadt) -> bool {
    if fadt.flags & acpi::Fadt::FLAG_HW_REDUCED != 0
        || fadt.pm1a_event == 0
        || fadt.pm1_event_len < 4
        || fadt.sci_interrupt == 0
    {
        return false;
    }
    let half = (fadt.pm1_event_len / 2) as u16;
    let block = fadt.pm1a_event as u16;
    unsafe {
        if fadt.gpe0_block != 0 {
            let gpe_half = (fadt.gpe0_len / 2) as u16;
            for i in 0..gpe_half {
                outb(fadt.gpe0_block as u16 + gpe_half + i, 0);
                outb(fadt.gpe0_block as u16 + i, 0xff);
            }
        }
        outw(block, 0xffff);
        outw(block + half, PWRBTN);
    }
    PM1_EVENT.store(block as u32, Ordering::Relaxed);
    if let Ok(irq) = u8::try_from(fadt.sci_interrupt) {
        crate::arch::idt::register_irq(irq, on_sci);
    }
    true
}

// ---- ekran parlaklığı (ACPI video) ----

/// Parlaklık destekli ekran düğümü ve geçerli seviyeleri (_BCL'nin ilk ikisi hariç).
fn backlight() -> Option<(crate::aml::NodeId, Vec<u64>)> {
    for node in crate::aml::find_parents_with("_BCM") {
        if let Some(Ok(value)) = crate::aml::evaluate_child(node, "_BCL", Vec::new())
            && let Some(items) = value.as_package()
        {
            let mut levels: Vec<u64> = items
                .iter()
                .skip(2)
                .filter_map(|v| v.as_integer())
                .collect();
            levels.sort_unstable();
            levels.dedup();
            if !levels.is_empty() {
                return Some((node, levels));
            }
        }
    }
    None
}

pub fn brightness() -> Option<u32> {
    if super::backlight::available() {
        return Some(BRIGHTNESS.load(Ordering::Relaxed));
    }
    backlight().map(|_| BRIGHTNESS.load(Ordering::Relaxed))
}

/// Ekran parlaklığını yüzde olarak ayarlar; parlaklık denetimi yoksa `false`.
/// Intel'de ACPI _BCM yalnızca grafik sürücüsüne (OpRegion ASLE) haber verir, o yüzden
/// doğrudan PWM önce gelir.
pub fn set_brightness(percent: u32) -> bool {
    let percent = percent.min(100);
    if super::backlight::set(percent) {
        BRIGHTNESS.store(percent, Ordering::Relaxed);
        return true;
    }
    let Some((node, levels)) = backlight() else {
        return false;
    };
    let index = ((levels.len() - 1) as u32 * percent).div_ceil(100) as usize;
    let level = levels[index.min(levels.len() - 1)];
    let ok =
        crate::aml::evaluate_child(node, "_BCM", alloc::vec![crate::aml::Value::Integer(level)])
            .is_some_and(|r| r.is_ok());
    if ok {
        BRIGHTNESS.store(percent, Ordering::Relaxed);
    }
    ok
}

pub fn lid_open() -> Option<bool> {
    let node = *crate::aml::find_devices("PNP0C0D").first()?;
    let value = crate::aml::evaluate_child(node, "_LID", Vec::new())?.ok()?;
    Some(value.as_integer()? != 0)
}

/// EC sorgularını işler (`_Qxx`); ekran parlaklık bildirimleri (0x86/0x87) uygulanır.
fn ec_events(ec: crate::aml::NodeId) {
    for _ in 0..8 {
        let Some(query) = super::ec::query() else {
            break;
        };
        let method = alloc::format!("_Q{:02X}", query);
        let _ = crate::aml::evaluate_child(ec, &method, Vec::new());
    }
    for (_, value) in crate::aml::take_notifications() {
        let step: i32 = match value {
            0x86 => 10,
            0x87 => -10,
            _ => continue,
        };
        if let Some(current) = brightness() {
            let next = (current as i32 + step).clamp(5, 100) as u32;
            set_brightness(next);
            crate::log!("power: brightness {}%", next);
        }
    }
}

/// Güç düğmesi, kapak ve sıcaklığı izler.
fn acpid() -> i64 {
    let has_lid = lid_open().is_some();
    let mut saved_brightness = BRIGHTNESS.load(Ordering::Relaxed);
    let embedded = if crate::boot::has_flag("noec") {
        None
    } else {
        crate::aml::find_devices("PNP0C09").first().copied()
    };
    // EC varsa olayları (Fn tuşları) sık yoklanır; diğer denetimler saniyede bir.
    let interval = if embedded.is_some() && super::ec::present() {
        50
    } else {
        1000
    };
    let mut next_check = 0;
    loop {
        EVENTS.wait_timeout(|| POWER_BUTTON.load(Ordering::Acquire), interval);
        if let Some(ec) = embedded {
            ec_events(ec);
        }
        if POWER_BUTTON.swap(false, Ordering::AcqRel) {
            crate::log!("power: power button pressed");
            if POWER_BUTTON_ACTION.load(Ordering::Relaxed) == 0 {
                shutdown();
            }
        }
        let now = crate::arch::pit::uptime_ms();
        if now < next_check && !POWER_BUTTON.load(Ordering::Relaxed) {
            continue;
        }
        next_check = now + 1000;
        if has_lid && let Some(open) = lid_open() {
            let was = LID_OPEN.swap(open, Ordering::Relaxed);
            if was && !open {
                crate::log!("power: lid closed");
                match LID_ACTION.load(Ordering::Relaxed) {
                    0 => {
                        saved_brightness = BRIGHTNESS.load(Ordering::Relaxed);
                        set_brightness(0);
                    }
                    2 => shutdown(),
                    _ => {}
                }
            } else if !was && open {
                crate::log!("power: lid opened");
                if LID_ACTION.load(Ordering::Relaxed) == 0 {
                    set_brightness(saved_brightness);
                }
            }
        }
        if let Some(t) = super::cpupower::temperature()
            && t >= CRITICAL_CELSIUS
        {
            crate::log!(
                "power: CPU at {} C, shutting down to protect the hardware",
                t
            );
            shutdown();
        }
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let events = acpi::fadt().is_some_and(|f| enable_events(&f));
    crate::sched::spawn_detached("acpid", acpid).ok_or("could not start acpid")?;
    let s5 = crate::aml::sleep_type(5).is_some() || acpi::s5().is_some();
    let reset = acpi::fadt().is_some_and(|f| f.reset_register.is_some());
    let efi = crate::boot::EFI_SYSTEM_TABLE.response().is_some();
    let _ = write!(
        detail,
        "shutdown: {}, reboot: {}{}",
        if s5 { "ACPI S5" } else { "fallback" },
        if reset { "ACPI reset" } else { "CF9/8042" },
        if efi { " + UEFI" } else { "" }
    );
    if events {
        let _ = write!(detail, ", power button");
    }
    if lid_open().is_some() {
        let _ = write!(detail, ", lid");
    }
    if let Some(native) = super::backlight::probe() {
        BRIGHTNESS.store(super::backlight::get().unwrap_or(100), Ordering::Relaxed);
        let _ = write!(detail, ", backlight ({})", native);
    } else if backlight().is_some() {
        let _ = write!(detail, ", backlight (ACPI)");
    }
    Ok(())
}

crate::initcall!(driver, 40, "Power management", init);
