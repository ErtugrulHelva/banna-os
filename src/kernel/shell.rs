//! Çekirdek kabuğu: satır düzenleme, geçmiş ve tanı komutları.

use alloc::string::String;
use alloc::vec::Vec;

use crate::arch::pit;
use crate::drivers::keyboard::{self, Key};
use crate::drivers::{acpi, battery, console, pci, power, rtc};
use crate::fs;
use crate::kernel::users;
use crate::kernel::{elf, recovery, usermode};
use crate::memory::{heap, pmm};
use crate::sched::process::Descriptor;
use crate::{aml, sched};

fn prompt() -> String {
    if recovery::is_active() {
        let name = users::session().map_or_else(String::new, |s| s.name);
        return alloc::format!("{}@kurtarma# ", name);
    }
    match users::session() {
        Some(s) if users::effective_uid() == users::ROOT_UID => {
            alloc::format!("{}@banna# ", s.name)
        }
        Some(s) => alloc::format!("{}@banna> ", s.name),
        None => String::from("banna> "),
    }
}
const HISTORY_LIMIT: usize = 32;

struct Command {
    name: &'static str,
    help: &'static str,
    run: fn(&str),
}

const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        help: "list commands",
        run: cmd_help,
    },
    Command {
        name: "clear",
        help: "clear the screen",
        run: |_| console::clear(),
    },
    Command {
        name: "echo",
        help: "print the arguments",
        run: |args| println!("{}", args),
    },
    Command {
        name: "mem",
        help: "physical memory and heap usage",
        run: cmd_mem,
    },
    Command {
        name: "uptime",
        help: "time since boot",
        run: cmd_uptime,
    },
    Command {
        name: "date",
        help: "current date and time from the RTC",
        run: cmd_date,
    },
    Command {
        name: "whoami",
        help: "current user",
        run: cmd_whoami,
    },
    Command {
        name: "users",
        help: "list accounts",
        run: cmd_users,
    },
    Command {
        name: "useradd",
        help: "add account: useradd <name> [admin]",
        run: cmd_useradd,
    },
    Command {
        name: "userdel",
        help: "remove account: userdel <name>",
        run: cmd_userdel,
    },
    Command {
        name: "passwd",
        help: "change password: passwd [name]",
        run: cmd_passwd,
    },
    Command {
        name: "sudo",
        help: "run a command as root: sudo <command>",
        run: cmd_sudo,
    },
    Command {
        name: "logout",
        help: "end the session",
        run: cmd_logout,
    },
    Command {
        name: "lsusb",
        help: "USB devices",
        run: cmd_lsusb,
    },
    Command {
        name: "mount",
        help: "mounted file systems",
        run: cmd_mount,
    },
    Command {
        name: "fsck",
        help: "check ext2 volume: fsck [-y] [/mount]",
        run: cmd_fsck,
    },
    Command {
        name: "umount",
        help: "unmount: umount /usb0",
        run: cmd_umount,
    },
    Command {
        name: "netstat",
        help: "TCP sockets",
        run: cmd_netstat,
    },
    Command {
        name: "lsmod",
        help: "loaded kernel modules",
        run: cmd_lsmod,
    },
    Command {
        name: "insmod",
        help: "load a kernel module: insmod <name|path.bko>",
        run: cmd_insmod,
    },
    Command {
        name: "keymap",
        help: "keyboard layout: keymap [tr|us]",
        run: cmd_layout,
    },
    Command {
        name: "cpu",
        help: "processor information",
        run: cmd_cpu,
    },
    Command {
        name: "lspci",
        help: "list PCI devices",
        run: cmd_lspci,
    },
    Command {
        name: "acpi",
        help: "ACPI tables and platform info",
        run: cmd_acpi,
    },
    Command {
        name: "battery",
        help: "battery and AC adapter status",
        run: cmd_battery,
    },
    Command {
        name: "ps",
        help: "list threads and processes",
        run: cmd_ps,
    },
    Command {
        name: "kill",
        help: "terminate a process: kill <pid>",
        run: cmd_kill,
    },
    Command {
        name: "aml",
        help: "ACPI AML: aml eval <path> | aml devices",
        run: cmd_aml,
    },
    Command {
        name: "desktop",
        help: "start the graphical desktop (60 Hz compositor)",
        run: cmd_desktop,
    },
    Command {
        name: "console",
        help: "leave the desktop and return to the full-screen console",
        run: |_| crate::gui::stop(),
    },
    Command {
        name: "power",
        help: "power status; power profile performance|balanced|powersave",
        run: cmd_power,
    },
    Command {
        name: "brightness",
        help: "screen brightness: brightness [0-100]",
        run: cmd_brightness,
    },
    Command {
        name: "volume",
        help: "sound volume: volume [0-100]",
        run: cmd_volume,
    },
    Command {
        name: "beep",
        help: "play a beep: beep [Hz] [ms]",
        run: cmd_beep,
    },
    Command {
        name: "fps",
        help: "display and compositor statistics; fps bench: render speed",
        run: cmd_fps,
    },
    Command {
        name: "ls",
        help: "list a directory: ls [path]",
        run: cmd_ls,
    },
    Command {
        name: "cat",
        help: "print files: cat <file>...",
        run: cmd_cat,
    },
    Command {
        name: "write",
        help: "write a line to a file: write <file> <text>",
        run: cmd_write,
    },
    Command {
        name: "mkdir",
        help: "create a directory",
        run: cmd_mkdir,
    },
    Command {
        name: "rm",
        help: "remove a file or empty directory",
        run: cmd_rm,
    },
    Command {
        name: "df",
        help: "file system usage",
        run: cmd_df,
    },
    Command {
        name: "disks",
        help: "list disks and volumes",
        run: cmd_disks,
    },
    Command {
        name: "gpu",
        help: "graphics adapter: gpu | gpu modes | gpu mode <W>x<H>",
        run: cmd_gpu,
    },
    Command {
        name: "net",
        help: "network status: link, DHCP lease, statistics, ARP",
        run: cmd_net,
    },
    Command {
        name: "ping",
        help: "ping <host|ip> [count]",
        run: cmd_ping,
    },
    Command {
        name: "host",
        help: "resolve a host name with DNS",
        run: cmd_host,
    },
    Command {
        name: "dhcp",
        help: "dhcp renew | dhcp release",
        run: cmd_dhcp,
    },
    Command {
        name: "programs",
        help: "list built-in ring 3 programs",
        run: cmd_programs,
    },
    Command {
        name: "run",
        help: "run a ring 3 program: run <name> [&]",
        run: cmd_run,
    },
    Command {
        name: "reboot",
        help: "restart the machine",
        run: |_| power::reboot(),
    },
    Command {
        name: "shutdown",
        help: "power off the machine",
        run: |_| power::shutdown(),
    },
    Command {
        name: "dmesg",
        help: "kernel log: dmesg [lines] | dmesg save [path]",
        run: super::rescue::cmd_dmesg,
    },
    Command {
        name: "hwinfo",
        help: "hardware summary (CPU, memory, PCI, disks)",
        run: super::rescue::cmd_hwinfo,
    },
    Command {
        name: "bootcfg",
        help: "boot settings: bootcfg [timeout|cmdline|add|remove]",
        run: super::rescue::cmd_bootcfg,
    },
    Command {
        name: "rollback",
        help: "recovery: make the previous kernel the default",
        run: super::rescue::cmd_rollback,
    },
    Command {
        name: "backup",
        help: "copy /home and /etc: backup <dir>",
        run: super::rescue::cmd_backup,
    },
    Command {
        name: "restore",
        help: "recovery: restore a backup: restore <dir>",
        run: super::rescue::cmd_restore,
    },
    Command {
        name: "factory-reset",
        help: "recovery: delete accounts, homes and settings",
        run: super::rescue::cmd_factory_reset,
    },
    Command {
        name: "debug-hang",
        help: "test the watchdog: freeze CPU 0 with interrupts off",
        run: |_| {
            if users::effective_uid() == users::ROOT_UID {
                crate::arch::cpu::disable_interrupts();
                loop {
                    core::hint::spin_loop();
                }
            }
            println!("debug-hang: root only");
        },
    },
    Command {
        name: "reset-desktop",
        help: "restore default desktop settings",
        run: super::rescue::cmd_reset_desktop,
    },
];

pub fn run() -> ! {
    loop {
        if crate::gui::is_running() {
            // Masaüstünde kabuk, kullanıcı alanındaki terminal uygulamasıdır.
            crate::gui::wait_stopped();
            continue;
        }
        if recovery::is_active() {
            recovery_banner();
        }
        let session = console_login();
        console::clear();
        if recovery::is_active() {
            users::set_elevated(true);
            recovery_banner();
            println!("{} olarak root yetkisiyle oturum açıldı.", session.name);
            println!("Komutlar: help. Normal açılış için: reboot");
        } else {
            console::set_color(console::CYAN);
            println!("Hoş geldin, {}!", session.name);
            console::set_color(console::WHITE);
            println!("Type 'help' for a list of commands.");
        }
        session_loop();
        console::clear();
    }
}

fn recovery_banner() {
    console::set_color(console::RED);
    println!("BANNA OS KURTARMA KİPİ");
    console::set_color(console::GRAY);
    println!("Masaüstü kapalı. Hesaplar, ağ, disk ve donanım ayarları buradan yönetilir.");
    println!("Giriş için yönetici hesabı gerekir.");
    console::set_color(console::WHITE);
    println!();
}

/// Metin konsolunda giriş (ilk açılışta yönetici hesabı kurulumu).
fn console_login() -> users::Session {
    loop {
        if let Some(s) = users::session() {
            return s;
        }
        if users::needs_setup() {
            crate::log!("login: setup required");
            console::set_color(console::CYAN);
            println!("Banna OS ilk kurulum: yönetici hesabı oluştur");
            console::set_color(console::WHITE);
            print!("kullanıcı adı: ");
            let name = read_plain();
            let password = read_secret("parola: ");
            let again = read_secret("parola (tekrar): ");
            if password != again {
                println!("Parolalar eşleşmiyor.");
                continue;
            }
            match users::create(name.trim(), &password, true) {
                Ok(user) => users::begin_session(&user),
                Err(e) => println!("kurulum: {}", e),
            }
            continue;
        }
        crate::log!("login: waiting");
        print!("banna login: ");
        let name = read_plain();
        let password = read_secret("parola: ");
        match users::authenticate(name.trim(), &password) {
            Some(user) if recovery::is_active() && !user.admin => {
                crate::log!("recovery: '{}' is not an administrator", user.name);
                println!("Kurtarma kipi yönetici hesabı gerektirir.");
            }
            Some(user) => users::begin_session(&user),
            None => {
                crate::log!("login: failed attempt for '{}'", name.trim());
                crate::sched::sleep_ms(600);
                println!("Giriş başarısız.");
            }
        }
    }
}

pub(crate) fn read_line_plain() -> String {
    let line = read_plain();
    println!();
    line
}

/// Yankılı tek satır okur.
fn read_plain() -> String {
    let mut line = String::new();
    console::set_cursor(true);
    loop {
        let event = next_key();
        match event.key {
            Key::Enter => break,
            Key::Backspace => {
                if line.pop().is_some() {
                    print!("\x08");
                }
            }
            Key::Char(c) if !c.is_control() && line.len() < 64 => {
                line.push(c);
                print!("{}", c);
            }
            _ => {}
        }
    }
    console::set_cursor(false);
    println!();
    line
}

/// Parolayı ekrana yazmadan (yıldızlarla) okur.
fn read_secret(label: &str) -> String {
    print!("{}", label);
    let mut line = String::new();
    console::set_cursor(true);
    loop {
        let event = next_key();
        match event.key {
            Key::Enter => break,
            Key::Backspace => {
                if line.pop().is_some() {
                    print!("\x08");
                }
            }
            Key::Char(c) if !c.is_control() && line.len() < 64 => {
                line.push(c);
                print!("*");
            }
            _ => {}
        }
    }
    console::set_cursor(false);
    println!();
    line
}

/// Oturum kapanana (`logout`) kadar komut okur.
fn session_loop() {
    let mut history: Vec<String> = Vec::new();
    let generation = users::generation();
    loop {
        if users::generation() != generation || users::session().is_none() {
            return;
        }
        let line = read_line(&mut history);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if history.last().is_none_or(|last| last != line) {
            if history.len() == HISTORY_LIMIT {
                history.remove(0);
            }
            history.push(String::from(line));
        }
        execute(line);
        if crate::gui::is_running() {
            return;
        }
    }
}

/// Masaüstü terminalinden (`kcmd` syscall'ı) çalıştırılabilen çekirdek komutları.
/// Sistemi değiştirenler (hesaplar, sudo, dhcp, aml eval, gpu mode) kurtarma
/// kipindeki çekirdek kabuğunda kalır.
const PROCESS_COMMANDS: &[&str] = &[
    "help",
    "fsck",
    "mem",
    "volume",
    "beep",
    "power",
    "brightness",
    "uptime",
    "date",
    "whoami",
    "users",
    "lsusb",
    "mount",
    "umount",
    "netstat",
    "lsmod",
    "insmod",
    "keymap",
    "dmesg",
    "hwinfo",
    "bootcfg",
    "backup",
    "reset-desktop",
    "cpu",
    "lspci",
    "acpi",
    "battery",
    "ps",
    "aml",
    "fps",
    "gpu",
    "disks",
    "df",
    "net",
    "ping",
    "host",
    "desktop",
    "console",
    "reboot",
    "shutdown",
];

fn process_may_run(name: &str, args: &str) -> bool {
    if !PROCESS_COMMANDS.contains(&name) {
        return false;
    }
    match name {
        "aml" => args.trim() == "devices",
        // Çözünürlük Ayarlar'dan da herkesçe değiştirilebilir.
        "gpu" => matches!(
            args.split_whitespace().next(),
            None | Some("modes" | "mode")
        ),
        // Onarım yalnızca root işlemlerden (sudo fsck -y).
        "fsck" => {
            !args.split_whitespace().any(|a| a == "-y")
                || sched::current_process().is_some_and(|p| p.uid() == users::ROOT_UID)
        }
        _ => true,
    }
}

static REDIRECTS: crate::sync::SpinLock<Vec<(sched::Tid, Descriptor)>> =
    crate::sync::SpinLock::new(Vec::new());
static REDIRECTING: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// `print!` çıktısı yönlendirilmiş bir thread'den geliyorsa hedefe yazar.
pub fn redirected(args: core::fmt::Arguments) -> bool {
    use core::sync::atomic::Ordering;
    if REDIRECTING.load(Ordering::Acquire) == 0 || !sched::is_running() {
        return false;
    }
    let tid = sched::current_tid();
    let target = REDIRECTS
        .lock()
        .iter()
        .find(|(t, _)| *t == tid)
        .map(|(_, d)| d.clone());
    let Some(target) = target else {
        return false;
    };
    let text = alloc::format!("{}", args);
    crate::kernel::syscall::write_descriptor(&target, text.as_bytes());
    true
}

/// İzinli bir çekirdek komutunu işlem adına, çıktısı `stdout`'a gidecek şekilde çalıştırır.
pub fn run_for_process(line: &str, stdout: Descriptor) -> bool {
    use core::sync::atomic::Ordering;
    let (name, args) = line.split_once(' ').unwrap_or((line, ""));
    if !process_may_run(name, args) {
        return false;
    }
    let Some(command) = COMMANDS.iter().find(|c| c.name == name) else {
        return false;
    };
    crate::log!("shell: {}", line);
    let tid = sched::current_tid();
    let redirect = !matches!(stdout, Descriptor::Console);
    if redirect {
        REDIRECTS.lock().push((tid, stdout));
        REDIRECTING.fetch_add(1, Ordering::AcqRel);
    }
    (command.run)(args.trim());
    if redirect {
        REDIRECTS.lock().retain(|(t, _)| *t != tid);
        REDIRECTING.fetch_sub(1, Ordering::AcqRel);
    }
    true
}

fn allowed(path: &str, write: bool) -> bool {
    let uid = users::effective_uid();
    let ok = if write {
        users::can_write(uid, path)
    } else {
        users::can_read(uid, path)
    };
    if !ok {
        println!("{}: izin yok (yönetici için: sudo)", path);
    }
    ok
}

fn execute(line: &str) {
    crate::log!("shell: {}", line);
    let (name, args) = line.split_once(' ').unwrap_or((line, ""));
    if let Some(command) = COMMANDS.iter().find(|c| c.name == name) {
        (command.run)(args.trim());
        return;
    }
    let (path, background) = match args.trim().strip_suffix('&') {
        Some(rest) => (resolve_program(name), Some(rest.trim())),
        None => (resolve_program(name), None),
    };
    match path {
        Some(path) => exec(
            &path,
            background.unwrap_or(args.trim()),
            background.is_some(),
        ),
        None => println!("{}: command not found", name),
    }
}

fn resolve_program(name: &str) -> Option<alloc::string::String> {
    let path = if name.starts_with('/') {
        alloc::string::String::from(name)
    } else {
        alloc::format!("/bin/{}", name)
    };
    fs::stat(&path)
        .ok()
        .filter(|m| m.kind == fs::Kind::File)
        .map(|_| path)
}

fn exec(path: &str, args: &str, background: bool) {
    let mut argv: Vec<String> = alloc::vec![String::from(path.rsplit('/').next().unwrap_or(path))];
    argv.extend(args.split_whitespace().map(String::from));
    match elf::spawn(path, &argv) {
        Ok((process, done)) => {
            if background {
                println!("[{}] {}", process.pid, path);
            } else {
                drop(process);
                let code = done.wait();
                if code < 0 {
                    println!("[{} terminated: {}]", path, describe_exit(code));
                } else if code != 0 {
                    println!("[{} exited with {}]", path, code);
                }
            }
        }
        Err(error) => println!("{}: {}", path, error.message()),
    }
}

fn describe_exit(code: i64) -> alloc::string::String {
    if code == -9 {
        alloc::string::String::from("killed")
    } else if code <= -128 {
        alloc::format!("CPU exception {}", -code - 128)
    } else {
        alloc::format!("code {}", code)
    }
}

fn path_or_root(args: &str) -> alloc::string::String {
    fs::normalize("/", if args.is_empty() { "/" } else { args })
}

fn cmd_ls(args: &str) {
    let path = path_or_root(args);
    if !allowed(&path, false) {
        return;
    }
    match fs::read_dir(&path) {
        Ok(entries) => {
            for e in entries {
                match e.kind {
                    fs::Kind::Directory => println!("  {:<28} <dir>", e.name),
                    fs::Kind::Device => println!("  {:<28} <dev> {:>10}", e.name, e.size),
                    fs::Kind::File => println!("  {:<28} {:>10}", e.name, e.size),
                }
            }
        }
        Err(e) => println!("ls: {}: {}", path, e.message()),
    }
}

fn cmd_cat(args: &str) {
    for path in args.split_whitespace() {
        let path = path_or_root(path);
        if !allowed(&path, false) {
            continue;
        }
        match fs::read_all(&path) {
            Ok(data) => print!("{}", String::from_utf8_lossy(&data)),
            Err(e) => println!("cat: {}: {}", path, e.message()),
        }
    }
}

fn cmd_write(args: &str) {
    let Some((path, text)) = args.split_once(' ') else {
        println!("usage: write <file> <text>");
        return;
    };
    let path = path_or_root(path);
    if !allowed(&path, true) {
        return;
    }
    let mut data = String::from(text);
    data.push('\n');
    match fs::write_all(&path, data.as_bytes()) {
        Ok(()) => println!("wrote {} bytes to {}", data.len(), path),
        Err(e) => println!("write: {}: {}", path, e.message()),
    }
}

fn cmd_mkdir(args: &str) {
    let path = path_or_root(args);
    if !allowed(&path, true) {
        return;
    }
    if let Err(e) = fs::mkdir(&path) {
        println!("mkdir: {}: {}", path, e.message());
    }
}

fn cmd_rm(args: &str) {
    let path = path_or_root(args);
    if !allowed(&path, true) {
        return;
    }
    if let Err(e) = fs::remove(&path) {
        println!("rm: {}: {}", path, e.message());
    }
}

fn cmd_fsck(args: &str) {
    let repair = args.split_whitespace().any(|a| a == "-y");
    if repair && !require_admin() {
        return;
    }
    let point = args
        .split_whitespace()
        .find(|a| !a.starts_with('-'))
        .unwrap_or("/");
    let volume = match fs::resolve(point) {
        Ok((fs::Volume::Ext2(v), _)) => v,
        Ok(_) => {
            println!("fsck: {} ext2 değil", point);
            return;
        }
        Err(e) => {
            println!("fsck: {}: {:?}", point, e);
            return;
        }
    };
    println!(
        "fsck: {} denetleniyor{}",
        point,
        if repair { " (onarım açık)" } else { "" }
    );
    let result = volume
        .fs
        .lock()
        .check(repair, |line| println!("  {}", line));
    match result {
        Ok(r) => {
            println!(
                "fsck: {} dizin, {} dosya, {} inode; boş {} blok / {} inode",
                r.directories, r.files, r.inodes, r.free_blocks, r.free_inodes
            );
            if r.clean() {
                println!("fsck: temiz");
            } else if repair {
                println!(
                    "fsck: {} sorun bulundu, {} düzeltme yapıldı",
                    r.problems, r.fixed
                );
            } else {
                println!("fsck: sorunlar var; onarmak için: sudo fsck -y {}", point);
            }
        }
        Err(e) => println!("fsck: {:?}", e),
    }
}

fn cmd_df(_: &str) {
    println!(
        "  {:<16} {:<12} {:>9} {:>9} {:>9}",
        "mount", "device", "MiB", "used", "free"
    );
    for m in fs::mounts() {
        if let Ok((total, free)) = fs::capacity_of(&m.point) {
            println!(
                "  {:<16} {:<12} {:>9} {:>9} {:>9}",
                m.point,
                m.device,
                total >> 20,
                (total - free) >> 20,
                free >> 20
            );
        }
    }
}

fn cmd_disks(_: &str) {
    for disk in crate::storage::disks() {
        println!(
            "  {:<10} {:>8} MiB  {}{}",
            disk.name(),
            disk.sectors() / 2048,
            disk.model(),
            if disk.removable() { " (removable)" } else { "" }
        );
    }
    for volume in crate::storage::volumes() {
        let id = crate::storage::partition_info(&volume)
            .map(|p| alloc::format!("  PARTUUID={}", crate::storage::guid_text(&p.unique)))
            .unwrap_or_default();
        let mount = fs::mount_of(&volume.name())
            .map(|p| alloc::format!(" on {}", p))
            .unwrap_or_default();
        println!(
            "  {:<10} {:>8} MiB  {}{}{}",
            volume.name(),
            volume.sectors() / 2048,
            volume.model(),
            mount,
            id
        );
    }
}

fn next_key() -> keyboard::KeyEvent {
    keyboard::wait_event()
}

fn replace_line(line: &mut String, with: &str) {
    for _ in line.chars() {
        print!("\x08");
    }
    line.clear();
    line.push_str(with);
    print!("{}", line);
}

fn read_line(history: &mut [String]) -> String {
    console::set_color(console::CYAN);
    print!("{}", prompt());
    console::set_color(console::WHITE);
    console::set_cursor(true);

    let mut line = String::new();
    // İmleç: satırdaki karakter sırası.
    let mut at = 0usize;
    let mut history_index = history.len();
    let byte = |line: &String, i: usize| line.char_indices().nth(i).map_or(line.len(), |(b, _)| b);
    // İmleçten sonrasını yeniden yazar (`erase` kadar fazladan boşlukla) ve imleci geri getirir.
    let redraw_tail = |line: &String, at: usize, erase: usize| {
        let tail: String = line.chars().skip(at).collect();
        print!("{}{}", tail, " ".repeat(erase));
        console::move_cursor(-((tail.chars().count() + erase) as isize));
    };
    loop {
        let event = next_key();
        match event.key {
            Key::Enter => {
                println!();
                break;
            }
            Key::Backspace if at > 0 => {
                at -= 1;
                let b = byte(&line, at);
                line.remove(b);
                console::move_cursor(-1);
                redraw_tail(&line, at, 1);
            }
            Key::Delete if at < line.chars().count() => {
                let b = byte(&line, at);
                line.remove(b);
                redraw_tail(&line, at, 1);
            }
            Key::Left if at > 0 => {
                at -= 1;
                console::move_cursor(-1);
            }
            Key::Right if at < line.chars().count() => {
                at += 1;
                console::move_cursor(1);
            }
            Key::Home => {
                console::move_cursor(-(at as isize));
                at = 0;
            }
            Key::End => {
                let len = line.chars().count();
                console::move_cursor((len - at) as isize);
                at = len;
            }
            Key::Up if history_index > 0 => {
                history_index -= 1;
                console::move_cursor((line.chars().count() - at) as isize);
                replace_line(&mut line, &history[history_index]);
                at = line.chars().count();
            }
            Key::Down if history_index < history.len() => {
                history_index += 1;
                let next = history.get(history_index).cloned().unwrap_or_default();
                console::move_cursor((line.chars().count() - at) as isize);
                replace_line(&mut line, &next);
                at = line.chars().count();
            }
            Key::Char('c') if event.modifiers.ctrl => {
                println!("^C");
                line.clear();
                break;
            }
            Key::Char('l') if event.modifiers.ctrl => {
                console::clear();
                print!("{}{}", prompt(), line);
                at = line.chars().count();
            }
            Key::Char(c) if !c.is_control() && line.len() < 200 => {
                let b = byte(&line, at);
                line.insert(b, c);
                print!("{}", c);
                at += 1;
                redraw_tail(&line, at, 0);
            }
            _ => {}
        }
    }
    console::set_cursor(false);
    line
}

fn cmd_whoami(_: &str) {
    let uid = users::effective_uid();
    match users::session() {
        Some(s) if s.uid == uid => println!("{} (uid {}, ev {})", s.name, uid, s.home),
        _ => println!("{} (uid {})", users::name_of(uid), uid),
    }
}

fn cmd_users(_: &str) {
    for (name, uid, admin) in users::list() {
        println!(
            "  {:<16} uid {:<6} {}",
            name,
            uid,
            if admin { "yönetici" } else { "" }
        );
    }
}

fn require_admin() -> bool {
    let admin =
        users::effective_uid() == users::ROOT_UID || users::session().is_some_and(|s| s.admin);
    if !admin {
        println!("bu işlem için yönetici olmalısın");
    }
    admin
}

fn ask_new_password() -> Option<String> {
    let password = read_secret("yeni parola: ");
    let again = read_secret("yeni parola (tekrar): ");
    if password != again {
        println!("Parolalar eşleşmiyor.");
        return None;
    }
    Some(password)
}

fn cmd_useradd(args: &str) {
    if !require_admin() {
        return;
    }
    let mut parts = args.split_whitespace();
    let Some(name) = parts.next() else {
        println!("usage: useradd <name> [admin]");
        return;
    };
    let admin = parts.next() == Some("admin");
    let Some(password) = ask_new_password() else {
        return;
    };
    match users::create(name, &password, admin) {
        Ok(user) => println!(
            "{} oluşturuldu (uid {}, ev {})",
            user.name, user.uid, user.home
        ),
        Err(e) => println!("useradd: {}", e),
    }
}

fn cmd_userdel(args: &str) {
    if !require_admin() {
        return;
    }
    let name = args.trim();
    if users::session().is_some_and(|s| s.name == name) {
        println!("userdel: oturumu açık kullanıcı silinemez");
        return;
    }
    match users::remove(name) {
        Ok(()) => println!("{} silindi (ev dizini korunur)", name),
        Err(e) => println!("userdel: {}", e),
    }
}

fn cmd_passwd(args: &str) {
    let Some(session) = users::session() else {
        return;
    };
    let target = if args.trim().is_empty() {
        session.name.clone()
    } else {
        String::from(args.trim())
    };
    if target != session.name {
        if !require_admin() {
            return;
        }
    } else if users::effective_uid() != users::ROOT_UID {
        let current = read_secret("mevcut parola: ");
        if users::authenticate(&session.name, &current).is_none() {
            println!("passwd: parola hatalı");
            return;
        }
    }
    let Some(password) = ask_new_password() else {
        return;
    };
    match users::set_password(&target, &password) {
        Ok(()) => println!("{} için parola değiştirildi", target),
        Err(e) => println!("passwd: {}", e),
    }
}

fn cmd_sudo(args: &str) {
    let Some(session) = users::session() else {
        return;
    };
    if args.trim().is_empty() {
        println!("usage: sudo <command>");
        return;
    }
    if !session.admin {
        println!("sudo: {} yönetici değil", session.name);
        return;
    }
    let password = read_secret(&alloc::format!("[sudo] {} parolası: ", session.name));
    if users::authenticate(&session.name, &password).is_none() {
        crate::log!("sudo: failed for '{}'", session.name);
        println!("sudo: parola hatalı");
        return;
    }
    crate::log!("sudo: {} ran '{}'", session.name, args.trim());
    users::set_elevated(true);
    execute(args.trim());
    users::set_elevated(false);
}

fn cmd_logout(_: &str) {
    users::end_session();
}

fn cmd_lsusb(_: &str) {
    let list = crate::drivers::usb::devices();
    if list.is_empty() {
        println!("no USB devices");
        return;
    }
    for a in list {
        let d = &a.device;
        println!(
            "  port {:<2} {:04x}:{:04x}  {:<11} {:<28} {}",
            d.port,
            d.vendor,
            d.product,
            d.speed.name(),
            d.name,
            if a.drivers.is_empty() {
                String::from("-")
            } else {
                a.drivers.join(", ")
            }
        );
    }
}

fn cmd_mount(_: &str) {
    for m in fs::mounts() {
        println!(
            "  {:<16} {:<12} {:<6} '{}'",
            m.point, m.device, m.kind, m.label
        );
    }
}

fn cmd_umount(args: &str) {
    let point = fs::normalize("/", args.trim());
    match fs::unmount(&point) {
        Ok(()) => println!("umount: {} unmounted", point),
        Err(e) => println!("umount: {}: {}", point, e.message()),
    }
}

fn cmd_lsmod(_: &str) {
    let list = crate::kernel::module::loaded();
    if list.is_empty() {
        println!("no modules loaded ({})", crate::kernel::module::DIR);
    }
    for (name, base, size) in list {
        println!("{:<20} {:#x} {} KiB", name, base, size / 1024);
    }
}

fn cmd_insmod(args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println!("usage: insmod <name|path.bko>");
        return;
    }
    if crate::kernel::users::effective_uid() != crate::kernel::users::ROOT_UID {
        println!("insmod: yönetici yetkisi gerekir (sudo)");
        return;
    }
    let path = if name.contains('/') {
        String::from(name)
    } else {
        alloc::format!(
            "{}/{}.bko",
            crate::kernel::module::DIR,
            name.trim_end_matches(".bko")
        )
    };
    match crate::kernel::module::load(&path) {
        Ok(()) => println!("insmod: {} loaded", name),
        Err(e) => println!("insmod: {}", e),
    }
}

fn cmd_netstat(_: &str) {
    let sockets = crate::net::tcp_sockets();
    if sockets.is_empty() {
        println!("no TCP sockets");
        return;
    }
    println!(
        "  {:<5} {:<13} {:>6}  {:<21} {:>7} {:>7} {:>6} {:>6} {:>7} {:>4}",
        "id", "state", "local", "remote", "send-q", "recv-q", "srtt", "rto", "cwnd", "rtx"
    );
    for s in sockets {
        let remote = if s.remote_port == 0 {
            String::from("*")
        } else {
            alloc::format!("{}:{}", s.remote, s.remote_port)
        };
        let srtt = s
            .srtt_ms
            .map_or(String::from("-"), |ms| alloc::format!("{}ms", ms));
        println!(
            "  {:<5} {:<13} {:>6}  {:<21} {:>7} {:>7} {:>6} {:>6} {:>7} {:>4}",
            s.id,
            s.state.name(),
            s.local_port,
            remote,
            s.send_queue,
            s.receive_queue,
            srtt,
            alloc::format!("{}ms", s.rto_ms),
            s.cwnd,
            s.retransmits
        );
    }
}

fn cmd_layout(args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println!("layout: {}", keyboard::layout().name());
        return;
    }
    match keyboard::Layout::from_name(name) {
        Some(layout) => {
            keyboard::set_layout(layout);
            println!("layout: {}", layout.name());
        }
        None => println!("usage: keymap [tr|us]"),
    }
}

fn cmd_help(_: &str) {
    let from_terminal = sched::current_process().is_some();
    if from_terminal {
        println!("Kernel commands:");
    }
    for command in COMMANDS
        .iter()
        .filter(|c| !from_terminal || PROCESS_COMMANDS.contains(&c.name))
    {
        println!("  {:<14} {}", command.name, command.help);
    }
}

fn cmd_mem(_: &str) {
    let stats = pmm::stats();
    let used = stats.total_bytes - stats.free_bytes;
    println!(
        "physical: {} MiB total, {} MiB used, {} MiB free",
        stats.total_bytes / (1024 * 1024),
        used / (1024 * 1024),
        stats.free_bytes / (1024 * 1024)
    );
    println!("heap:     {} KiB mapped", heap::mapped_bytes() / 1024);
}

fn cmd_uptime(_: &str) {
    let ms = pit::uptime_ms();
    let s = ms / 1000;
    println!(
        "up {}h {}m {}s ({} ms)",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ms
    );
}

fn cmd_date(_: &str) {
    match rtc::now() {
        Some(time) => println!("{} (unix {})", time, time.unix_timestamp()),
        None => println!("RTC unreadable"),
    }
}

fn cmd_cpu(_: &str) {
    use crate::arch::{apic, cpuinfo, smp, tsc};
    let info = cpuinfo::read();
    println!("model:    {} ({})", info.brand, info.vendor);
    println!(
        "family {:#x} model {:#x} stepping {}, TSC {} MHz{}",
        info.family,
        info.model,
        info.stepping,
        tsc::mhz(),
        info.base_mhz
            .map(|m| alloc::format!(", base {} MHz", m))
            .unwrap_or_default()
    );
    if let Some(h) = &info.hypervisor {
        println!("hypervisor: {}", h);
    }
    for (level, kind, kib) in &info.caches {
        println!("cache:    L{} {} {} KiB", level, kind, kib);
    }
    match crate::drivers::cpupower::temperature() {
        Some(t) => println!("temperature: {} C", t),
        None => println!("temperature: no digital thermal sensor"),
    }
    println!(
        "logical processors per package: {}",
        info.logical_per_package.max(1)
    );
    println!("features: {}", info.features.join(" "));
    println!(
        "interrupts: {}, LAPIC timer {} kHz",
        if apic::active() {
            "LAPIC + I/O APIC"
        } else {
            "8259 PIC"
        },
        apic::ticks_per_ms()
    );
    let loads = sched::cpu_loads();
    for (c, load) in sched::cpu_stats()
        .iter()
        .zip(loads.iter().chain(core::iter::repeat(&0)))
    {
        println!(
            "  cpu{:<2} lapic {:>2}  busy {:>3}%  {:>7} switches  running {}",
            c.index, c.lapic_id, load, c.switches, c.current
        );
    }
    println!(
        "  {} of {} CPU(s) scheduling work",
        sched::online_cpus(),
        smp::online_count()
    );
}

fn cmd_lspci(_: &str) {
    for d in pci::devices() {
        println!(
            "{:02x}:{:02x}.{} {:04x}:{:04x} {}",
            d.bus,
            d.slot,
            d.function,
            d.vendor_id,
            d.device_id,
            d.class_name()
        );
    }
}

fn cmd_acpi(_: &str) {
    let shown = acpi::with(|acpi| {
        println!(
            "ACPI revision {}, OEM '{}'",
            acpi.revision,
            core::str::from_utf8(&acpi.oem_id).unwrap_or("").trim_end()
        );
        for table in &acpi.tables {
            println!(
                "  {} rev {:<2} {:>6} bytes  OEM '{}'",
                table.signature_str(),
                table.revision,
                table.length,
                table.oem_str()
            );
        }
        if let Some(fadt) = acpi.fadt {
            println!(
                "platform: {}, SCI IRQ {}, 8042 {}",
                fadt.profile_name(),
                fadt.sci_interrupt,
                if fadt.boot_arch & acpi::Fadt::BOOT_ARCH_8042 != 0 {
                    "declared"
                } else {
                    "not declared"
                }
            );
        }
        if let Some(madt) = acpi.madt {
            println!(
                "MADT: {} CPU(s), {} I/O APIC(s), LAPIC at {:#x}, 8259 PICs {}",
                madt.enabled_cpus,
                madt.io_apics,
                madt.local_apic_address,
                if madt.legacy_pics {
                    "present"
                } else {
                    "absent"
                }
            );
        }
        println!(
            "S5 sleep type: {}",
            if acpi.s5.is_some() {
                "found (ACPI power-off available)"
            } else {
                "not found"
            }
        );
    });
    if shown.is_none() {
        println!("ACPI is not available");
    }
}

fn cmd_battery(_: &str) {
    let batteries = battery::batteries();
    if batteries.is_empty() {
        println!("No battery found.");
    }
    for b in &batteries {
        if !b.present {
            println!("{}: not inserted", b.path);
            continue;
        }
        let unit = if b.power_unit_mwh { "mWh" } else { "mAh" };
        println!(
            "{}: {}% {}{}",
            b.path,
            b.percent().map(|p| p as i32).unwrap_or(-1),
            b.state_name(),
            if b.critical { " (CRITICAL)" } else { "" }
        );
        println!(
            "  remaining {} / full {} / design {} {}",
            b.remaining.unwrap_or(0),
            b.full_capacity.unwrap_or(0),
            b.design_capacity.unwrap_or(0),
            unit
        );
        if let Some(minutes) = b.minutes_left() {
            println!(
                "  {}h {:02}m {}",
                minutes / 60,
                minutes % 60,
                if b.charging { "until full" } else { "left" }
            );
        }
        if !b.model.is_empty() || !b.oem.is_empty() {
            println!(
                "  {} {} ({}) serial {}",
                b.oem, b.model, b.technology, b.serial
            );
        }
    }
    match battery::ac_online() {
        Some(true) => println!("AC adapter: online"),
        Some(false) => println!("AC adapter: offline"),
        None => println!("AC adapter: not described by firmware"),
    }
}

fn cmd_aml(args: &str) {
    let (sub, rest) = args.split_once(' ').unwrap_or((args, ""));
    match sub {
        "eval" if !rest.is_empty() => match aml::evaluate(rest.trim(), Vec::new()) {
            Ok(value) => println!("{:?}", value),
            Err(error) => println!("error: {:?}", error),
        },
        "devices" => {
            for hid in [
                "PNP0A03", "PNP0A08", "PNP0C09", "PNP0C0A", "ACPI0003", "PNP0C0C", "PNP0C0D",
                "PNP0303", "PNP0F13", "PNP0B00", "PNP0501",
            ] {
                for node in aml::find_devices(hid) {
                    println!("  {:<8} {}", hid, aml::path_of(node));
                }
            }
        }
        _ => {
            println!("{} AML objects loaded", aml::object_count());
            println!("usage: aml eval <\\path>   aml devices");
        }
    }
}

fn cmd_desktop(_: &str) {
    if recovery::is_active() {
        println!("desktop: kurtarma kipinde masaüstü yok (normal açılış için: reboot)");
        return;
    }
    match crate::gui::start() {
        Ok(()) => println!("Desktop started. Type 'console' or close this window to return."),
        Err(reason) => println!("desktop: {}", reason),
    }
}

fn cmd_power(args: &str) {
    use crate::drivers::{cpupower, power};
    let mut parts = args.split_whitespace();
    if parts.next() == Some("profile") {
        match parts.next().and_then(cpupower::Profile::parse) {
            Some(p) => {
                if cpupower::set_profile(p) {
                    println!("profile: {}", p.name());
                } else {
                    println!(
                        "profile: {} (no HWP; frequency is managed by firmware)",
                        p.name()
                    );
                }
            }
            None => println!("usage: power profile performance|balanced|powersave"),
        }
        return;
    }
    println!(
        "CPU:        {} ({})",
        cpupower::profile().name(),
        if cpupower::hwp_available() {
            "Intel HWP"
        } else {
            "firmware managed"
        }
    );
    match cpupower::temperature() {
        Some(t) => println!("temperature: {} C", t),
        None => println!("temperature: no sensor"),
    }
    let batteries = crate::drivers::battery::batteries();
    match batteries.first() {
        Some(b) => println!(
            "battery:    {}% {}",
            b.percent().unwrap_or(0),
            b.state_name()
        ),
        None => println!("battery:    none (AC powered)"),
    }
    match power::lid_open() {
        Some(open) => println!("lid:        {}", if open { "open" } else { "closed" }),
        None => println!("lid:        none"),
    }
    match power::brightness() {
        Some(b) => println!("brightness: {}%", b),
        None => println!("brightness: no ACPI backlight control"),
    }
}

fn cmd_brightness(args: &str) {
    use crate::drivers::power;
    if let Ok(v) = args.trim().parse::<u32>()
        && !power::set_brightness(v)
    {
        println!("brightness: no ACPI backlight control on this machine");
        return;
    }
    match power::brightness() {
        Some(b) => println!("brightness: {}%", b),
        None => println!("brightness: no ACPI backlight control on this machine"),
    }
}

fn cmd_volume(args: &str) {
    if let Ok(v) = args.trim().parse::<u32>() {
        crate::drivers::audio::set_volume(v);
    }
    println!(
        "volume: {}%{}",
        crate::drivers::audio::volume(),
        if crate::drivers::audio::present() {
            ""
        } else {
            " (no HD Audio; PC speaker only)"
        }
    );
}

fn cmd_beep(args: &str) {
    let mut parts = args.split_whitespace().map(|p| p.parse::<u32>().ok());
    let frequency = parts.next().flatten().unwrap_or(880);
    let ms = parts.next().flatten().unwrap_or(200).min(5000);
    crate::drivers::audio::beep(frequency, ms);
}

fn cmd_fps(args: &str) {
    if args.trim() == "dock" {
        for (cold, label) in [(true, "first hover (cold cache)"), (false, "warm")] {
            match crate::gui::dock_benchmark(60, cold) {
                Some((avg, worst)) => {
                    println!("dock {}: {} us/frame, worst {} us", label, avg, worst)
                }
                None => println!("fps dock: the desktop is not running"),
            }
        }
        return;
    }
    if args.trim() == "drag" {
        match crate::gui::drag_benchmark(80) {
            Some(us) => println!("window drag: {} us/frame", us),
            None => println!("fps drag: no window"),
        }
        return;
    }
    if args.trim() == "bench" {
        match crate::gui::benchmark(20) {
            Some(b) => println!(
                "full scene (compose + present): 1 CPU {} us, {} CPU(s) {} us ({}.{}x) = compose {} us + present {} us",
                b.single,
                crate::gfx::parallel::workers() + 1,
                b.parallel,
                b.single / b.parallel.max(1),
                b.single * 10 / b.parallel.max(1) % 10,
                b.compose,
                b.present
            ),
            None => println!("fps bench: the desktop is not running"),
        }
        return;
    }
    let stats = crate::gui::stats();
    println!(
        "full-frame redraw: {} us (budget 16667 us at 60 Hz), write-combining {}",
        crate::gfx::display::full_frame_ns() / 1000,
        if crate::gfx::display::write_combining() {
            "on"
        } else {
            "off"
        }
    );
    if crate::gui::is_running() {
        println!(
            "compositor: {} fps, {} us/frame, {} frames ({} cursor-only), {} KiB scene redraw",
            stats.fps,
            stats.frame_time_us,
            stats.frames,
            stats.cursor_only_frames,
            stats.scene_pixels * 4 / 1024
        );
    } else {
        println!("compositor: not running (start it with 'desktop')");
    }
}

fn cmd_gpu(args: &str) {
    use crate::drivers::gpu;
    let mut parts = args.split_whitespace();
    match parts.next() {
        None => println!("{}", gpu::describe()),
        Some("modes") => {
            for (w, h) in gpu::modes() {
                print!("{}x{}  ", w, h);
            }
            println!();
        }
        Some("mode") => {
            let parsed = parts
                .next()
                .and_then(|m| m.split_once('x'))
                .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
            match parsed {
                Some((w, h)) => match gpu::set_mode(w, h) {
                    Ok(()) => println!("{}", gpu::describe()),
                    Err(e) => println!("gpu: {}", e),
                },
                None => println!("usage: gpu mode <width>x<height>"),
            }
        }
        Some(_) => println!("usage: gpu | gpu modes | gpu mode <W>x<H>"),
    }
}

fn cmd_net(_: &str) {
    use crate::net::{self, wire::Mac};
    let Some(r) = net::report() else {
        println!("no network interface");
        return;
    };
    println!("eth0: {} {}", r.model, Mac(&r.mac));
    if r.link_up {
        println!(
            "  link up, {} Mbit/s {} duplex",
            r.speed,
            if r.full_duplex { "full" } else { "half" }
        );
    } else {
        println!("  link down");
    }
    match &r.config {
        Some(c) => {
            let age = (net::now_ms() - c.acquired_ms) / 1000;
            println!(
                "  inet {}/{}  gateway {}",
                c.address,
                c.netmask.prefix_len(),
                c.gateway
            );
            println!(
                "  dns {:?}{}",
                c.dns,
                if c.domain.is_empty() {
                    alloc::string::String::new()
                } else {
                    alloc::format!("  domain {}", c.domain)
                }
            );
            println!(
                "  DHCP lease from {}: {} s, renews in {} s",
                c.server,
                c.lease_seconds,
                (c.lease_seconds as u64 / 2).saturating_sub(age)
            );
        }
        None => println!("  no address ({})", r.dhcp_state),
    }
    let s = r.stats;
    println!(
        "  rx {} packets / {} KiB, tx {} packets / {} KiB",
        s.rx_packets,
        s.rx_bytes / 1024,
        s.tx_packets,
        s.tx_bytes / 1024
    );
    println!(
        "  dropped rx {} tx {}, errors rx {} crc {} missed {}, link changes {}",
        s.rx_dropped, s.tx_dropped, r.rx_errors, r.crc_errors, r.missed, s.link_changes
    );
    for (ip, mac, age) in r.arp {
        println!(
            "  arp {:<15} {}  {} s",
            alloc::format!("{}", ip),
            Mac(&mac),
            age
        );
    }
}

fn cmd_ping(args: &str) {
    let mut parts = args.split_whitespace();
    let Some(target) = parts.next() else {
        println!("usage: ping <host|ip> [count]");
        return;
    };
    let mut count: u16 = parts
        .next()
        .and_then(|c| c.parse().ok())
        .unwrap_or(4)
        .clamp(1, 100);
    if crate::net::present() && crate::net::config().is_none() {
        println!("waiting for the network to come up...");
        crate::net::wait_configured(10_000);
    }
    let address = match crate::net::dns::resolve(target) {
        Ok(list) => list[0],
        Err(e) => {
            println!("ping: {}: {}", target, e.message());
            return;
        }
    };
    println!("PING {} ({})", target, address);
    let mut received = 0;
    let mut total = 0;
    for seq in 1..=count {
        match crate::net::ping(address, seq, 2000) {
            Ok(rtt) => {
                received += 1;
                total += rtt;
                println!(
                    "  reply from {}: seq={} time={}.{:03} ms",
                    address,
                    seq,
                    rtt / 1000,
                    rtt % 1000
                );
            }
            Err(e) => println!("  seq={}: {}", seq, e),
        }
        if sched::pending_signal().is_some() {
            count = seq;
            break;
        }
        if seq < count {
            sched::sleep_ms(1000);
        }
    }
    println!(
        "{} sent, {} received, {}% loss{}",
        count,
        received,
        (count as u32 - received as u32) * 100 / count as u32,
        if received > 0 {
            alloc::format!(", avg {} us", total / received as u64)
        } else {
            alloc::string::String::new()
        }
    );
}

fn cmd_host(args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println!("usage: host <name>");
        return;
    }
    match crate::net::dns::resolve(name) {
        Ok(addresses) => {
            for a in addresses {
                println!("{} has address {}", name, a);
            }
        }
        Err(e) => println!("host: {}: {}", name, e.message()),
    }
}

fn cmd_dhcp(args: &str) {
    match args.trim() {
        "renew" => {
            crate::net::dhcp_renew();
            if crate::net::wait_configured(5000) {
                println!("lease renewed");
            } else {
                println!("no lease yet");
            }
        }
        "release" => {
            crate::net::dhcp_release();
            println!("address released");
        }
        _ => println!("usage: dhcp renew | dhcp release"),
    }
}

fn cmd_ps(_: &str) {
    println!("  TID  PID  CPU  STATE     CPU(ms)  NAME");
    for t in sched::threads() {
        println!(
            "{:>5} {:>4}  {:>3}  {:<9} {:>7}  {}",
            t.tid,
            t.pid,
            t.cpu,
            t.state.name(),
            t.cpu_ms,
            t.name
        );
    }

    for p in sched::process::list().iter().filter(|p| p.running) {
        println!("  process {:>4}  {}", p.pid, p.name);
    }
    println!(
        "{} live processes, {} context switches",
        sched::process::live_count(),
        sched::context_switches()
    );
}

fn cmd_kill(args: &str) {
    match args.trim().parse::<u64>() {
        Ok(pid) if sched::kill_process(pid) => println!("killed pid {}", pid),
        Ok(pid) => println!("kill: no process {}", pid),
        Err(_) => println!("usage: kill <pid>"),
    }
}

fn cmd_programs(_: &str) {
    for program in usermode::PROGRAMS.iter() {
        println!("  {:<11} {}", program.name, program.description);
    }
}

fn cmd_run(args: &str) {
    let (name, background) = match args.strip_suffix('&') {
        Some(name) => (name.trim(), true),
        None => (args, false),
    };
    let Some(program) = usermode::find(name) else {
        println!("usage: run <program> [&]  (see 'programs')");
        return;
    };
    if background {
        match usermode::spawn(program) {
            Ok((process, _)) => {
                println!("[{}] {} started in background", process.pid, program.name)
            }
            Err(reason) => println!("run: {}", reason),
        }
        return;
    }
    match usermode::run(program) {
        Ok(code) if code >= 0 => println!("[{} exited with {}]", program.name, code),
        Ok(code) => println!("[{} killed by CPU exception {}]", program.name, -code - 128),
        Err(reason) => println!("run: {}", reason),
    }
}
