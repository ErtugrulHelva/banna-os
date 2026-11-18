//! Banna Hata Raporlama Sistemi: takılma (bekçi), çekirdek paniği ve istisnalarda tam
//! ekran rapor paneli çizer ve raporu ESP'ye (`/boot/boot/crash/`) kaydeder. Kullanıcı
//! Banna hesabına giriş yapmışsa bir sonraki oturumda `banna-crash` raporu Banna
//! Ekosistemi'ne yükler; ana onay cihazına bildirim gider.
//!
//! Hata anında kilitler takılı olabilir: çizim, konsolun framebuffer'ına doğrudan yapılır;
//! kayıt ayrı bir thread'de denenir (disk çalışmıyorsa rapor yalnızca ekranda kalır).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{cpu, tsc};
use crate::gfx::text;
use crate::gfx::{Rect, Surface, rgb, with_alpha};

pub const DIR: &str = "/boot/boot/crash";

static SHOWN: AtomicBool = AtomicBool::new(false);

const BG: u32 = rgb(0x0f, 0x12, 0x19);
const CARD: u32 = rgb(0x18, 0x1d, 0x28);
const ACCENT: u32 = rgb(0xe5, 0x48, 0x4d);
const TEXT: u32 = rgb(0xe8, 0xea, 0xef);
const MUTED: u32 = rgb(0x9a, 0xa3, 0xb2);

pub struct Report {
    /// watchdog | panic | exception
    pub kind: &'static str,
    /// Kullanıcıya gösterilen kısa açıklama.
    pub title: String,
    pub details: Vec<String>,
}

fn stamp() -> String {
    crate::drivers::rtc::now()
        .map(|t| {
            format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                t.year, t.month, t.day, t.hour, t.minute, t.second
            )
        })
        .unwrap_or_else(|| format!("boot-{}", tsc::now_ns() / 1_000_000))
}

/// Raporun dosya metni: başlık alanları, ayrıntılar ve açılıştan beri tüm günlük.
fn render_text(report: &Report, when: &str) -> String {
    let cpu = crate::arch::cpuinfo::read();
    let mut text = String::from("BANNA-CRASH 1\n");
    text.push_str(&format!("kind: {}\n", report.kind));
    text.push_str(&format!("title: {}\n", report.title));
    text.push_str(&format!("version: {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!("time: {}\n", when));
    text.push_str(&format!("uptime_ms: {}\n", tsc::now_ns() / 1_000_000));
    text.push_str(&format!("cpu: {}\n", cpu.brand.trim()));
    text.push_str(&format!("cmdline: {}\n", crate::boot::cmdline()));
    text.push_str("--- details ---\n");
    for line in &report.details {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("--- log ---\n");
    text.push_str(&String::from_utf8_lossy(&crate::log::boot_log()));
    text
}

fn draw(report: &Report, saved_as: &str) {
    // Konsol kilidi takılı çekirdekte kalmış olabilir: zorla devralınır.
    crate::drivers::console::emergency_takeover();
    let Some(mut fb) = crate::drivers::console::framebuffer() else {
        return;
    };
    let (w, h) = (fb.width as i32, fb.height as i32);
    let mut surface = Surface::new(w, h, BG);
    let f = text::ui_fonts(h);
    let u = if h >= 1300 { 2 } else { 1 };
    {
        let mut c = surface.canvas();
        c.fill(Rect::new(0, 0, w, 6 * u), ACCENT);
        let x = 48 * u;
        let mut y = 40 * u;
        f.heading
            .draw(&mut c, x, y, "BANNA HATA RAPORLAMA SİSTEMİ", TEXT);
        y += f.heading.line_height() + 6 * u;
        f.title.draw(&mut c, x, y, &report.title, ACCENT);
        y += f.title.line_height() + 4 * u;
        let info = format!(
            "Banna OS {}  |  çalışma süresi {} sn  |  {}",
            env!("CARGO_PKG_VERSION"),
            tsc::now_ns() / 1_000_000_000,
            report.kind
        );
        f.small.draw(&mut c, x, y, &info, MUTED);
        y += f.small.line_height() + 18 * u;

        // Ayrıntılar kartı.
        let card_w = w - 2 * x;
        let detail_h = (report.details.len() as i32 * f.small.line_height() + 46 * u).min(h / 3);
        let card = Rect::new(x, y, card_w, detail_h);
        c.fill_rounded(card, 12 * u, CARD);
        f.title
            .draw(&mut c, x + 18 * u, y + 12 * u, "Ayrıntılar", TEXT);
        let mut ly = y + 14 * u + f.title.line_height();
        for line in &report.details {
            if ly + f.small.line_height() > card.bottom() - 8 * u {
                break;
            }
            f.small
                .draw_clipped(&mut c, x + 18 * u, ly, line, card_w - 36 * u, TEXT);
            ly += f.small.line_height();
        }
        y = card.bottom() + 16 * u;

        // Günlük kartı: ekrana sığan son satırlar.
        let footer_h = 3 * f.small.line_height() + 30 * u;
        let log_card = Rect::new(x, y, card_w, h - y - footer_h - 16 * u);
        c.fill_rounded(log_card, 12 * u, CARD);
        f.title
            .draw(&mut c, x + 18 * u, y + 12 * u, "Ayrıntılı günlük", TEXT);
        let top = y + 14 * u + f.title.line_height();
        let rows = ((log_card.bottom() - 10 * u - top) / f.small.line_height()).max(0) as usize;
        let log = crate::log::boot_log();
        let log = String::from_utf8_lossy(&log);
        let lines: Vec<&str> = log.lines().collect();
        let mut ly = top;
        for line in &lines[lines.len().saturating_sub(rows)..] {
            f.small
                .draw_clipped(&mut c, x + 18 * u, ly, line, card_w - 36 * u, MUTED);
            ly += f.small.line_height();
        }

        // Alt bilgi.
        let fy = h - footer_h;
        c.fill(
            Rect::new(0, fy - 8 * u, w, u.max(1)),
            with_alpha(0xffffff, 30),
        );
        let status = if saved_as.is_empty() {
            String::from("Rapor diske kaydedilemedi (disk yanıt vermiyor); yalnızca bu ekranda.")
        } else {
            format!("Rapor kaydedildi: {}", saved_as)
        };
        f.small.draw(&mut c, x, fy, &status, TEXT);
        f.small.draw(
            &mut c,
            x,
            fy + f.small.line_height(),
            "Banna hesabınızla giriş yaptıysanız rapor bir sonraki oturumda ana onay cihazınıza gönderilir.",
            MUTED,
        );
        f.small.draw(
            &mut c,
            x,
            fy + 2 * f.small.line_height(),
            "Yeniden başlatmak için R tuşuna ya da güç düğmesine basın.",
            MUTED,
        );
    }
    for row in 0..h {
        let start = (row * w) as usize;
        fb.write_row(0, row as usize, &surface.pixels[start..start + w as usize]);
    }
}

/// Raporu ESP'ye yazar (ayrı thread; disk takılıysa bekçiyi bekletmez). Yol ya da boş.
fn save(text: String, name: String) -> String {
    let path = format!("{}/{}.log", DIR, name);
    let done = alloc::sync::Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let target = path.clone();
    crate::sched::spawn_detached("crash-save", move || {
        let _ = crate::fs::mkdir(DIR);
        if crate::fs::write_all(&target, text.as_bytes()).is_ok() {
            flag.store(true, Ordering::Release);
        }
        0
    });
    let deadline = tsc::now_ns() + 5_000_000_000;
    while !done.load(Ordering::Acquire) && tsc::now_ns() < deadline {
        crate::sched::wake_sleepers();
        let next = tsc::now_ns() + 500_000;
        while tsc::now_ns() < next {
            core::hint::spin_loop();
        }
    }
    if done.load(Ordering::Acquire) {
        path
    } else {
        String::new()
    }
}

/// PS/2 denetleyicisinden R tuşunu (set 1 tarama kodu 0x13) yoklar; kesmeler kapalı da çalışır.
fn wait_for_reboot() -> ! {
    loop {
        let status = unsafe { cpu::inb(0x64) };
        if status != 0xff && status & 1 != 0 {
            let code = unsafe { cpu::inb(0x60) };
            if code == 0x13 {
                crate::drivers::power::reboot();
            }
        }
        core::hint::spin_loop();
    }
}

/// Raporu gösterir ve kaydeder. `wait` ise yeniden başlatma tuşu beklenir (dönmez).
pub fn raise(report: Report, wait: bool) {
    if SHOWN.swap(true, Ordering::AcqRel) {
        if wait {
            wait_for_reboot();
        }
        return;
    }
    for line in &report.details {
        crate::log!("crash: {}", line);
    }
    crate::log!("crash: {} ({})", report.title, report.kind);
    let when = stamp();
    // Önce panel (disk takılıysa da görünsün), sonra kayıt ve kayıt durumu.
    draw(&report, "");
    let text = render_text(&report, &when);
    let saved = save(text, format!("crash-{}", when));
    draw(&report, &saved);
    if wait {
        wait_for_reboot();
    }
}
