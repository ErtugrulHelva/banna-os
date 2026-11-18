//! Masaüstü sahnesinin çizimi (Inter ile kenar yumuşatmalı yazı).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::prefs::{Prefs, Theme};
use super::{Desktop, Window, apps, icons, m};
use crate::arch::smp;
use crate::drivers::{battery, rtc};
use crate::gfx::text::{self, Font, UiFonts};
use crate::gfx::{Argb, Canvas, Glass, Rect, Surface, glass, rgb, with_alpha};
use crate::sync::SpinLock;

pub const WINDOW_BG: Argb = rgb(0x16, 0x1b, 0x26);
const CLOSE: Argb = rgb(0xff, 0x5f, 0x57);
const MINIMIZE: Argb = rgb(0xfe, 0xbc, 0x2e);
const MAXIMIZE: Argb = rgb(0x28, 0xc8, 0x40);
const WHITE: Argb = rgb(0xff, 0xff, 0xff);

/// Önbelleğe alınmış cam katmanı: (konum, yarıçap, görünüm, arka plan kuşağı) aynıysa
/// yeniden hesaplanmadan bindirilir.
type GlassSlot = SpinLock<Option<((Rect, i32, Glass, u64), Surface)>>;

static PANEL_GLASS: GlassSlot = SpinLock::new(None);
static DOCK_GLASS: GlassSlot = SpinLock::new(None);

fn cached_glass(
    canvas: &mut Canvas,
    slot: &GlassSlot,
    rect: Rect,
    radius: i32,
    desktop: &Desktop,
    style: &Glass,
) {
    if rect.intersect(&canvas.clip).is_empty() {
        return;
    }
    let key = (rect, radius, *style, desktop.backdrop_gen);
    let mut cached = slot.lock();
    if !matches!(&*cached, Some((k, _)) if *k == key) {
        *cached = Some((key, glass::layer(rect, radius, &desktop.backdrop, style)));
    }
    if let Some((_, layer)) = &*cached {
        canvas.blend_image(&layer.pixels, layer.width, layer.height, rect.x, rect.y);
    }
}

/// Panel çubuğu: ince, kırılmasız cam.
fn bar_glass(t: &Theme) -> Glass {
    Glass {
        tint: t.panel,
        rim: 0,
        refraction: 0,
        edge: 1,
        sheen: if t.dark { 10 } else { 40 },
    }
}

/// Dock, ipuçları ve kayan yüzeyler: kenarı kırılan, parlak kenarlı cam.
fn float_glass(t: &Theme) -> Glass {
    let u = m().unit;
    Glass {
        tint: t.dock,
        rim: if t.dark { 150 } else { 210 },
        refraction: 10 * u,
        edge: 9 * u,
        sheen: if t.dark { 22 } else { 50 },
    }
}

/// Başlatıcı ve Alt+Tab gibi içerik taşıyan büyük yüzeyler: okunurluk için daha koyu ton.
pub fn sheet_glass(t: &Theme) -> Glass {
    let u = m().unit;
    Glass {
        tint: if t.dark {
            with_alpha(rgb(0x14, 0x14, 0x17), 150)
        } else {
            with_alpha(rgb(0xfa, 0xfa, 0xfc), 170)
        },
        rim: if t.dark { 120 } else { 200 },
        refraction: 16 * u,
        edge: 18 * u,
        sheen: if t.dark { 14 } else { 40 },
    }
}

/// Pencere başlığı: arkadaki duvar kağıdından renk alan cam.
fn title_glass(t: &Theme, focused: bool) -> Glass {
    let base = if focused { t.title_active } else { t.title_bg };
    Glass {
        tint: with_alpha(base, if focused { 165 } else { 205 }),
        rim: if t.dark { 90 } else { 160 },
        refraction: 0,
        edge: 1,
        sheen: if t.dark { 8 } else { 20 },
    }
}

pub fn fonts(height: i32) -> UiFonts {
    text::ui_fonts(height)
}

const DAYS: [&str; 7] = ["Paz", "Pzt", "Sal", "Çar", "Per", "Cum", "Cmt"];
const MONTHS: [&str; 12] = [
    "Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara",
];

pub fn clock_text(p: &Prefs) -> String {
    let Some(now) = rtc::now() else {
        return String::from("--:--");
    };
    let l = now.to_local();
    let (hour, suffix) = if p.clock_24h {
        (l.hour, "")
    } else {
        let h = l.hour % 12;
        (
            if h == 0 { 12 } else { h },
            if l.hour < 12 { " ÖÖ" } else { " ÖS" },
        )
    };
    let time = if p.clock_seconds {
        format!("{:02}:{:02}:{:02}{}", hour, l.minute, l.second, suffix)
    } else {
        format!("{:02}:{:02}{}", hour, l.minute, suffix)
    };
    if !p.show_date {
        return time;
    }
    let weekday = (l.unix_timestamp().div_euclid(86_400) + 4).rem_euclid(7) as usize;
    format!(
        "{} {} {}   {}",
        DAYS[weekday],
        l.day,
        MONTHS[(l.month as usize).clamp(1, 12) - 1],
        time
    )
}

pub fn status_text(p: &Prefs) -> String {
    let mut parts: Vec<String> = Vec::new();
    if p.status_items {
        parts.push(crate::net::status_text());
        parts.push(match battery::batteries().into_iter().find(|b| b.present) {
            Some(b) => format!(
                "Pil %{}{}",
                b.percent().unwrap_or(0),
                if b.charging { " (şarj)" } else { "" }
            ),
            None => String::from("AC"),
        });
        parts.push(format!("{} çekirdek", smp::online_count()));
    }
    if p.muted {
        parts.push(String::from("Sessiz"));
    }
    let fps = super::stats().fps;
    if p.show_fps && fps > 0 {
        parts.push(format!("{} fps", fps));
    }
    parts.join("   •   ")
}

/// Gece ışığı: maviyi ve biraz yeşili azaltarak sıcak tona çeker.
fn night_light(canvas: &mut Canvas, strength: u8) {
    let clip = canvas.clip;
    let k = strength as u32 * 256 / 100;
    let keep_b = 256 - k * 3 / 4;
    let keep_g = 256 - k / 3;
    for y in clip.y..clip.bottom() {
        for p in canvas.span(y, clip.x, clip.right()) {
            let c = *p;
            let g = (((c >> 8) & 0xff) * keep_g) >> 8;
            let b = ((c & 0xff) * keep_b) >> 8;
            *p = (c & 0xff_0000) | g << 8 | b;
        }
    }
}

pub fn scene(desktop: &Desktop, canvas: &mut Canvas) {
    let clip = canvas.clip;
    if desktop.blanked {
        canvas.fill(clip, rgb(0, 0, 0));
        return;
    }
    if let Some(login) = &desktop.login {
        login.draw(canvas, &desktop.wallpaper, desktop.width, desktop.height);
    } else {
        let (needs_wallpaper, first) = desktop.visible_from(&clip);
        let layers: Vec<&Window> = desktop.windows[first..]
            .iter()
            .filter(|w| w.visible && !w.footprint().intersect(&clip).is_empty())
            .collect();
        let opaque: Vec<Rect> = layers.iter().map(|w| w.opaque()).collect();
        // Her katman yalnızca üstündeki opak pencerelerin örtmediği parçalarda çizilir.
        let focused = desktop.focused().map(|w| w.id);
        let f = fonts(desktop.height);
        let t = &desktop.theme;
        if needs_wallpaper {
            let icons = super::desk::bounds(&desktop.desk);
            for piece in uncovered(clip, &opaque) {
                canvas.blit(&desktop.wallpaper, piece, piece.x, piece.y);
                if !piece.intersect(&icons).is_empty() {
                    canvas.clip = piece;
                    super::desk::draw(canvas, &desktop.desk, desktop.desk_selected, t, &f);
                    canvas.clip = clip;
                }
            }
        }
        // Yaslama önizlemesi sürüklenen (en üstteki) pencerenin altında çizilir.
        let preview = desktop
            .snap_preview
            .map(|(_, r)| r)
            .filter(|r| !r.intersect(&clip).is_empty());
        // Sürüklenen pencere en üsttedir; bu bantta yoksa önizleme her şeyin üstüne çizilir.
        let dragged = desktop
            .windows
            .iter()
            .rev()
            .find(|w| w.visible)
            .map(|w| w.id);
        for (i, window) in layers.iter().enumerate() {
            for piece in uncovered(clip, &opaque[i + 1..]) {
                canvas.clip = piece;
                if Some(window.id) == dragged
                    && let Some(r) = preview
                {
                    draw_snap_preview(canvas, r, t);
                }
                draw_window(
                    window,
                    focused == Some(window.id),
                    canvas,
                    f.title,
                    t,
                    &desktop.backdrop,
                );
            }
            canvas.clip = clip;
        }
        if let Some(r) = preview
            && !layers.iter().any(|w| Some(w.id) == dragged)
        {
            draw_snap_preview(canvas, r, t);
        }
        if !Rect::new(0, 0, desktop.width, m().panel_h)
            .intersect(&clip)
            .is_empty()
        {
            draw_panel(desktop, canvas, &f);
        }
        if !desktop.dock_area().intersect(&clip).is_empty() {
            draw_dock(desktop, canvas, &f);
        }
        if desktop.switcher.is_some() {
            draw_switcher(desktop, canvas, &f);
        }
        if let Some(l) = &desktop.launcher
            && !l.footprint().intersect(&clip).is_empty()
            && let Some(progress) = l.progress(desktop.now_ms)
        {
            l.draw(canvas, progress);
        }
        if let Some(menu) = &desktop.menu
            && !menu.footprint().intersect(&clip).is_empty()
        {
            menu.draw(canvas, t, &f, &desktop.backdrop);
        }
    }
    if desktop.prefs.night_light {
        night_light(canvas, desktop.prefs.night_strength);
    }
}

/// `clip`'in `holes` tarafından örtülmeyen parçaları. Çok parçalanırsa (çizim
/// tekrarının maliyeti kazancı aşacağından) `clip` olduğu gibi döner.
fn uncovered(clip: Rect, holes: &[Rect]) -> Vec<Rect> {
    let mut pieces = alloc::vec![clip];
    for hole in holes {
        if hole.intersect(&clip).is_empty() {
            continue;
        }
        let mut next = Vec::with_capacity(pieces.len() + 3);
        for p in pieces {
            let h = hole.intersect(&p);
            if h.is_empty() {
                next.push(p);
                continue;
            }
            let top = Rect::new(p.x, p.y, p.w, h.y - p.y);
            let bottom = Rect::new(p.x, h.bottom(), p.w, p.bottom() - h.bottom());
            let left = Rect::new(p.x, h.y, h.x - p.x, h.h);
            let right = Rect::new(h.right(), h.y, p.right() - h.right(), h.h);
            next.extend(
                [top, bottom, left, right]
                    .into_iter()
                    .filter(|r| !r.is_empty()),
            );
        }
        pieces = next;
        if pieces.len() > 12 {
            return alloc::vec![clip];
        }
    }
    pieces
}

fn contains(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn draw_window(
    window: &Window,
    focused: bool,
    canvas: &mut Canvas,
    title_font: &Font,
    t: &Theme,
    backdrop: &crate::gfx::Surface,
) {
    let mt = m();
    let frame = window.frame;
    let client = window.client();
    // Uygulama yüzeyinin örttüğü bölge zemin rengiyle boyanmaz (hemen üstüne kopyalanır).
    let covered = Rect::new(
        client.x,
        client.y,
        window.surface.width,
        window.surface.height,
    )
    .intersect(&client);
    // Alt köşe satırları eğriyle kırpıldığından tam örtülmüş sayılmaz.
    let covered = Rect::new(
        covered.x,
        covered.y,
        covered.w,
        (covered.h - mt.radius).max(0),
    );
    if !contains(&window.opaque(), &canvas.clip) && t.shadows {
        canvas.shadow(frame, mt.radius, mt.shadow, if focused { 120 } else { 70 });
    }
    // Başlık camının iç kısmı da (kenar yumuşatması ve köşeler hariç) tamamen örter.
    let r = mt.radius;
    let glass_core = [
        Rect::new(frame.x + 1, frame.y + r, frame.w - 2, mt.title_h - r),
        Rect::new(frame.x + r, frame.y + 1, frame.w - 2 * r, r),
    ];
    let clip = canvas.clip;
    for piece in uncovered(clip, &[covered, glass_core[0], glass_core[1]]) {
        canvas.clip = piece;
        canvas.fill_rounded(frame, r, t.window_bg);
    }
    canvas.clip = clip;
    let title = window.title_bar();
    if !title.intersect(&canvas.clip).is_empty() {
        canvas.glass(
            Rect::new(frame.x, frame.y, frame.w, mt.title_h + mt.radius),
            mt.radius,
            backdrop,
            (0, 0),
            &title_glass(t, focused),
        );
        canvas.fill(
            Rect::new(frame.x, frame.y + mt.title_h - 1, frame.w, 1),
            with_alpha(if t.dark { 0xffffff } else { 0 }, 18),
        );
        let close = window.close_button();
        let minimize = window.minimize_button();
        let r = close.w / 2;
        icons::disc(
            canvas,
            close.x + r,
            close.y + r,
            r,
            if focused { CLOSE } else { t.muted },
        );
        icons::disc(
            canvas,
            minimize.x + r,
            minimize.y + r,
            r,
            if focused { MINIMIZE } else { t.muted },
        );
        let maximize = window.maximize_button();
        icons::disc(
            canvas,
            maximize.x + r,
            maximize.y + r,
            r,
            if focused { MAXIMIZE } else { t.muted },
        );
        let max = frame.w - 2 * (maximize.right() - frame.x) - 8;
        let width = title_font.measure(&window.title).min(max);
        let ty = frame.y + (mt.title_h - title_font.ascent - title_font.descent) / 2;
        title_font.draw_clipped(
            canvas,
            frame.x + frame.w / 2 - width / 2,
            ty,
            &window.title,
            max,
            if focused { t.text } else { t.muted },
        );
    }
    if !client.intersect(&canvas.clip).is_empty() {
        // İçerik pencerenin altına kadar iner: alt köşeler çerçeve gibi yuvarlatılır.
        let bottom_flush = client.bottom() >= frame.bottom();
        let radius = if bottom_flush { r } else { 0 };
        canvas.blit_round_bottom(&window.surface, client.x, client.y, radius);
    }
    // Boyutlandırma tutamağı: köşede üç kısa çizgi.
    if focused {
        let h = window.resize_handle();
        let u = mt.unit;
        for i in 1..=3 {
            let off = i * 4 * u;
            canvas.fill(
                Rect::new(h.right() - off - 2 * u, h.bottom() - 4 * u, off, u.max(1)),
                with_alpha(if t.dark { 0xffffff } else { 0 }, 40),
            );
        }
    }
}

/// Alt+Tab pencere değiştirici: ortada simgeler, seçili olan vurgulu.
fn draw_switcher(desktop: &Desktop, canvas: &mut Canvas, f: &UiFonts) {
    let Some((order, selected)) = &desktop.switcher else {
        return;
    };
    let t = &desktop.theme;
    let u = m().unit;
    let tile = 120 * u;
    let count = order.len() as i32;
    let w = (count * tile + 32 * u).min(desktop.width - 40 * u);
    let h = tile + 56 * u;
    let r = Rect::new(desktop.width / 2 - w / 2, desktop.height / 2 - h / 2, w, h);
    canvas.shadow(r, 24 * u, m().shadow, 90);
    canvas.glass(r, 24 * u, &desktop.backdrop, (0, 0), &sheet_glass(t));
    for (i, id) in order.iter().enumerate() {
        let Some(window) = desktop.windows.iter().find(|w| w.id == *id) else {
            continue;
        };
        let cell = Rect::new(r.x + 16 * u + i as i32 * tile, r.y + 16 * u, tile, tile);
        if cell.right() > r.right() {
            break;
        }
        if i == *selected {
            icons::rounded_rect(
                canvas,
                cell.inset(4 * u),
                18 * u,
                with_alpha(if t.dark { WHITE } else { 0 }, 34),
            );
        }
        let icon = Rect::new(cell.x + tile / 2 - 32 * u, cell.y + 14 * u, 64 * u, 64 * u);
        let app = apps::app_of(window).and_then(|a| apps::get(&a));
        apps::draw_icon(canvas, icon, app.as_ref(), &window.title, f.title);
        let name = app.map(|a| a.name).unwrap_or_else(|| window.title.clone());
        let nw = f.small.measure(&name).min(tile - 12 * u);
        f.small.draw_clipped(
            canvas,
            cell.x + tile / 2 - nw / 2,
            icon.bottom() + 8 * u,
            &name,
            tile - 12 * u,
            t.text,
        );
    }
}

/// Bırakınca pencerenin yerleşeceği alan: vurgu renginde yarı saydam, kenarlıklı.
fn draw_snap_preview(canvas: &mut Canvas, r: Rect, t: &Theme) {
    let u = m().unit;
    let area = r.inset(6 * u);
    icons::rounded_rect(canvas, area, 14 * u, with_alpha(t.accent, 150));
    icons::rounded_rect(canvas, area.inset(2 * u), 12 * u, with_alpha(t.accent, 70));
}

/// Paneldeki sanal masaüstü düğmeleri: geçerli olan vurgulu, pencere bulunanın altında nokta.
fn draw_desktop_buttons(desktop: &Desktop, canvas: &mut Canvas, f: &UiFonts) {
    let t = &desktop.theme;
    let u = m().unit;
    let ink = if t.dark { WHITE } else { rgb(0x1c, 0x21, 0x2d) };
    for (i, r) in desktop.desktop_buttons().into_iter().enumerate() {
        let current = i as u8 == desktop.current_desktop;
        if current {
            icons::rounded_rect(canvas, r, 7 * u, t.accent);
        } else {
            icons::rounded_rect(canvas, r, 7 * u, with_alpha(ink, 26));
        }
        let label = alloc::format!("{}", i + 1);
        let lw = f.small.measure(&label);
        f.small.draw(
            canvas,
            r.x + (r.w - lw) / 2,
            r.y + (r.h - f.small.ascent - f.small.descent) / 2,
            &label,
            if current { WHITE } else { ink },
        );
        let used = desktop
            .windows
            .iter()
            .any(|w| w.desktop == i as u8 && (w.visible || w.parked));
        if used && !current {
            icons::disc(
                canvas,
                r.x + r.w / 2,
                r.bottom() + 2 * u,
                u.max(1) + u / 2,
                ink,
            );
        }
    }
}

fn draw_panel(desktop: &Desktop, canvas: &mut Canvas, f: &UiFonts) {
    let t = &desktop.theme;
    let h = m().panel_h;
    let u = m().unit;
    let panel = Rect::new(0, 0, desktop.width, h);
    cached_glass(canvas, &PANEL_GLASS, panel, 0, desktop, &bar_glass(t));
    canvas.fill(
        Rect::new(0, h - 1, desktop.width, 1),
        with_alpha(if t.dark { 0xffffff } else { 0 }, 14),
    );
    let ty = (h - f.body.ascent - f.body.descent) / 2;
    let pad = 16 * u;

    // "B" logosu: başlatıcıyı açar.
    let logo = desktop.logo_rect();
    let launcher_open = desktop.launcher.as_ref().is_some_and(|l| !l.closing());
    if desktop.hovered_logo || launcher_open {
        icons::rounded_rect(
            canvas,
            logo.inset(-4 * u),
            9 * u,
            with_alpha(if t.dark { 0xffffff } else { 0 }, 30),
        );
    }
    let (bg, fg) = if launcher_open {
        (t.accent, WHITE)
    } else if t.dark {
        (WHITE, rgb(0, 0, 0))
    } else {
        (rgb(0x1c, 0x21, 0x2d), WHITE)
    };
    icons::logo(canvas, logo, f.title, bg, fg);

    // Odaklı uygulamanın adı.
    let app = desktop
        .focused()
        .map(|w| {
            apps::app_of(w)
                .and_then(|a| apps::get(&a))
                .map(|a| a.name)
                .unwrap_or_else(|| w.title.clone())
        })
        .unwrap_or_else(|| String::from("Banna OS"));
    let text_color = if t.dark { WHITE } else { rgb(0x1c, 0x21, 0x2d) };
    f.title.draw_clipped(
        canvas,
        logo.right() + 14 * u,
        ty,
        &app,
        desktop.width / 3 - logo.right(),
        text_color,
    );
    let cw = f.body.measure(&desktop.clock_text);
    f.body.draw(
        canvas,
        desktop.width / 2 - cw / 2,
        ty,
        &desktop.clock_text,
        t.text,
    );
    let sw = f.body.measure(&desktop.status_text);
    f.body.draw(
        canvas,
        desktop.width - sw - pad,
        ty,
        &desktop.status_text,
        t.text,
    );
    draw_desktop_buttons(desktop, canvas, f);
}

fn draw_dock(desktop: &Desktop, canvas: &mut Canvas, f: &UiFonts) {
    let t = &desktop.theme;
    let u = m().unit;
    let bg = desktop.dock_background();
    canvas.shadow(bg, 26 * u, 18 * u, if t.dark { 110 } else { 60 });
    cached_glass(canvas, &DOCK_GLASS, bg, 26 * u, desktop, &float_glass(t));
    for (i, item) in desktop.dock.iter().enumerate() {
        if item.after_separator {
            let x = item.rect.x - desktop.base_dock_size() / 4 - u;
            canvas.fill(
                Rect::new(x, bg.y + 14 * u, u.max(1), bg.h - 28 * u),
                with_alpha(if t.dark { 0xffffff } else { 0 }, 40),
            );
        }
        let app = item.app.as_ref().and_then(|id| apps::get(id));
        // Her boyut (büyütme dahil) bir kez çizilip önbellekten bindirilir.
        match &app {
            Some(app) => apps::draw_icon_cached(canvas, item.rect, app, f.title),
            None => apps::draw_icon(canvas, item.rect, None, &item.label, f.title),
        }
        if desktop.hovered_dock == Some(i) {
            icons::rounded_rect(
                canvas,
                item.rect,
                item.rect.w * 23 / 100,
                with_alpha(0xffffff, 28),
            );
        }
        if item.running {
            let d = 4 * u;
            let w = if item.focused { 14 * u } else { d };
            icons::rounded_rect(
                canvas,
                Rect::new(
                    item.rect.x + item.rect.w / 2 - w / 2,
                    bg.bottom() - 7 * u,
                    w,
                    d,
                ),
                d / 2,
                if t.dark { WHITE } else { rgb(0x1c, 0x21, 0x2d) },
            );
        }
    }
    // Üzerine gelinen uygulamanın adı.
    if desktop.menu.is_none()
        && let Some(item) = desktop.hovered_dock.and_then(|i| desktop.dock.get(i))
    {
        let tw = f.small.measure(&item.label);
        let pad = 10 * u;
        let h = f.small.line_height() + 10 * u;
        let tip = Rect::new(
            (item.rect.x + item.rect.w / 2 - tw / 2 - pad)
                .clamp(4, desktop.width - tw - 2 * pad - 4),
            item.rect.y - h - 10 * u,
            tw + 2 * pad,
            h,
        );
        canvas.glass(tip, tip.h / 2, &desktop.backdrop, (0, 0), &sheet_glass(t));
        f.small.draw(
            canvas,
            tip.x + pad,
            tip.y + (h - f.small.ascent - f.small.descent) / 2,
            &item.label,
            t.text,
        );
    }
}
