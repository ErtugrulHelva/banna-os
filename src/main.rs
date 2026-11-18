#![no_std]
#![no_main]

//! Banna OS çekirdek giriş noktası.

extern crate alloc;

#[macro_use]
mod log;
#[macro_use]
mod kernel;

mod aml;
mod arch;
mod boot;
mod crypto;
mod drivers;
mod fs;
mod gfx;
mod gui;
mod memory;
mod net;
mod sched;
mod storage;
mod sync;

use core::panic::PanicInfo;

use arch::cpu;
use drivers::console;
use drivers::framebuffer::Framebuffer;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    let serial_ok = drivers::serial::init();
    log!(
        "\nBanna OS {} booting (serial: {})",
        VERSION,
        if serial_ok { "ok" } else { "missing" }
    );

    if !boot::BASE_REVISION.is_supported() {
        log!("boot: Limine base revision 3 is not supported by this bootloader");
        cpu::halt_forever();
    }
    let Some(hhdm) = boot::HHDM.response() else {
        log!("boot: no HHDM response");
        cpu::halt_forever();
    };
    memory::set_hhdm_offset(hhdm.offset);

    let framebuffer = boot::FRAMEBUFFER
        .response()
        .and_then(|r| r.framebuffers().next())
        .and_then(Framebuffer::from_limine);
    let has_screen = framebuffer.is_some();
    if let Some(fb) = framebuffer {
        console::init(fb);
    }
    let text_console = boot::has_flag("console");
    if has_screen && !text_console {
        gfx::splash::begin();
    }

    banner();
    if let Some((w, h)) = console::CONSOLE.lock().size() {
        log!("console: framebuffer {}x{}", w, h);
    }
    if has_screen {
        log::ok(format_args!("Framebuffer console"));
    } else {
        log!("console: no usable 32 bpp framebuffer, serial only");
    }
    if boot::STACK_SIZE.response().is_none() {
        log!("boot: stack size request was not honoured");
    }
    if let Some(exe) = boot::EXECUTABLE_ADDRESS.response() {
        log!(
            "boot: kernel phys {:#x} -> virt {:#x}",
            exe.physical_base,
            exe.virtual_base
        );
    }
    log!("boot: HHDM offset {:#x}", hhdm.offset);

    let healthy = kernel::initcall::run_all();

    let mem = memory::pmm::stats();
    println!();
    if healthy {
        log::ok(format_args!("All self-tests passed"));
    } else {
        log::fail(format_args!("Some subsystems failed (see serial log)"));
    }
    console::set_color(console::GRAY);
    println!(
        "{} MiB free of {} MiB, booted in {} ms",
        mem.free_bytes / (1024 * 1024),
        mem.total_bytes / (1024 * 1024),
        arch::pit::uptime_ms()
    );
    console::set_color(console::WHITE);
    log!("boot: complete (cmdline '{}')", boot::cmdline());
    if fs::stat("/boot/boot").is_ok() && log::save_boot_log("/boot/boot/kernel.log") {
        log!("boot: log saved to /boot/boot/kernel.log");
    }

    kernel::watchdog::start();
    let recovery = kernel::recovery::detect();
    if !recovery {
        kernel::update::apply_pending();
        kernel::services::start();
    }
    gfx::splash::finish();
    if recovery {
        console::set_muted(false);
        console::attach_display();
    } else if !text_console && let Err(reason) = gui::start() {
        log!(
            "boot: desktop unavailable ({}), using the text console",
            reason
        );
        console::set_muted(false);
        console::attach_display();
    }
    kernel::shell::run()
}

fn banner() {
    console::set_color(console::CYAN);
    println!("Banna OS {}", VERSION);
    console::set_color(console::GRAY);
    println!("x86_64 / Rust / Limine");
    console::set_color(console::WHITE);
    println!();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    cpu::disable_interrupts();
    log::emergency(format_args!("\nKERNEL PANIC: {}\n", info));
    kernel::crash::raise(
        kernel::crash::Report {
            kind: "panic",
            title: alloc::string::String::from("Çekirdek paniği"),
            details: alloc::vec![alloc::format!("{}", info)],
        },
        true,
    );
    cpu::halt_forever()
}
