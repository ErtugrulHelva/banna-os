//! Seviyeli başlatma çağrıları.
//!
//! Her alt sistem `initcall!` ile kendini `.initcall.<seviye>.<öncelik>` bölümüne
//! kaydeder; linker bu bölümleri ada göre sıralar, böylece çağrılar seviye ve
//! öncelik sırasıyla çalışır. `early`/`arch`/`core` başarısızlığı ölümcüldür.

use core::fmt;

use crate::arch::cpu;
use crate::log;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Early = 0,
    Arch = 1,
    Core = 2,
    Driver = 3,
    Late = 4,
}

impl Level {
    fn is_fatal(self) -> bool {
        self <= Level::Core
    }
}

pub type InitFn = fn(&mut Detail) -> Result<(), &'static str>;

#[repr(C)]
pub struct InitCall {
    pub level: Level,
    pub name: &'static str,
    pub func: InitFn,
}

pub struct Detail {
    buf: [u8; 80],
    len: usize,
}

impl Detail {
    const fn new() -> Self {
        Self {
            buf: [0; 80],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for Detail {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let room = self.buf.len() - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

#[macro_export]
macro_rules! initcall {
    (@section early, $prio:literal) => {
        concat!(".initcall.0.", stringify!($prio))
    };
    (@section arch, $prio:literal) => {
        concat!(".initcall.1.", stringify!($prio))
    };
    (@section core, $prio:literal) => {
        concat!(".initcall.2.", stringify!($prio))
    };
    (@section driver, $prio:literal) => {
        concat!(".initcall.3.", stringify!($prio))
    };
    (@section late, $prio:literal) => {
        concat!(".initcall.4.", stringify!($prio))
    };
    (@level early) => {
        $crate::kernel::initcall::Level::Early
    };
    (@level arch) => {
        $crate::kernel::initcall::Level::Arch
    };
    (@level core) => {
        $crate::kernel::initcall::Level::Core
    };
    (@level driver) => {
        $crate::kernel::initcall::Level::Driver
    };
    (@level late) => {
        $crate::kernel::initcall::Level::Late
    };
    ($level:ident, $prio:literal, $name:literal, $func:path) => {
        const _: () = {
            #[used]
            #[unsafe(link_section = $crate::initcall!(@section $level, $prio))]
            static ENTRY: $crate::kernel::initcall::InitCall = $crate::kernel::initcall::InitCall {
        level: $crate::initcall!(@level $level),
                name: $name,
                func: $func,
            };
        };
    };
}

unsafe extern "C" {
    static __initcall_start: u8;
    static __initcall_end: u8;
}

fn table() -> &'static [InitCall] {
    unsafe {
        let start = &raw const __initcall_start as *const InitCall;
        let end = &raw const __initcall_end as *const InitCall;
        let count = (end as usize - start as usize) / size_of::<InitCall>();
        core::slice::from_raw_parts(start, count)
    }
}

pub fn run_all() -> bool {
    let calls = table();
    crate::log!("initcall: {} entries", calls.len());
    let mut healthy = true;
    let mut previous = Level::Early;
    for (index, call) in calls.iter().enumerate() {
        crate::gfx::splash::progress(index, calls.len());
        crate::gfx::splash::set_step(call.name);
        if call.level < previous {
            crate::log!("initcall: '{}' is out of order", call.name);
        }
        previous = call.level;

        let mut detail = Detail::new();
        let result = (call.func)(&mut detail);
        let detail = detail.as_str();
        match (result, detail.is_empty()) {
            (Ok(()), true) => log::ok(format_args!("{}", call.name)),
            (Ok(()), false) => log::ok(format_args!("{} ({})", call.name, detail)),
            (Err(reason), _) => {
                log::fail(format_args!("{}: {}", call.name, reason));
                healthy = false;
                if call.level.is_fatal() {
                    log::emergency(format_args!(
                        "\nboot halted: {:?}-level initcall '{}' failed\n",
                        call.level, call.name
                    ));
                    cpu::halt_forever();
                }
            }
        }
    }
    healthy
}

pub fn self_test(name: &str, passed: bool) -> Result<(), &'static str> {
    if passed {
        crate::log!("{}: self-test PASS", name);
        Ok(())
    } else {
        crate::log!("{}: self-test FAIL", name);
        Err("self-test failed")
    }
}
