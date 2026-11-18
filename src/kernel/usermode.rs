//! Gömülü ring 3 programlarını kendi adres alanlarında işlem olarak başlatma.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::fmt::Write;
use core::ptr::copy_nonoverlapping;

use crate::kernel::initcall::{Detail, self_test as check};
use crate::memory::paging::{self, NO_EXECUTE, USER, WRITABLE};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sched::{self, Completion, Process};

const CODE_BASE: u64 = 0x0000_0000_0040_0000;
const STACK_TOP: u64 = 0x0000_7fff_ff00_0000;
const STACK_PAGES: u64 = 4;
const MAX_CODE_PAGES: u64 = 16;

pub struct Program {
    pub name: &'static str,
    pub description: &'static str,
    code: fn() -> &'static [u8],
}

macro_rules! embedded_program {
    ($start:ident, $end:ident) => {{
        unsafe extern "C" {
            static $start: u8;
            static $end: u8;
        }
        fn code() -> &'static [u8] {
            unsafe {
                let start = &raw const $start;
                let end = &raw const $end;
                core::slice::from_raw_parts(start, end as usize - start as usize)
            }
        }
        code
    }};
}

global_asm!(
    ".pushsection .rodata.user_programs, \"a\"",
    ".global user_hello_start",
    ".global user_hello_end",
    "user_hello_start:",
    "mov eax, 6",
    "syscall",
    "mov r12, rax",
    "mov eax, 1",
    "mov edi, 1",
    "lea rsi, [rip + 10f]",
    "lea rdx, [rip + 11f]",
    "sub rdx, rsi",
    "syscall",
    "mov eax, 3",
    "syscall",
    "mov r13, rax",
    "mov eax, 4",
    "mov edi, 20",
    "syscall",
    "mov eax, 3",
    "syscall",
    "sub rax, r13",
    "cmp rax, 20",
    "jb 3f",
    "mov eax, 1",
    "mov edi, 1",
    "lea rsi, [rip + 12f]",
    "lea rdx, [rip + 13f]",
    "sub rdx, rsi",
    "syscall",
    "mov eax, 1",
    "mov edi, 1",
    "mov rsi, 0xffffffff80000000",
    "mov edx, 16",
    "syscall",
    "cmp rax, -14",
    "jne 3f",
    "mov eax, 99",
    "syscall",
    "cmp rax, -38",
    "jne 3f",
    "mov eax, 0",
    "mov edi, 42",
    "syscall",
    "3:",
    "mov eax, 0",
    "mov edi, 1",
    "syscall",
    "ud2",
    "10: .ascii \"Hello from ring 3 via syscall!\\n\"",
    "11:",
    "12: .ascii \"sleep(20) via syscall ok\\n\"",
    "13:",
    "user_hello_end:",
    "",
    ".global user_fault_start",
    ".global user_fault_end",
    "user_fault_start:",
    "mov rax, 0xffffffff80000000",
    "mov rax, [rax]",
    "mov eax, 0",
    "mov edi, 0",
    "syscall",
    "user_fault_end:",
    "",
    ".global user_privileged_start",
    ".global user_privileged_end",
    "user_privileged_start:",
    "cli",
    "hlt",
    "mov eax, 0",
    "mov edi, 0",
    "syscall",
    "user_privileged_end:",
    "",
    ".global user_spin_start",
    ".global user_spin_end",
    "user_spin_start:",
    "2: jmp 2b",
    "user_spin_end:",
    "",
    ".global user_counter_start",
    ".global user_counter_end",
    "user_counter_start:",
    "mov r12d, 5",
    "4:",
    "mov eax, 1",
    "mov edi, 1",
    "lea rsi, [rip + 14f]",
    "lea rdx, [rip + 15f]",
    "sub rdx, rsi",
    "syscall",
    "mov eax, 4",
    "mov edi, 100",
    "syscall",
    "dec r12d",
    "jnz 4b",
    "mov eax, 0",
    "mov edi, 5",
    "syscall",
    "14: .ascii \"counter: tick\\n\"",
    "15:",
    "user_counter_end:",
    ".popsection",
);

pub static PROGRAMS: [Program; 5] = [
    Program {
        name: "hello",
        description: "write/uptime/sleep/getpid syscalls, exits with 42",
        code: embedded_program!(user_hello_start, user_hello_end),
    },
    Program {
        name: "fault",
        description: "reads kernel memory, must be killed by a page fault",
        code: embedded_program!(user_fault_start, user_fault_end),
    },
    Program {
        name: "privileged",
        description: "executes cli, must be killed by a general protection fault",
        code: embedded_program!(user_privileged_start, user_privileged_end),
    },
    Program {
        name: "spin",
        description: "busy loop forever; shows preemption (stop it with kill)",
        code: embedded_program!(user_spin_start, user_spin_end),
    },
    Program {
        name: "counter",
        description: "prints 5 ticks 100 ms apart, exits with 5",
        code: embedded_program!(user_counter_start, user_counter_end),
    },
];

pub fn find(name: &str) -> Option<&'static Program> {
    PROGRAMS.iter().find(|p| p.name == name)
}

fn map_user_page(root: u64, virt: u64, flags: u64) -> Result<u64, &'static str> {
    let frame = pmm::alloc_zeroed_frame().ok_or("out of memory")?;
    if paging::map_page_in(root, virt, frame, flags | USER).is_err() {
        pmm::free_frame(frame);
        return Err("user mapping failed");
    }
    Ok(frame)
}

/// Programı yeni bir adres alanına yükler ve ring 3 thread'ini başlatır.
pub fn spawn(program: &Program) -> Result<(Arc<Process>, Arc<Completion>), &'static str> {
    let code = (program.code)();
    let code_pages = (code.len() as u64).div_ceil(PAGE_SIZE);
    if code_pages == 0 || code_pages > MAX_CODE_PAGES {
        return Err("program has an invalid size");
    }

    let process = Process::new(program.name).ok_or("out of memory")?;
    for page in 0..code_pages {
        let frame = map_user_page(process.root(), CODE_BASE + page * PAGE_SIZE, 0)?;
        let offset = (page * PAGE_SIZE) as usize;
        let chunk = &code[offset..code.len().min(offset + PAGE_SIZE as usize)];
        unsafe { copy_nonoverlapping(chunk.as_ptr(), phys_to_virt(frame), chunk.len()) };
    }
    for page in 1..=STACK_PAGES {
        map_user_page(
            process.root(),
            STACK_TOP - page * PAGE_SIZE,
            WRITABLE | NO_EXECUTE,
        )?;
    }

    let (_, completion) = sched::spawn_user(
        process.clone(),
        sched::UserStart::Enter {
            entry: CODE_BASE,
            stack: STACK_TOP,
            arg0: 0,
            arg1: 0,
        },
        None,
    )
    .ok_or("could not create the user thread")?;
    sched::process::set_completion(process.pid, completion.clone());
    crate::log!("user: started '{}' as pid {}", program.name, process.pid);
    Ok((process, completion))
}

pub fn run(program: &Program) -> Result<i64, &'static str> {
    let (process, completion) = spawn(program)?;
    let code = completion.wait();
    crate::log!(
        "user: '{}' (pid {}) exited with {}",
        program.name,
        process.pid,
        code
    );
    Ok(code)
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let mut running = Vec::new();
    for name in ["hello", "fault", "privileged", "counter"] {
        running.push(spawn(find(name).unwrap())?);
    }
    let (spin, spin_done) = spawn(find("spin").unwrap())?;
    sched::sleep_ms(30);
    let spin_alive = !spin_done.is_done();
    sched::kill_process(spin.pid);
    let spin_code = spin_done.wait();

    let codes: Vec<i64> = running.iter().map(|(_, done)| done.wait()).collect();
    let spaces_distinct = running.windows(2).all(|w| w[0].0.root() != w[1].0.root());
    drop(running);
    drop(spin);
    sched::yield_now();

    let _ = write!(
        detail,
        "{} processes, codes {:?}, spin killed",
        codes.len() + 1,
        codes
    );
    crate::log!(
        "usermode: codes {:?}, spin {} -> {}",
        codes,
        spin_alive,
        spin_code
    );
    check(
        "usermode",
        codes == [42, -128 - 14, -128 - 13, 5] && spin_alive && spin_code == -9 && spaces_distinct,
    )
}

crate::initcall!(late, 10, "Ring 3 processes + syscalls", init);
