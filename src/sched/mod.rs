//! Çok çekirdekli preemptif zamanlayıcı: ortak hazır kuyruğu, kernel thread'leri ve işlemler.
//!
//! - Durum tek bir spinlock ile korunur (kesmeler de kapanır).
//! - Bir thread'in bağlamı kaydedilene kadar `on_cpu` doğru kalır; başka bir
//!   çekirdek onu bu süre içinde seçmez. Geçişten sonra yeni bağlam
//!   `finish_switch` ile önceki thread'i serbest bırakır.
//! - Bloklanmadan hemen önce gelen uyandırma `pending_wake` ile kaybolmaz.
//! - Önyükleme çekirdeği PIT, diğerleri LAPIC zamanlayıcısı ile 1 kHz tik alır. İkincil
//!   çekirdekler boştayken zamanlayıcıları durur (iş gelince IPI ile uyanırlar); PIT tiki
//!   uyuyan thread'leri yalnızca en yakın uyanma zamanı geldiğinde tarar. Öykünmede
//!   (UTM/QEMU TCG) her kesme pahalı olduğundan boştaki yük büyük ölçüde düşer.

pub mod mutex;
pub mod process;
pub mod wait;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::fpu::FpuState;
use crate::arch::syscall::{self, MAX_CPUS, cpu_index};
use crate::arch::{apic, cpu, pit, tsc};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::memory::kstack::{self, KernelStack};
use crate::memory::paging;
use crate::sync::SpinLock;

pub use mutex::Mutex;
pub use process::Process;
pub use wait::{Completion, WaitQueue};

pub type Tid = u64;

const TIME_SLICE_MS: u32 = 10;
/// İkincil çekirdeklerin zaman dilimi zamanlayıcısı (çalışırken).
const AP_TIMER_HZ: u64 = 1000;

/// En yakın uyuyan thread uyanma zamanı (ms); `u64::MAX` = uyuyan yok. Zamanlayıcı kilidi
/// altında güncellenir.
static NEXT_WAKE: AtomicU64 = AtomicU64::new(u64::MAX);
/// Çekirdeğin LAPIC zamanlayıcısı boşta beklerken durduruldu mu.
static TIMER_PARKED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
const DEFAULT_STACK: u64 = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ready,
    Running,
    Sleeping(u64),
    Blocked,
    Dead,
}

impl State {
    pub fn name(&self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Running => "running",
            State::Sleeping(_) => "sleeping",
            State::Blocked => "blocked",
            State::Dead => "dead",
        }
    }
}

struct Thread {
    tid: Tid,
    name: String,
    state: State,
    rsp: u64,
    stack: Option<KernelStack>,
    root: u64,
    process: Option<Arc<Process>>,
    completion: Arc<Completion>,
    cpu_ns: u64,
    fpu: Box<FpuState>,
    on_cpu: bool,
    pending_wake: bool,
    last_cpu: usize,
    /// Yalnızca bu çekirdekte çalışabilir (donanım IRQ'larıyla aynı çekirdek gerekenler için).
    pinned: Option<usize>,
}

#[derive(Clone, Copy)]
struct PerCpu {
    online: bool,
    lapic_id: u32,
    current: Tid,
    idle: Tid,
    previous: Option<Tid>,
    need_resched: bool,
    slice_left: u32,
    last_switch_ns: u64,
    busy_ns: u64,
    idle_ns: u64,
    switches: u64,
    /// Hibrit işlemcinin verimlilik (E) çekirdeği.
    efficient: bool,
}

const OFFLINE: PerCpu = PerCpu {
    online: false,
    lapic_id: 0,
    current: 0,
    idle: 0,
    previous: None,
    need_resched: false,
    slice_left: TIME_SLICE_MS,
    last_switch_ns: 0,
    busy_ns: 0,
    idle_ns: 0,
    switches: 0,
    efficient: false,
};

struct Scheduler {
    threads: BTreeMap<Tid, Box<Thread>>,
    ready: VecDeque<Tid>,
    next_tid: Tid,
    dead: Vec<Tid>,
    cpus: [PerCpu; MAX_CPUS],
}

static SCHED: SpinLock<Option<Scheduler>> = SpinLock::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static TOTAL_SWITCHES: AtomicU64 = AtomicU64::new(0);

global_asm!(
    ".global switch_context",
    "switch_context:",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    "",
    ".global thread_trampoline",
    "thread_trampoline:",
    "mov rdi, r12",
    "call {start}",
    "ud2",
    start = sym thread_start,
);

unsafe extern "C" {
    fn switch_context(prev_rsp: *mut u64, next_rsp: u64);
    fn thread_trampoline();
}

type Entry = Box<dyn FnOnce() -> i64 + Send>;

/// Ring 3'e nasıl girileceği.
pub enum UserStart {
    /// Yeni program: `entry` adresinden, `rdi`/`rsi` = `arg0`/`arg1`.
    Enter {
        entry: u64,
        stack: u64,
        arg0: u64,
        arg1: u64,
    },
    /// `fork` çocuğu: ebeveynin syscall yazmaçlarıyla, `rax` = 0.
    Fork(syscall::SyscallFrame),
}

enum Start {
    Kernel(Entry),
    User(UserStart),
}

extern "C" fn thread_start(start: *mut Start) -> ! {
    finish_switch();
    cpu::enable_interrupts();
    // Kutu, geri dönülmeyen ring 3 geçişinden önce serbest bırakılmalı.
    let start = *unsafe { Box::from_raw(start) };
    match start {
        Start::Kernel(entry) => exit_current(entry()),
        Start::User(UserStart::Enter {
            entry,
            stack,
            arg0,
            arg1,
        }) => unsafe { syscall::enter_user_mode(entry, stack, arg0, arg1) },
        Start::User(UserStart::Fork(frame)) => unsafe { syscall::fork_return(&frame) },
    }
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

fn new_thread(
    tid: Tid,
    name: &str,
    state: State,
    rsp: u64,
    stack: Option<KernelStack>,
    root: u64,
) -> Box<Thread> {
    Box::new(Thread {
        tid,
        name: String::from(name),
        state,
        rsp,
        stack,
        root,
        process: None,
        completion: Arc::new(Completion::new()),
        cpu_ns: 0,
        fpu: Box::new(FpuState::initial()),
        on_cpu: state == State::Running,
        pending_wake: false,
        last_cpu: 0,
        pinned: None,
    })
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    {
        let mut guard = SCHED.lock();
        let mut threads = BTreeMap::new();
        let mut boot = new_thread(0, "kernel", State::Running, 0, None, paging::kernel_root());
        boot.pinned = Some(0);
        threads.insert(0, boot);
        let mut cpus = [OFFLINE; MAX_CPUS];
        cpus[0] = PerCpu {
            online: true,
            lapic_id: apic::lapic_id(),
            last_switch_ns: tsc::now_ns(),
            efficient: crate::arch::cpuinfo::is_efficiency_core(),
            ..OFFLINE
        };
        *guard = Some(Scheduler {
            threads,
            ready: VecDeque::new(),
            next_tid: 1,
            dead: Vec::new(),
            cpus,
        });
    }

    let idle = spawn_detached("idle/0", || {
        loop {
            cpu::enable_interrupts();
            cpu::halt();
        }
    })
    .ok_or("could not create the idle thread")?;
    with_sched(|s| {
        s.ready.retain(|&t| t != idle);
        s.cpus[0].idle = idle;
    });
    RUNNING.store(true, Ordering::Release);

    let expected = crate::arch::smp::online_count();
    let start = tsc::now_ns();
    while online_cpus() < expected && tsc::now_ns() < start + 200_000_000 {
        core::hint::spin_loop();
    }

    check("sched", self_test())?;
    let _ = write!(
        detail,
        "{} CPU(s), {} ms slices",
        online_cpus(),
        TIME_SLICE_MS
    );
    Ok(())
}

crate::initcall!(core, 60, "Scheduler (SMP)", init);

fn with_sched<R>(f: impl FnOnce(&mut Scheduler) -> R) -> Option<R> {
    SCHED.lock().as_mut().map(f)
}

/// İkincil bir çekirdeği zamanlayıcıya katar; geçerli bağlam o çekirdeğin boşta thread'i olur.
pub fn register_cpu(lapic_id: u32) {
    while !is_running() {
        core::hint::spin_loop();
    }
    let cpu = cpu_index();
    with_sched(|s| {
        let tid = s.next_tid;
        s.next_tid += 1;
        let mut idle = new_thread(
            tid,
            &alloc::format!("idle/{}", cpu),
            State::Running,
            0,
            None,
            paging::kernel_root(),
        );
        idle.last_cpu = cpu;
        s.threads.insert(tid, idle);
        s.cpus[cpu] = PerCpu {
            online: true,
            lapic_id,
            current: tid,
            idle: tid,
            last_switch_ns: tsc::now_ns(),
            efficient: crate::arch::cpuinfo::is_efficiency_core(),
            ..OFFLINE
        };
    });
}

/// Çevrimiçi verimlilik (E) çekirdeği sayısı (hibrit olmayan işlemcide 0).
pub fn efficiency_cpus() -> usize {
    with_sched(|s| s.cpus.iter().filter(|c| c.online && c.efficient).count()).unwrap_or(0)
}

pub fn online_cpus() -> usize {
    with_sched(|s| s.cpus.iter().filter(|c| c.online).count()).unwrap_or(1)
}

fn create_thread(
    name: &str,
    root: u64,
    process: Option<Arc<Process>>,
    start: Start,
    fpu: Option<FpuState>,
) -> Option<(Tid, Arc<Completion>)> {
    let stack = kstack::alloc(DEFAULT_STACK)?;
    let entry_ptr = Box::into_raw(Box::new(start));

    let mut rsp = stack.top();
    let mut push = |value: u64| {
        rsp -= 8;
        unsafe { (rsp as *mut u64).write(value) };
    };
    push(thread_trampoline as *const () as u64);
    push(0);
    push(0);
    push(entry_ptr as u64);
    push(0);
    push(0);
    push(0);

    let result = with_sched(|s| {
        let tid = s.next_tid;
        s.next_tid += 1;
        let mut thread = new_thread(tid, name, State::Ready, rsp, Some(stack), root);
        thread.process = process;
        if let Some(fpu) = fpu {
            *thread.fpu = fpu;
        }
        let completion = thread.completion.clone();
        s.threads.insert(tid, thread);
        s.ready.push_back(tid);
        (tid, completion)
    });
    match result {
        Some(r) => {
            kick_idle_cpu();
            Some(r)
        }
        None => {
            drop(unsafe { Box::from_raw(entry_ptr) });
            None
        }
    }
}

/// Bir kernel thread'i başlatır; dönen `Completion` ile çıkış kodu beklenebilir.
pub fn spawn(
    name: &str,
    f: impl FnOnce() -> i64 + Send + 'static,
) -> Option<(Tid, Arc<Completion>)> {
    create_thread(
        name,
        paging::kernel_root(),
        None,
        Start::Kernel(Box::new(f)),
        None,
    )
}

pub fn spawn_detached(name: &str, f: impl FnOnce() -> i64 + Send + 'static) -> Option<Tid> {
    spawn(name, f).map(|(tid, _)| tid)
}

/// İşlemin ring 3 thread'ini başlatır; `fpu` verilmezse temiz FPU durumuyla.
pub(crate) fn spawn_user(
    process: Arc<Process>,
    start: UserStart,
    fpu: Option<FpuState>,
) -> Option<(Tid, Arc<Completion>)> {
    let root = process.root();
    let name = process.name();
    create_thread(&name, root, Some(process), Start::User(start), fpu)
}

/// `exec` sonrası: geçerli thread'in adres alanını ve adını günceller, yeni alana geçer.
pub(crate) fn set_current_image(root: u64, name: &str) {
    cpu::without_interrupts(|| {
        let cpu = cpu_index();
        with_sched(|s| {
            if let Some(t) = s.threads.get_mut(&s.cpus[cpu].current) {
                t.root = root;
                t.name = String::from(name);
            }
        });
        paging::switch_address_space(root);
    });
}

/// Boşta bekleyen bir çekirdek varsa hazır işi hemen alması için IPI gönderir.
fn kick_idle_cpu() {
    let me = cpu_index();
    let target = with_sched(|s| {
        if s.ready.is_empty() {
            return None;
        }
        let idle =
            |(i, c): &(usize, &PerCpu)| *i != me && c.online && c.current == c.idle && c.idle != 0;
        // Bir çekirdeğe sabitlenmiş hazır iş varsa önce o çekirdek uyandırılır.
        let pinned = s.ready.iter().find_map(|tid| {
            let cpu = s.threads.get(tid)?.pinned?;
            let c = s.cpus.get(cpu)?;
            idle(&(cpu, c)).then_some(c.lapic_id)
        });
        if pinned.is_some() {
            return pinned;
        }
        // Hibrit işlemcide önce boştaki performans (P) çekirdeği uyandırılır.
        let cpus = || s.cpus.iter().enumerate().filter(idle);
        cpus()
            .find(|(_, c)| !c.efficient)
            .or_else(|| cpus().next())
            .map(|(_, c)| c.lapic_id)
    })
    .flatten();
    if let Some(lapic) = target
        && apic::active()
    {
        apic::send_ipi(lapic, crate::arch::idt::IPI_VECTOR);
    }
}

struct Switch {
    prev_rsp: *mut u64,
    next_rsp: u64,
    next_root: u64,
    next_top: Option<u64>,
    prev_fpu: *mut FpuState,
    next_fpu: *const FpuState,
}

/// Kesmeler kapalıyken çağrılır.
fn schedule() {
    let cpu = cpu_index();
    let now = tsc::now_ns();
    let switch = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else { return };
        let pc = s.cpus[cpu];
        let current = pc.current;
        let delta = now.saturating_sub(pc.last_switch_ns);
        if current == pc.idle {
            s.cpus[cpu].idle_ns += delta;
        } else {
            s.cpus[cpu].busy_ns += delta;
        }
        s.cpus[cpu].last_switch_ns = now;
        let mut pinned_elsewhere = None;
        if let Some(thread) = s.threads.get_mut(&current) {
            thread.cpu_ns += delta;
            if thread.state == State::Running {
                thread.state = State::Ready;
                if current != pc.idle {
                    s.ready.push_back(current);
                    pinned_elsewhere = thread.pinned.filter(|&p| p != cpu);
                }
            }
        }
        // Başka çekirdeğe sabitli thread sıraya döndü: o çekirdek uyuyorsa uyandırılır.
        if let Some(target) = pinned_elsewhere
            && let Some(c) = s.cpus.get(target)
            && c.online
            && apic::active()
        {
            apic::send_ipi(c.lapic_id, crate::arch::idt::IPI_VECTOR);
        }

        let mut next = pc.idle;
        let mut skipped = Vec::new();
        let pinned_here = s.ready.iter().position(|tid| {
            s.threads
                .get(tid)
                .is_some_and(|t| t.state == State::Ready && !t.on_cpu && t.pinned == Some(cpu))
        });
        if let Some(i) = pinned_here {
            next = s.ready.remove(i).unwrap_or(pc.idle);
        }
        while next == pc.idle
            && let Some(tid) = s.ready.pop_front()
        {
            match s.threads.get(&tid) {
                Some(t)
                    if t.state == State::Ready
                        && !t.on_cpu
                        && t.pinned.is_none_or(|p| p == cpu) =>
                {
                    next = tid;
                    break;
                }
                Some(t) if t.state == State::Ready && t.tid == current => {
                    next = tid;
                    break;
                }
                Some(t) if t.state == State::Ready => skipped.push(tid),
                _ => {}
            }
        }
        for tid in skipped.into_iter().rev() {
            s.ready.push_front(tid);
        }
        s.cpus[cpu].slice_left = TIME_SLICE_MS;
        s.cpus[cpu].need_resched = false;
        let run_idle = next == pc.idle;

        if next == current {
            if let Some(t) = s.threads.get_mut(&current) {
                t.state = State::Running;
            }
            (None, run_idle)
        } else {
            let next_thread = s.threads.get_mut(&next).expect("next thread missing");
            next_thread.state = State::Running;
            next_thread.on_cpu = true;
            next_thread.last_cpu = cpu;
            let next_rsp = next_thread.rsp;
            let next_root = next_thread.root;
            let next_top = next_thread.stack.as_ref().map(|k| k.top());
            let next_fpu = &*next_thread.fpu as *const FpuState;
            s.cpus[cpu].current = next;
            s.cpus[cpu].previous = Some(current);
            s.cpus[cpu].switches += 1;
            TOTAL_SWITCHES.fetch_add(1, Ordering::Relaxed);
            let prev = s.threads.get_mut(&current).expect("current thread missing");
            (
                Some(Switch {
                    prev_rsp: &mut prev.rsp as *mut u64,
                    next_rsp,
                    next_root,
                    next_top,
                    prev_fpu: &mut *prev.fpu as *mut FpuState,
                    next_fpu,
                }),
                run_idle,
            )
        }
    };
    let (switch, run_idle) = switch;
    park_timer(cpu, run_idle);

    if let Some(sw) = switch {
        unsafe {
            (*sw.prev_fpu).save();
            (*sw.next_fpu).restore();
        }
        if let Some(top) = sw.next_top {
            syscall::set_kernel_stack(top);
        }
        paging::switch_address_space(sw.next_root);
        unsafe { switch_context(sw.prev_rsp, sw.next_rsp) };
        finish_switch();
    }
    reap();
}

/// İkincil çekirdek boşta kalacaksa LAPIC zamanlayıcısını durdurur, iş başlarken yeniden
/// başlatır (önyükleme çekirdeği PIT ile saati ve uyuyanları yürüttüğü için hep tik alır).
fn park_timer(cpu: usize, idle: bool) {
    if crate::arch::smp::is_boot_cpu() || !apic::active() {
        return;
    }
    if TIMER_PARKED[cpu].swap(idle, Ordering::AcqRel) != idle {
        if idle {
            apic::stop_timer();
        } else {
            apic::start_timer(AP_TIMER_HZ);
        }
    }
}

/// Yeni bağlamda çağrılır: bu çekirdeğin bıraktığı thread artık başka çekirdeklerce seçilebilir.
fn finish_switch() {
    let cpu = cpu_index();
    with_sched(|s| {
        if let Some(prev) = s.cpus[cpu].previous.take()
            && let Some(t) = s.threads.get_mut(&prev)
        {
            t.on_cpu = false;
        }
    });
}

fn reap() {
    let dead: Vec<Box<Thread>> = with_sched(|s| {
        let ids: Vec<Tid> = s
            .dead
            .iter()
            .copied()
            .filter(|t| s.threads.get(t).is_some_and(|th| !th.on_cpu))
            .collect();
        s.dead.retain(|t| !ids.contains(t));
        ids.iter().filter_map(|t| s.threads.remove(t)).collect()
    })
    .unwrap_or_default();
    drop(dead);
}

pub fn yield_now() {
    cpu::without_interrupts(schedule);
}

pub fn current_tid() -> Tid {
    cpu::without_interrupts(|| {
        let cpu = cpu_index();
        with_sched(|s| s.cpus[cpu].current).unwrap_or(0)
    })
}

pub fn current_process() -> Option<Arc<Process>> {
    cpu::without_interrupts(|| {
        let cpu = cpu_index();
        with_sched(|s| s.threads.get(&s.cpus[cpu].current)?.process.clone()).flatten()
    })
}

/// Geçerli thread'in durumunu değiştirir; `honor_pending` ise bekleyen uyandırmayı tüketir.
fn park(state: State, honor_pending: bool) -> bool {
    let cpu = cpu_index();
    with_sched(|s| {
        let current = s.cpus[cpu].current;
        let Some(t) = s.threads.get_mut(&current) else {
            return false;
        };
        if honor_pending && t.pending_wake {
            t.pending_wake = false;
            return false;
        }
        t.state = state;
        if let State::Sleeping(until) = state {
            NEXT_WAKE.fetch_min(until, Ordering::AcqRel);
        }
        true
    })
    .unwrap_or(false)
}

pub fn sleep_ms(ms: u64) {
    let wake = pit::uptime_ms() + ms;
    while pit::uptime_ms() < wake {
        cpu::without_interrupts(|| {
            if park(State::Sleeping(wake), false) {
                schedule();
            }
        });
    }
}

/// `wake` ya da `deadline_ms` (uptime) gelene kadar geçerli thread'i uyutur.
pub(crate) fn block_current_until(deadline_ms: u64) {
    if park(State::Sleeping(deadline_ms), true) {
        schedule();
    }
}

/// Geçerli thread'i engeller; `wake` ile uyandırılana kadar çalışmaz.
pub(crate) fn block_current() {
    if park(State::Blocked, true) {
        schedule();
    }
}

pub(crate) fn wake(tids: &[Tid]) {
    let me = cpu_index();
    let pinned_ipi = with_sched(|s| {
        let mut ipi = None;
        for tid in tids {
            let Some(t) = s.threads.get_mut(tid) else {
                continue;
            };
            match t.state {
                State::Blocked | State::Sleeping(_) => {
                    t.state = State::Ready;
                    s.ready.push_back(*tid);
                    // Sabitli thread (kabuk) yalnızca kendi çekirdeğinde koşabilir:
                    // o çekirdek meşgulse dilimin bitmesini beklemeden yer açılır.
                    if let Some(cpu) = t.pinned
                        && s.cpus[cpu].online
                    {
                        s.cpus[cpu].need_resched = true;
                        if cpu != me {
                            ipi = Some(s.cpus[cpu].lapic_id);
                        }
                    }
                }
                State::Running | State::Ready => t.pending_wake = true,
                State::Dead => {}
            }
        }
        for c in s
            .cpus
            .iter_mut()
            .filter(|c| c.online && c.current == c.idle)
        {
            c.need_resched = true;
        }
        ipi
    })
    .flatten();
    if let Some(lapic) = pinned_ipi
        && apic::active()
    {
        apic::send_ipi(lapic, crate::arch::idt::IPI_VECTOR);
    }
    kick_idle_cpu();
}

pub fn exit_current(code: i64) -> ! {
    // Tanıtıcılar thread hâlâ canlıyken kapanır: dosya kapatma G/Ç bekleyebilir.
    if let Some(process) = current_process() {
        cpu::enable_interrupts();
        process.set_exit_code(code);
        process.close_all();
        crate::gui::app::forget_process(process.pid);
    }
    cpu::disable_interrupts();
    let cpu = cpu_index();
    let completion = with_sched(|s| {
        let current = s.cpus[cpu].current;
        let thread = s.threads.get_mut(&current).expect("current thread missing");
        thread.state = State::Dead;
        s.dead.push(current);
        thread.completion.clone()
    })
    .expect("scheduler not running");
    completion.complete(code);
    drop(completion);
    schedule();
    unreachable!("dead thread was scheduled again")
}

/// Önyükleme çekirdeğinde PIT kesmesinden (1 kHz) çağrılır: uyuyanları uyandırır.
/// Önyükleme çekirdeğinin zamanlayıcı tikleri (PIT ya da LAPIC; bekçi canlılık için okur).
static BSP_TICKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn bsp_ticks() -> u64 {
    BSP_TICKS.load(Ordering::Relaxed)
}

pub fn tick() {
    BSP_TICKS.fetch_add(1, Ordering::Relaxed);
    if !is_running() {
        return;
    }
    let now = pit::uptime_ms();
    let cpu = cpu_index();
    let woke = with_sched(|s| {
        let woke = scan_sleepers(s, now);
        local_tick(s, cpu);
        woke
    })
    .unwrap_or(false);
    if woke {
        kick_idle_cpu();
    }
}

fn scan_sleepers(s: &mut Scheduler, now: u64) -> bool {
    let mut woke = false;
    if now >= NEXT_WAKE.load(Ordering::Acquire) {
        let mut next = u64::MAX;
        for thread in s.threads.values_mut() {
            if let State::Sleeping(until) = thread.state {
                if now >= until {
                    thread.state = State::Ready;
                    s.ready.push_back(thread.tid);
                    woke = true;
                } else {
                    next = next.min(until);
                }
            }
        }
        NEXT_WAKE.store(next, Ordering::Release);
    }
    woke
}

/// Önyükleme çekirdeği takıldığında (hata raporu) süresi dolan uyuyanları başka bir
/// çekirdekten uyandırır; böylece disk/ağ sürücüleri çalışmayı sürdürür.
pub fn wake_sleepers() {
    let now = pit::uptime_ms();
    let Some(mut guard) = SCHED.try_lock_for(1_000_000) else {
        return;
    };
    let woke = guard.as_mut().is_some_and(|s| scan_sleepers(s, now));
    drop(guard);
    if woke {
        kick_idle_cpu();
    }
}

fn local_tick(s: &mut Scheduler, cpu: usize) {
    let pc = &mut s.cpus[cpu];
    pc.slice_left = pc.slice_left.saturating_sub(1);
    if pc.slice_left == 0 || (pc.current == pc.idle && !s.ready.is_empty()) {
        s.cpus[cpu].need_resched = true;
    }
}

/// İkincil çekirdeklerde LAPIC zamanlayıcısından çağrılır.
pub fn tick_secondary() {
    if !is_running() {
        return;
    }
    let cpu = cpu_index();
    with_sched(|s| {
        if s.cpus[cpu].online {
            local_tick(s, cpu);
        }
    });
}

/// IPI geldi: bu çekirdek bir sonraki kesme dönüşünde yeniden zamanlar.
pub fn request_resched() {
    let cpu = cpu_index();
    with_sched(|s| s.cpus[cpu].need_resched = true);
}

/// Her kesmenin sonunda çağrılır; gerekiyorsa preemption yapar.
pub fn preempt_point(from_user: bool) {
    if !is_running() {
        return;
    }
    let cpu = cpu_index();
    let resched = with_sched(|s| s.cpus[cpu].online && s.cpus[cpu].need_resched).unwrap_or(false);
    if resched {
        schedule();
    }
    if from_user && let Some(signal) = pending_signal() {
        exit_current(-(signal as i64));
    }
}

pub struct ThreadInfo {
    pub tid: Tid,
    pub name: String,
    pub state: State,
    pub pid: u64,
    pub cpu_ms: u64,
    pub cpu: usize,
}

pub fn threads() -> Vec<ThreadInfo> {
    with_sched(|s| {
        s.threads
            .values()
            .map(|t| ThreadInfo {
                tid: t.tid,
                name: t.name.clone(),
                state: t.state,
                pid: t.process.as_ref().map(|p| p.pid).unwrap_or(0),
                cpu_ms: t.cpu_ns / 1_000_000,
                cpu: t.last_cpu,
            })
            .collect()
    })
    .unwrap_or_default()
}

pub fn context_switches() -> u64 {
    TOTAL_SWITCHES.load(Ordering::Relaxed)
}

static LOAD_SNAPSHOT: SpinLock<[(u64, u64); MAX_CPUS]> = SpinLock::new([(0, 0); MAX_CPUS]);

/// Çevrimiçi her çekirdeğin son ölçümden beri yüzde kaç meşgul olduğu.
pub fn cpu_loads() -> Vec<u8> {
    let now = tsc::now_ns();
    let totals: Vec<(usize, u64, u64)> = with_sched(|s| {
        s.cpus
            .iter()
            .enumerate()
            .filter(|(_, c)| c.online)
            .map(|(i, c)| {
                let running = now.saturating_sub(c.last_switch_ns);
                let (busy, idle) = if c.current == c.idle {
                    (c.busy_ns, c.idle_ns + running)
                } else {
                    (c.busy_ns + running, c.idle_ns)
                };
                (i, busy, idle)
            })
            .collect()
    })
    .unwrap_or_default();
    let mut snapshot = LOAD_SNAPSHOT.lock();
    totals
        .into_iter()
        .map(|(i, busy, idle)| {
            let (old_busy, old_idle) = snapshot[i];
            snapshot[i] = (busy, idle);
            let db = busy.saturating_sub(old_busy);
            let di = idle.saturating_sub(old_idle);
            ((db * 100) / (db + di).max(1)) as u8
        })
        .collect()
}

pub struct CpuStat {
    pub index: usize,
    pub lapic_id: u32,
    pub current: String,
    pub switches: u64,
}

/// Tanı için: zamanlayıcı kilidi takılıysa beklemeden `None`.
pub fn try_cpu_stats(timeout_ns: u64) -> Option<Vec<CpuStat>> {
    let guard = SCHED.try_lock_for(timeout_ns)?;
    let s = guard.as_ref()?;
    Some(
        s.cpus
            .iter()
            .enumerate()
            .filter(|(_, c)| c.online)
            .map(|(i, c)| CpuStat {
                index: i,
                lapic_id: c.lapic_id,
                current: s
                    .threads
                    .get(&c.current)
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
                switches: c.switches,
            })
            .collect(),
    )
}

/// Çekirdek thread'ini yalnızca `cpu` çekirdeğinde çalışacak şekilde başlatır.
pub fn spawn_pinned(
    name: &str,
    cpu: usize,
    f: impl FnOnce() -> i64 + Send + 'static,
) -> Option<Tid> {
    let (tid, _) = spawn(name, f)?;
    with_sched(|s| {
        if let Some(t) = s.threads.get_mut(&tid) {
            t.pinned = Some(cpu);
        }
    });
    cpu::without_interrupts(kick_idle_cpu);
    Some(tid)
}

pub fn cpu_stats() -> Vec<CpuStat> {
    with_sched(|s| {
        s.cpus
            .iter()
            .enumerate()
            .filter(|(_, c)| c.online)
            .map(|(i, c)| CpuStat {
                index: i,
                lapic_id: c.lapic_id,
                current: s
                    .threads
                    .get(&c.current)
                    .map(|t| t.name.clone())
                    .unwrap_or_default(),
                switches: c.switches,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Geçerli işlemin bekleyen sonlandırıcı sinyali.
pub fn pending_signal() -> Option<u32> {
    current_process()
        .map(|p| p.pending_signal())
        .filter(|&s| s != 0)
}

pub fn kill_process(pid: u64) -> bool {
    signal_process(pid, process::SIGKILL)
}

/// Sinyali işleme iletir ve bloklu thread'ini uyandırır; işlem yoksa `false`.
pub fn signal_process(pid: u64, signal: u32) -> bool {
    let tids: Vec<Tid> = with_sched(|s| {
        s.threads
            .values()
            .filter(|t| t.process.as_ref().is_some_and(|p| p.pid == pid))
            .filter_map(|t| t.process.as_ref().unwrap().signal(signal).then_some(t.tid))
            .collect()
    })
    .unwrap_or_default();
    wake(&tids);
    process::find(pid).is_some() || !tids.is_empty()
}

/// Sinyali süreç grubundaki her işleme iletir; kaç işlem bulunduğunu döndürür.
pub fn signal_group(pgid: u64, signal: u32) -> usize {
    let members = process::group(pgid);
    for p in &members {
        signal_process(p.pid, signal);
    }
    members.len()
}

fn self_test() -> bool {
    use core::sync::atomic::AtomicU64;
    static COUNTERS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
    static CPUS_SEEN: AtomicU64 = AtomicU64::new(0);

    let mut handles = Vec::new();
    for i in 0..3u8 {
        let handle = spawn("sched-test", move || {
            for _ in 0..5 {
                COUNTERS[i as usize].fetch_add(1, Ordering::Relaxed);
                yield_now();
            }
            sleep_ms(5);
            100 + i as i64
        });
        match handle {
            Some((_, completion)) => handles.push(completion),
            None => return false,
        }
    }
    let codes: Vec<i64> = handles.iter().map(|c| c.wait()).collect();
    let counts_ok = COUNTERS.iter().all(|c| c.load(Ordering::Relaxed) == 5);

    let spin_flag = Arc::new(AtomicBool::new(false));
    let flag = spin_flag.clone();
    let Some((_, spinner)) = spawn("sched-spin", move || {
        let start = pit::uptime_ms();
        while pit::uptime_ms() < start + 30 {
            core::hint::spin_loop();
        }
        flag.store(true, Ordering::Relaxed);
        0
    }) else {
        return false;
    };
    sleep_ms(1);
    let preempted = !spin_flag.load(Ordering::Relaxed);
    spinner.wait();

    let cpus = online_cpus();
    let workers: Vec<Arc<Completion>> = (0..cpus.max(2) * 2)
        .filter_map(|_| {
            spawn("sched-smp", || {
                let start = tsc::now_ns();
                while tsc::now_ns() < start + 40_000_000 {
                    CPUS_SEEN.fetch_or(1 << (cpu_index() % 64), Ordering::Relaxed);
                    core::hint::spin_loop();
                }
                0
            })
            .map(|(_, c)| c)
        })
        .collect();
    for w in &workers {
        w.wait();
    }
    let seen = CPUS_SEEN.load(Ordering::Relaxed).count_ones() as usize;
    let parallel = cpus == 1 || seen >= cpus.min(2);

    reap();
    let leftover = with_sched(|s| {
        s.threads
            .values()
            .filter(|t| t.name.starts_with("sched-"))
            .count()
    })
    .unwrap_or(1);
    crate::log!(
        "sched: codes {:?}, preempted {}, {} CPU(s) online, workers ran on {} CPU(s), leftover {}",
        codes,
        preempted,
        cpus,
        seen,
        leftover
    );
    codes == [100, 101, 102] && counts_ok && preempted && parallel && leftover <= 1
}
