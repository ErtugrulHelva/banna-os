//! `banna-update [check | install | apply | status | version]`: Banna OS sistem güncellemesi.
//! Günlük kullanım `hatch update` / `hatch upgrade` üzerindendir; ayrıntılar
//! `banna::sysupdate` içinde. `apply` açılışta çekirdek tarafından çalıştırılır.

#![no_std]
#![no_main]

use banna::sysupdate::{self, EXIT_REBOOT};
use banna::{String, eprintln, println, process};

banna::entry!(main);

type Result<T> = core::result::Result<T, String>;

fn cmd_check() -> Result<i32> {
    let repo = sysupdate::repo_base()?;
    let m = sysupdate::refresh(&repo)?;
    let current = sysupdate::current_version();
    let list = sysupdate::changed(&m);
    println!("Kurulu sürüm: {}", current);
    println!("Yayındaki sürüm: {}", m.version);
    if !m.notes.is_empty() {
        println!("Notlar: {}", m.notes);
    }
    if list.is_empty() {
        println!("Sistem güncel.");
        return Ok(0);
    }
    println!("{} dosya güncellenecek:", list.len());
    for e in &list {
        println!("  {} ({} KiB)", e.path, e.size.div_ceil(1024));
    }
    println!("Kurmak için: sudo hatch upgrade");
    Ok(10)
}

fn require_root(command: &str) -> Result<()> {
    if process::uid() == 0 {
        Ok(())
    } else {
        Err(banna::format!(
            "yönetici gerekir: sudo banna-update {}",
            command
        ))
    }
}

/// Hemen kurar (hazırla + uygula).
fn cmd_install() -> Result<i32> {
    require_root("install")?;
    let repo = sysupdate::repo_base()?;
    let m = sysupdate::refresh(&repo)?;
    if sysupdate::stage(&repo, &m)? == 0 {
        sysupdate::mark_current(&m);
        println!("Sistem güncel ({}).", m.version);
        return Ok(0);
    }
    if sysupdate::apply()? {
        println!("Yeni çekirdek için bilgisayarı yeniden başlatın. Sorun olursa önyükleme");
        println!("menüsünden \"Önceki sürüm\" seçilebilir.");
    }
    Ok(0)
}

fn cmd_apply() -> Result<i32> {
    require_root("apply")?;
    let version = sysupdate::pending().unwrap_or_default();
    println!("banna-update: applying {}", version);
    let boot = sysupdate::apply()?;
    println!("banna-update: applied {}", version);
    Ok(if boot { EXIT_REBOOT } else { 0 })
}

fn cmd_status() -> Result<i32> {
    println!("Kurulu sürüm: {}", sysupdate::current_version());
    match sysupdate::pending() {
        Some(v) => println!("Yeniden başlatınca kurulacak: {}", v),
        None => println!("Bekleyen güncelleme yok."),
    }
    if let Some(e) = sysupdate::last_error() {
        println!("Son uygulama hatası: {}", e);
    }
    Ok(0)
}

fn main(args: &[&str]) -> i32 {
    let result = match args.get(1).copied() {
        None | Some("check") => cmd_check(),
        Some("install") => cmd_install(),
        Some("apply") => cmd_apply(),
        Some("status") => cmd_status(),
        Some("version") => {
            println!("{}", sysupdate::current_version());
            Ok(0)
        }
        _ => {
            eprintln!("usage: banna-update [check | install | apply | status | version]");
            Ok(1)
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("banna-update: {}", e);
            1
        }
    }
}
