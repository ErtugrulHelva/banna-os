//! Uygulamaların masaüstüyle konuştuğu arayüz (Ayarlar, Sistem İzleyici, Hakkında):
//! `query` tüm ayarları ve canlı sistem bilgisini `anahtar=değer` satırları olarak verir,
//! `set` tek bir ayarı değiştirir ya da bir eylemi (ses denemesi, DHCP yenileme...) başlatır.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::prefs::{Prefs, Wallpaper};
use crate::drivers::{audio, battery, cpupower, keyboard, mouse, rtc};
use crate::sync::SpinLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pref {
    Dark,
    Accent,
    PanelOpacity,
    Animations,
    Shadows,
    WallpaperDim,
    NightLight,
    NightStrength,
    ShowFps,
    Vsync,
    Parallel,
    ScreenOff,
    Brightness,
    Resolution,
    Volume,
    Muted,
    UiSounds,
    Profile,
    LidAction,
    PowerButton,
    Layout,
    MouseSpeed,
    LeftHanded,
    Clock24,
    ClockSeconds,
    ShowDate,
    DockSize,
    DockMagnify,
    DockRunning,
    StatusItems,
}

const KEYS: [(&str, Pref); 30] = [
    ("dark", Pref::Dark),
    ("accent", Pref::Accent),
    ("panel_opacity", Pref::PanelOpacity),
    ("animations", Pref::Animations),
    ("shadows", Pref::Shadows),
    ("wallpaper_dim", Pref::WallpaperDim),
    ("night_light", Pref::NightLight),
    ("night_strength", Pref::NightStrength),
    ("show_fps", Pref::ShowFps),
    ("vsync", Pref::Vsync),
    ("parallel", Pref::Parallel),
    ("screen_off", Pref::ScreenOff),
    ("brightness", Pref::Brightness),
    ("resolution", Pref::Resolution),
    ("volume", Pref::Volume),
    ("muted", Pref::Muted),
    ("ui_sounds", Pref::UiSounds),
    ("profile", Pref::Profile),
    ("lid_action", Pref::LidAction),
    ("power_button", Pref::PowerButton),
    ("layout", Pref::Layout),
    ("mouse_speed", Pref::MouseSpeed),
    ("left_handed", Pref::LeftHanded),
    ("clock_24h", Pref::Clock24),
    ("clock_seconds", Pref::ClockSeconds),
    ("show_date", Pref::ShowDate),
    ("dock_size", Pref::DockSize),
    ("dock_magnify", Pref::DockMagnify),
    ("dock_running", Pref::DockRunning),
    ("status_items", Pref::StatusItems),
];

impl Pref {
    fn from_key(key: &str) -> Option<Self> {
        KEYS.iter().find(|(k, _)| *k == key).map(|(_, p)| *p)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    TestSound,
    Benchmark,
    DhcpRenew,
    Logout,
    Reboot,
    Shutdown,
    TzMinus,
    TzPlus,
    Reset,
}

pub enum Change {
    Set(Pref, i32),
    Wallpaper(Wallpaper),
    Action(Action),
    /// Kaydırıcı bırakıldı: pahalı işler (duvar kağıdı, parlaklık) şimdi uygulanır.
    Release(Pref),
}

/// `anahtar`, `değer` çiftini değişikliğe çevirir.
pub fn parse(key: &str, value: &str) -> Option<Change> {
    match key {
        "wallpaper" => {
            let choice = match value.split_once(':')? {
                ("preset", i) => Wallpaper::Preset(i.parse().ok()?),
                ("solid", i) => Wallpaper::Solid(i.parse().ok()?),
                ("image", path) if path.starts_with('/') => Wallpaper::Image(path.into()),
                _ => return None,
            };
            Some(Change::Wallpaper(choice))
        }
        "action" => Some(Change::Action(match value {
            "test_sound" => Action::TestSound,
            "benchmark" => Action::Benchmark,
            "dhcp_renew" => Action::DhcpRenew,
            "logout" => Action::Logout,
            "reboot" => Action::Reboot,
            "shutdown" => Action::Shutdown,
            "tz_minus" => Action::TzMinus,
            "tz_plus" => Action::TzPlus,
            "reset" => Action::Reset,
            _ => return None,
        })),
        "release" => Pref::from_key(value).map(Change::Release),
        "resolution" => {
            let (w, h) = value.split_once('x')?;
            let (w, h): (i32, i32) = (w.parse().ok()?, h.parse().ok()?);
            Some(Change::Set(Pref::Resolution, (w << 16) | h))
        }
        "layout" => Some(Change::Set(
            Pref::Layout,
            (value == "us" || value == "1") as i32,
        )),
        _ => Some(Change::Set(Pref::from_key(key)?, value.parse().ok()?)),
    }
}

/// Arka plan işlerinin (performans testi, DHCP, çözünürlük) sonuç metni.
static NOTICE: SpinLock<Option<String>> = SpinLock::new(None);

pub fn set_notice(text: String) {
    *NOTICE.lock() = Some(text);
}

/// Ayarlar ve canlı bilgi; `prefs` masaüstünün o anki tercihleridir.
pub fn query(prefs: &Prefs) -> String {
    let mut out = prefs.serialize();
    let mut put = |k: &str, v: String| {
        out.push_str(k);
        out.push('=');
        out.push_str(&v.replace('\n', " "));
        out.push('\n');
    };

    // Sistem
    let info = crate::arch::cpuinfo::read();
    let mem = crate::memory::pmm::stats();
    put("sys.version", String::from(env!("CARGO_PKG_VERSION")));
    put(
        "sys.cpu",
        if info.brand.is_empty() {
            info.vendor.clone()
        } else {
            info.brand.clone()
        },
    );
    put("sys.cores", format!("{}", crate::arch::smp::online_count()));
    put("sys.mem_total", format!("{}", mem.total_bytes));
    put("sys.mem_free", format!("{}", mem.free_bytes));
    put("sys.hypervisor", info.hypervisor.unwrap_or_default());
    put(
        "sys.uptime_ms",
        format!("{}", crate::arch::pit::uptime_ms()),
    );

    // Ekran
    let (w, h) = crate::gfx::display::size().unwrap_or((0, 0));
    put("display.size", format!("{}x{}", w, h));
    let modes: Vec<String> = crate::drivers::gpu::modes()
        .into_iter()
        .map(|(mw, mh)| format!("{}x{}", mw, mh))
        .collect();
    put("display.modes", modes.join(","));
    if let Some((nw, nh)) = crate::drivers::gpu::native_mode() {
        put("display.native", format!("{}x{}", nw, nh));
    }
    put(
        "display.restart",
        String::from(
            if crate::drivers::gpu::adapter() == crate::drivers::gpu::Adapter::Firmware {
                "1"
            } else {
                "0"
            },
        ),
    );
    put("display.adapter", crate::drivers::gpu::describe());
    {
        use crate::drivers::gpu::vsync;
        put(
            "display.vsync",
            if vsync::available() {
                format!("{}, {} Hz", vsync::describe(), vsync::refresh_hz())
            } else {
                String::new()
            },
        );
    }
    put(
        "display.workers",
        format!("{}", crate::gfx::parallel::workers()),
    );
    put(
        "display.brightness",
        crate::drivers::power::brightness()
            .map(|b| format!("{}", b))
            .unwrap_or_default(),
    );
    let stats = super::stats();
    put("display.fps", format!("{}", stats.fps));
    put("display.frame_us", format!("{}", stats.frame_time_us));

    // Ses, güç, giriş, saat
    put("audio.present", format!("{}", audio::present() as u8));
    put("audio.volume", format!("{}", audio::volume()));
    put("power.hwp", format!("{}", cpupower::hwp_available() as u8));
    put(
        "power.profile",
        format!(
            "{}",
            match cpupower::profile() {
                cpupower::Profile::Performance => 0,
                cpupower::Profile::Balanced => 1,
                cpupower::Profile::PowerSave => 2,
            }
        ),
    );
    put(
        "power.temperature",
        cpupower::temperature()
            .map(|t| format!("{}", t))
            .unwrap_or_default(),
    );
    match battery::batteries().into_iter().find(|b| b.present) {
        Some(b) => {
            put("battery.percent", format!("{}", b.percent().unwrap_or(0)));
            put("battery.state", String::from(b.state_name()));
            put(
                "battery.minutes",
                b.minutes_left()
                    .map(|m| format!("{}", m))
                    .unwrap_or_default(),
            );
        }
        None => put("battery.percent", String::new()),
    }
    put(
        "input.layout",
        String::from(if keyboard::layout() == keyboard::Layout::Us {
            "us"
        } else {
            "tr"
        }),
    );
    put("input.mouse_speed", format!("{}", mouse::speed()));
    put(
        "input.left_handed",
        format!("{}", mouse::left_handed() as u8),
    );
    put("clock.offset", format!("{}", rtc::local_offset_minutes()));
    put("clock.zone", rtc::local_zone());
    put("clock.synced", format!("{}", rtc::synchronized() as u8));
    put("clock.now", super::draw::clock_text(prefs));

    // İşlemci yükü ve görevler (Sistem İzleyici)
    let loads: Vec<String> = crate::sched::cpu_loads()
        .iter()
        .map(|l| format!("{}", l))
        .collect();
    put("sched.loads", loads.join(","));
    put(
        "sched.threads",
        format!("{}", crate::sched::threads().len()),
    );
    put(
        "sched.processes",
        format!("{}", crate::sched::process::live_count()),
    );

    // Ağ
    put("net.status", crate::net::status_text());
    if let Some(r) = crate::net::report() {
        let m = r.mac;
        put("net.model", String::from(r.model));
        put(
            "net.mac",
            format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                m[0], m[1], m[2], m[3], m[4], m[5]
            ),
        );
        put("net.link", format!("{}", r.link_up as u8));
        put("net.speed", format!("{}", r.speed));
        put("net.duplex", format!("{}", r.full_duplex as u8));
        put("net.dhcp", String::from(r.dhcp_state));
        if let Some(cfg) = &r.config {
            put("net.address", format!("{}", cfg.address));
            put("net.netmask", format!("{}", cfg.netmask));
            put("net.gateway", format!("{}", cfg.gateway));
            let dns: Vec<String> = cfg.dns.iter().map(|d| format!("{}", d)).collect();
            put("net.dns", dns.join(", "));
        }
    }

    // Kullanıcılar
    if let Some(s) = crate::kernel::users::session() {
        put("user.name", s.name);
        put("user.uid", format!("{}", s.uid));
        put("user.admin", format!("{}", s.admin as u8));
        put("user.home", s.home);
    }
    let accounts: Vec<String> = crate::kernel::users::list()
        .into_iter()
        .map(|(name, uid, admin)| format!("{}:{}:{}", name, uid, admin as u8))
        .collect();
    put("users", accounts.join(";"));

    if let Some(text) = NOTICE.lock().clone() {
        put("notice", text);
    }
    out
}
