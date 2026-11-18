//! Takılma bekçisi: önyükleme çekirdeğinin zamanlayıcı tiki ya da masaüstü döngüsü 3 sn
//! ilerlemezse Banna Hata Raporlama Sistemi'ni (`crash`) açar.
//!
//! Uyuyan thread'leri önyükleme çekirdeğinin tiki uyandırdığından bekçi uyumaz: TSC ile
//! bekler ve önyükleme çekirdeğinde çalışmaz. Takılan çekirdeklere NMI gönderilir; NMI
//! kesmeler kapalıyken de gelir ve o anki RIP ile yığındaki çekirdek adresleri kaydedilir
//! (adresler `llvm-nm` ile fonksiyon adlarına çevrilir).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::arch::idt::InterruptFrame;
use crate::arch::{apic, smp, tsc};

const STALL_NS: u64 = 3_000_000_000;
const STACK_WORDS: usize = 24;
const KERNEL_TEXT: u64 = 0xffff_ffff_8000_0000;

/// Masaüstü döngüsünün son turu (TSC ns); 0 = masaüstü çalışmıyor.
static GUI_BEAT: AtomicU64 = AtomicU64::new(0);

static NMI_WANTED: AtomicBool = AtomicBool::new(false);
static NMI_DONE: AtomicBool = AtomicBool::new(false);
static NMI_RIP: AtomicU64 = AtomicU64::new(0);
static NMI_IF: AtomicBool = AtomicBool::new(false);
static NMI_STACK: [AtomicU64; STACK_WORDS] = [const { AtomicU64::new(0) }; STACK_WORDS];
static NMI_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn gui_beat() {
    GUI_BEAT.store(tsc::now_ns(), Ordering::Relaxed);
}

pub fn gui_stopped() {
    GUI_BEAT.store(0, Ordering::Relaxed);
}

/// IDT'nin NMI yolu: bekçi istediyse durumu kaydeder ve `true` döner.
pub fn nmi(frame: &InterruptFrame) -> bool {
    if !NMI_WANTED.swap(false, Ordering::AcqRel) {
        return false;
    }
    NMI_RIP.store(frame.rip, Ordering::Relaxed);
    NMI_IF.store(frame.rflags & (1 << 9) != 0, Ordering::Relaxed);
    let mut count = 0;
    if frame.cs & 3 == 0 {
        // Yığındaki dönüş adresi gibi görünen çekirdek kodu adresleri.
        let mut at = frame.rsp;
        for _ in 0..256 {
            if count == STACK_WORDS || at & 7 != 0 || at < 0xffff_8000_0000_0000 {
                break;
            }
            let word = unsafe { core::ptr::read_volatile(at as *const u64) };
            if (KERNEL_TEXT..KERNEL_TEXT + 0x400_0000).contains(&word) {
                NMI_STACK[count].store(word, Ordering::Relaxed);
                count += 1;
            }
            at += 8;
        }
    }
    NMI_COUNT.store(count, Ordering::Relaxed);
    NMI_DONE.store(true, Ordering::Release);
    true
}

/// `lapic` çekirdeğine NMI gönderip RIP ve yığın adreslerini toplar.
fn probe(lapic: u32) -> Option<(u64, bool, Vec<u64>)> {
    NMI_DONE.store(false, Ordering::Relaxed);
    NMI_WANTED.store(true, Ordering::Release);
    // Teslim kipi NMI (100b), assert.
    apic::send_raw(lapic, 4 << 8 | 1 << 14);
    let deadline = tsc::now_ns() + 100_000_000;
    while !NMI_DONE.load(Ordering::Acquire) {
        if tsc::now_ns() > deadline {
            NMI_WANTED.store(false, Ordering::Relaxed);
            return None;
        }
        core::hint::spin_loop();
    }
    let count = NMI_COUNT.load(Ordering::Relaxed);
    let stack = NMI_STACK[..count]
        .iter()
        .map(|w| w.load(Ordering::Relaxed))
        .collect();
    Some((
        NMI_RIP.load(Ordering::Relaxed),
        NMI_IF.load(Ordering::Relaxed),
        stack,
    ))
}

fn report(reason: &'static str) -> (&'static str, Vec<String>) {
    let mut lines = Vec::new();
    lines.push(format!("reason: {}", reason));
    lines.push(format!(
        "uptime {} ms, BSP ticks {}",
        tsc::now_ns() / 1_000_000,
        crate::sched::bsp_ticks()
    ));
    let stats = crate::sched::try_cpu_stats(200_000_000);
    if stats.is_none() {
        lines.push(String::from("scheduler lock is held (stuck holder)"));
    }
    let me = apic::lapic_id();
    let cpus: Vec<(usize, u32, String)> = match stats {
        Some(list) => list
            .into_iter()
            .map(|c| (c.index, c.lapic_id, c.current))
            .collect(),
        None => alloc::vec![(0, 0, String::from("?"))],
    };
    for (index, lapic, thread) in cpus {
        if lapic == me {
            lines.push(format!("cpu{:<2} watchdog", index));
            continue;
        }
        // Boşta olanlar yalnızca önyükleme çekirdeği değilse atlanır (rapor kısa kalsın).
        if thread.starts_with("idle/") && index != 0 {
            continue;
        }
        match probe(lapic) {
            Some((rip, interrupts, stack)) => {
                let mut line = format!(
                    "cpu{:<2} {:<14} rip {:#x} IF={} <-",
                    index, thread, rip, interrupts as u8
                );
                for word in stack.iter().take(10) {
                    line.push_str(&format!(" {:x}", word & 0xff_ffff));
                }
                lines.push(line);
            }
            None => lines.push(format!("cpu{:<2} {:<14} no NMI response", index, thread)),
        }
    }
    let title = if reason.starts_with("boot CPU") {
        "Sistem yanıt vermiyor: 0. çekirdek kesmeler kapalıyken takıldı"
    } else {
        "Masaüstü yanıt vermiyor"
    };
    (title, lines)
}

fn watch(target: usize) -> i64 {
    let mut last_ticks = crate::sched::bsp_ticks();
    let mut ticks_changed = tsc::now_ns();
    let mut reported = false;
    loop {
        // Yalnızca sabitlendiği (önyükleme dışı) çekirdekte çalışır; ilk turda başka bir
        // çekirdekte başladıysa oraya geçer.
        if smp::is_boot_cpu() || crate::arch::syscall::cpu_index() != target {
            crate::sched::yield_now();
            continue;
        }
        let until = tsc::now_ns() + 500_000_000;
        while tsc::now_ns() < until {
            core::hint::spin_loop();
        }
        let now = tsc::now_ns();
        let ticks = crate::sched::bsp_ticks();
        if ticks != last_ticks {
            last_ticks = ticks;
            ticks_changed = now;
        }
        let beat = GUI_BEAT.load(Ordering::Relaxed);
        let reason = if now - ticks_changed > STALL_NS {
            Some("boot CPU timer stopped (CPU 0 stuck with interrupts off)")
        } else if beat != 0 && now.saturating_sub(beat) > STALL_NS {
            Some("desktop loop stuck")
        } else {
            None
        };
        let Some(reason) = reason else {
            reported = false;
            continue;
        };
        if reported {
            continue;
        }
        reported = true;
        let (title, details) = report(reason);
        crate::kernel::crash::raise(
            crate::kernel::crash::Report {
                kind: "watchdog",
                title: String::from(title),
                details,
            },
            true,
        );
    }
}

pub fn start() {
    // Bir çekirdeği sürekli meşgul eder: öykünmede (QEMU/UTM) yalnızca `watchdog` bayrağıyla.
    let wanted = !crate::arch::cpuinfo::emulated() || crate::boot::has_flag("watchdog");
    if !wanted || crate::boot::has_flag("nowatchdog") || crate::sched::online_cpus() < 2 {
        return;
    }
    let target = crate::sched::online_cpus() - 1;
    crate::sched::spawn_pinned("watchdog", target, move || watch(target));
}
