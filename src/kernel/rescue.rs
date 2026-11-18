//! Kurtarma araçları (çekirdek kabuğu komutları): günlük, donanım özeti, önyükleme
//! ayarları, önceki çekirdeğe dönüş, yedekleme/geri yükleme ve fabrika ayarları.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs;
use crate::kernel::{recovery, users};

const BOOT_CONFIG: &str = "/boot/boot/banna.cfg";
const KERNEL: &str = "/boot/boot/banna_os";
const PREVIOUS_KERNEL: &str = "/boot/boot/banna_os.old";

fn root_only(command: &str) -> bool {
    if users::effective_uid() != users::ROOT_UID {
        println!(
            "{}: yönetici yetkisi gerekir (sudo ya da kurtarma kipi)",
            command
        );
        return false;
    }
    true
}

fn recovery_only(command: &str) -> bool {
    if !recovery::is_active() {
        println!(
            "{}: yalnızca kurtarma kipinde (açılışta B tuşu) kullanılabilir",
            command
        );
        return false;
    }
    true
}

/// Kullanıcıdan `word` yazmasını ister; doğru yazılırsa `true`.
fn confirm(question: &str, word: &str) -> bool {
    println!("{}", question);
    print!("Onaylamak için '{}' yazın: ", word);
    super::shell::read_line_plain().trim() == word
}

fn copy_tree(from: &str, to: &str, counts: &mut (usize, usize)) {
    let Ok(meta) = fs::stat(from) else {
        counts.1 += 1;
        return;
    };
    if meta.kind == fs::Kind::Directory {
        if fs::stat(to).is_err() && fs::mkdir(to).is_err() {
            println!("  ! {}", to);
            counts.1 += 1;
            return;
        }
        for entry in fs::read_dir(from).unwrap_or_default() {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            copy_tree(
                &format!("{}/{}", from.trim_end_matches('/'), entry.name),
                &format!("{}/{}", to.trim_end_matches('/'), entry.name),
                counts,
            );
        }
        return;
    }
    match fs::read_all(from).map(|data| fs::write_all(to, &data)) {
        Ok(Ok(())) => counts.0 += 1,
        _ => {
            println!("  ! {}", from);
            counts.1 += 1;
        }
    }
}

fn remove_tree(path: &str) -> usize {
    let mut failed = 0;
    if fs::stat(path).is_ok_and(|m| m.kind == fs::Kind::Directory) {
        for entry in fs::read_dir(path).unwrap_or_default() {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            failed += remove_tree(&format!("{}/{}", path.trim_end_matches('/'), entry.name));
        }
    }
    if fs::remove(path).is_err() {
        failed += 1;
    }
    failed
}

/// `dmesg [satır]` ya da `dmesg save [yol]`
pub fn cmd_dmesg(args: &str) {
    let mut words = args.split_whitespace();
    if words.clone().next() == Some("save") {
        let path = words.nth(1).unwrap_or("/boot/boot/kernel.log");
        if crate::log::save_boot_log(path) {
            println!("günlük kaydedildi: {}", path);
        } else {
            println!("dmesg: {} yazılamadı", path);
        }
        return;
    }
    let lines = words.next().and_then(|n| n.parse().ok()).unwrap_or(40usize);
    let log = crate::log::boot_log();
    let text = String::from_utf8_lossy(&log);
    let all: Vec<&str> = text.lines().collect();
    for line in &all[all.len().saturating_sub(lines)..] {
        println!("{}", line);
    }
}

/// Donanım özeti.
pub fn cmd_hwinfo(_: &str) {
    let cpu = crate::arch::cpuinfo::read();
    let online = crate::sched::online_cpus();
    let efficient = crate::sched::efficiency_cpus();
    println!("CPU:      {}", cpu.brand.trim());
    if efficient > 0 {
        println!(
            "          {} threads ({} P + {} E)",
            online,
            online - efficient,
            efficient
        );
    } else {
        println!("          {} threads", online);
    }
    let mem = crate::memory::pmm::stats();
    println!("Memory:   {} MiB", mem.total_bytes / (1024 * 1024));
    if let Some((w, h)) = crate::gfx::display::size() {
        println!(
            "Display:  {}x{} ({:?})",
            w,
            h,
            crate::drivers::gpu::adapter()
        );
    }
    if let Some(level) = crate::drivers::power::brightness() {
        println!("Backlight: {}%", level);
    }
    println!(
        "Audio:    {}",
        if crate::drivers::audio::present() {
            "HD Audio"
        } else {
            "PC speaker"
        }
    );
    println!("PCI:");
    for d in crate::drivers::pci::devices() {
        println!(
            "  {:02x}:{:02x}.{} {:04x}:{:04x} {}",
            d.bus,
            d.slot,
            d.function,
            d.vendor_id,
            d.device_id,
            d.class_name()
        );
    }
    println!("Storage:");
    for disk in crate::storage::disks() {
        println!(
            "  {:<10} {:>8} MiB  {}",
            disk.name(),
            disk.sectors() / 2048,
            disk.model()
        );
    }
}

fn read_config() -> Vec<(String, String)> {
    let data = fs::read_all(BOOT_CONFIG).unwrap_or_default();
    String::from_utf8_lossy(&data)
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (String::from(k.trim()), String::from(v.trim())))
        .collect()
}

fn write_config(entries: &[(String, String)]) -> bool {
    let mut text = String::from("# Banna OS önyükleme ayarları\n");
    for (k, v) in entries {
        text.push_str(&format!("{}={}\n", k, v));
    }
    fs::write_all(BOOT_CONFIG, text.as_bytes()).is_ok()
}

/// `bootcfg` | `bootcfg timeout <sn>` | `bootcfg cmdline <metin>` | `bootcfg add|remove <bayrak>`
pub fn cmd_bootcfg(args: &str) {
    let mut config = read_config();
    let (action, value) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
    if action.is_empty() {
        if config.is_empty() {
            println!("bootcfg: {} okunamadı", BOOT_CONFIG);
        }
        for (k, v) in &config {
            println!("{} = {}", k, v);
        }
        println!(
            "previous kernel: {}",
            if fs::stat(PREVIOUS_KERNEL).is_ok() {
                "available (rollback)"
            } else {
                "none"
            }
        );
        return;
    }
    if !root_only("bootcfg") {
        return;
    }
    let mut set = |key: &str, new: String| match config.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = new,
        None => config.push((String::from(key), new)),
    };
    let cmdline = read_config()
        .into_iter()
        .find(|(k, _)| k == "cmdline")
        .map(|(_, v)| v)
        .unwrap_or_default();
    let value = value.trim();
    match action {
        "timeout" if value.parse::<u32>().is_ok() => set("timeout", String::from(value)),
        "cmdline" => set("cmdline", String::from(value)),
        "add" if !value.is_empty() => {
            if !cmdline.split_whitespace().any(|w| w == value) {
                set("cmdline", format!("{} {}", cmdline, value).trim().into());
            }
        }
        "remove" if !value.is_empty() => {
            let kept: Vec<&str> = cmdline.split_whitespace().filter(|w| *w != value).collect();
            set("cmdline", kept.join(" "));
        }
        _ => {
            println!("usage: bootcfg [timeout <s> | cmdline <text> | add <flag> | remove <flag>]");
            return;
        }
    }
    if write_config(&config) {
        println!("bootcfg: kaydedildi, yeniden başlatınca geçerli olur");
    } else {
        println!("bootcfg: {} yazılamadı", BOOT_CONFIG);
    }
}

/// Önceki çekirdeği varsayılan yapar (güncelleme sonrası sorun çıkarsa).
pub fn cmd_rollback(_: &str) {
    if !recovery_only("rollback") || !root_only("rollback") {
        return;
    }
    if fs::stat(PREVIOUS_KERNEL).is_err() {
        println!("rollback: önceki çekirdek yok ({})", PREVIOUS_KERNEL);
        return;
    }
    if !confirm("Önceki çekirdek varsayılan yapılacak.", "rollback") {
        println!("rollback: iptal edildi");
        return;
    }
    let swap = "/boot/boot/banna_os.swap";
    let ok = fs::rename(KERNEL, swap).is_ok()
        && fs::rename(PREVIOUS_KERNEL, KERNEL).is_ok()
        && fs::rename(swap, PREVIOUS_KERNEL).is_ok();
    if ok {
        println!("rollback: tamam, yeniden başlatın (reboot)");
    } else {
        println!("rollback: dosyalar değiştirilemedi");
    }
}

/// `/home` ve `/etc`'yi hedef klasöre (ör. USB bellek) kopyalar.
pub fn cmd_backup(args: &str) {
    let target = args.trim();
    if target.is_empty() {
        println!("usage: backup <directory>   (ör. backup /media/usb1)");
        return;
    }
    if !root_only("backup") {
        return;
    }
    let stamp = crate::drivers::rtc::now()
        .map(|t| {
            format!(
                "{:04}{:02}{:02}-{:02}{:02}",
                t.year, t.month, t.day, t.hour, t.minute
            )
        })
        .unwrap_or_else(|| String::from("backup"));
    let dir = format!("{}/banna-backup-{}", target.trim_end_matches('/'), stamp);
    if fs::mkdir(&dir).is_err() {
        println!("backup: {} oluşturulamadı", dir);
        return;
    }
    let mut counts = (0, 0);
    for part in ["home", "etc"] {
        copy_tree(
            &format!("/{}", part),
            &format!("{}/{}", dir, part),
            &mut counts,
        );
    }
    println!("backup: {} dosya → {} ({} hata)", counts.0, dir, counts.1);
}

/// `backup` ile alınan yedeği geri yükler.
pub fn cmd_restore(args: &str) {
    let source = args.trim().trim_end_matches('/');
    if source.is_empty() {
        println!("usage: restore <backup directory>");
        return;
    }
    if !recovery_only("restore") || !root_only("restore") {
        return;
    }
    if fs::stat(&format!("{}/etc", source)).is_err() {
        println!("restore: {} bir Banna yedeği değil (etc/ yok)", source);
        return;
    }
    if !confirm(
        "Yedekteki /home ve /etc dosyaları mevcutların üzerine yazılacak.",
        "restore",
    ) {
        println!("restore: iptal edildi");
        return;
    }
    let mut counts = (0, 0);
    for part in ["home", "etc"] {
        let from = format!("{}/{}", source, part);
        if fs::stat(&from).is_ok() {
            copy_tree(&from, &format!("/{}", part), &mut counts);
        }
    }
    println!("restore: {} dosya ({} hata)", counts.0, counts.1);
}

/// Hesapları, ev klasörlerini ve masaüstü ayarlarını siler; sistem dosyaları kalır.
pub fn cmd_factory_reset(_: &str) {
    if !recovery_only("factory-reset") || !root_only("factory-reset") {
        return;
    }
    if !confirm(
        "TÜM kullanıcı hesapları, ev klasörleri ve ayarlar silinecek.",
        "RESET",
    ) {
        println!("factory-reset: iptal edildi");
        return;
    }
    let mut failed = 0;
    for entry in fs::read_dir("/home").unwrap_or_default() {
        if entry.name != "." && entry.name != ".." {
            failed += remove_tree(&format!("/home/{}", entry.name));
        }
    }
    for path in [users::USERS_FILE, crate::gui::prefs::PATH] {
        if fs::stat(path).is_ok() && fs::remove(path).is_err() {
            failed += 1;
        }
    }
    println!(
        "factory-reset: tamam ({} silinemeyen öğe). Yeniden başlatınca ilk kurulum açılır.",
        failed
    );
}

/// Masaüstü ayarlarını varsayılana döndürür (masaüstü açılmıyorsa).
pub fn cmd_reset_desktop(_: &str) {
    if !root_only("reset-desktop") {
        return;
    }
    match fs::remove(crate::gui::prefs::PATH) {
        Ok(()) => println!("reset-desktop: masaüstü ayarları varsayılana döndü"),
        Err(_) => println!("reset-desktop: ayar dosyası yok ya da silinemedi"),
    }
}
