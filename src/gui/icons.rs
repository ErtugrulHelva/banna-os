//! Uygulama ve ayar sayfası simgeleri: işaretli uzaklık alanlarıyla (1/16 piksel) kenar
//! yumuşatmalı çizilen vektör şekiller. Görüntü dosyası gerekmez, her boyutta keskindir.

use crate::gfx::text::Font;
use crate::gfx::{Argb, Canvas, Rect, blend, rgb, with_alpha};

const Q: i32 = 16;

fn isqrt(v: i64) -> i64 {
    if v <= 0 {
        return 0;
    }
    let mut r = 1i64 << ((64 - v.leading_zeros()) / 2 + 1);
    loop {
        let next = (r + v / r) / 2;
        if next >= r {
            return r;
        }
        r = next;
    }
}

/// atan2 yaklaşımı: 0..4096 bir tur (0 = sağ, saat yönünde; ekran y'si aşağı).
fn angle(dx: i64, dy: i64) -> i32 {
    if dx == 0 && dy == 0 {
        return 0;
    }
    let (ax, ay) = (dx.abs(), dy.abs());
    // Birinci oktant: a = min/max, atan(a) ≈ a*(512 + 178*(1-a)) (1/4096 tur).
    let (small, big) = if ax > ay { (ay, ax) } else { (ax, ay) };
    let a = small * 1024 / big.max(1);
    let base = (a * (512 * 1024 + 178 * (1024 - a)) / (1024 * 1024)) as i32;
    let oct = if ax > ay { base } else { 1024 - base };
    match (dx >= 0, dy >= 0) {
        (true, true) => oct,
        (false, true) => 2048 - oct,
        (false, false) => 2048 + oct,
        (true, false) => (4096 - oct) % 4096,
    }
}

/// Uzaklık fonksiyonunu (1/16 px, içeride negatif) kaplamaya çevirip `color` ile boyar.
fn paint(
    canvas: &mut Canvas,
    area: Rect,
    color: impl Fn(i32, i32) -> Argb,
    sdf: impl Fn(i64, i64) -> i64,
) {
    let r = area.intersect(&canvas.clip);
    if r.is_empty() {
        return;
    }
    for y in r.y..r.bottom() {
        let py = (y * Q + Q / 2) as i64;
        let row = canvas.span(y, r.x, r.right());
        for (i, p) in row.iter_mut().enumerate() {
            let x = r.x + i as i32;
            let d = sdf((x * Q + Q / 2) as i64, py);
            if d >= Q as i64 / 2 {
                continue;
            }
            let coverage = (Q as i64 / 2 - d).clamp(0, Q as i64) as u32;
            let c = color(x, y);
            let alpha = (c >> 24) * coverage / Q as u32;
            *p = blend(*p, with_alpha(c, alpha as u8));
        }
    }
}

fn solid(c: Argb) -> impl Fn(i32, i32) -> Argb {
    move |_, _| c
}

fn circle(cx: i64, cy: i64, r: i64) -> impl Fn(i64, i64) -> i64 {
    move |x, y| isqrt((x - cx).pow(2) + (y - cy).pow(2)) - r
}

fn ring(cx: i64, cy: i64, r: i64, half: i64) -> impl Fn(i64, i64) -> i64 {
    move |x, y| (isqrt((x - cx).pow(2) + (y - cy).pow(2)) - r).abs() - half
}

fn rounded(x0: i64, y0: i64, w: i64, h: i64, r: i64) -> impl Fn(i64, i64) -> i64 {
    move |x, y| {
        let cx = x0 + w / 2;
        let cy = y0 + h / 2;
        let qx = (x - cx).abs() - (w / 2 - r);
        let qy = (y - cy).abs() - (h / 2 - r);
        let outside = isqrt(qx.max(0).pow(2) + qy.max(0).pow(2));
        let inside = qx.max(qy).min(0);
        outside + inside - r
    }
}

fn capsule(ax: i64, ay: i64, bx: i64, by: i64, half: i64) -> impl Fn(i64, i64) -> i64 {
    move |x, y| {
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = (dx * dx + dy * dy).max(1);
        let t = (((x - ax) * dx + (y - ay) * dy) * 1024 / len2).clamp(0, 1024);
        let px = ax + dx * t / 1024;
        let py = ay + dy * t / 1024;
        isqrt((x - px).pow(2) + (y - py).pow(2)) - half
    }
}

/// Saat yönünde verilen köşelerle üçgen (kenar uzaklıklarının en büyüğü).
fn triangle(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> impl Fn(i64, i64) -> i64 {
    let edge = move |p: (i64, i64), q: (i64, i64), x: i64, y: i64| {
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        ((x - p.0) * dy - (y - p.1) * dx) / isqrt(dx * dx + dy * dy).max(1)
    };
    move |x, y| edge(a, b, x, y).max(edge(b, c, x, y)).max(edge(c, a, x, y))
}

fn mix(a: Argb, b: Argb, t: i32) -> Argb {
    let t = t.clamp(0, 256) as u32;
    let ch = |s: u32| ((((a >> s) & 0xff) * (256 - t) + ((b >> s) & 0xff) * t) >> 8) & 0xff;
    0xff00_0000 | ch(16) << 16 | ch(8) << 8 | ch(0)
}

/// Simge çizim bağlamı: `s` = kenar uzunluğu, koordinatlar simgenin binde biri.
#[derive(Clone, Copy)]
struct Ctx {
    x: i64,
    y: i64,
    s: i64,
    rect: Rect,
}

impl Ctx {
    fn new(rect: Rect) -> Self {
        Self {
            x: rect.x as i64 * Q as i64,
            y: rect.y as i64 * Q as i64,
            s: rect.w as i64 * Q as i64,
            rect,
        }
    }
    fn px(&self, permille: i64) -> i64 {
        self.x + self.s * permille / 1000
    }
    fn py(&self, permille: i64) -> i64 {
        self.y + self.s * permille / 1000
    }
    fn len(&self, permille: i64) -> i64 {
        self.s * permille / 1000
    }
}

fn base(canvas: &mut Canvas, c: &Ctx, top: Argb, bottom: Argb) {
    let rect = c.rect;
    let h = rect.h.max(1);
    paint(
        canvas,
        rect,
        |_, y| mix(top, bottom, (y - rect.y) * 256 / h),
        rounded(c.x, c.y, c.s, c.s, c.len(230)),
    );
    // Üst kenarda ince parlaklık.
    paint(
        canvas,
        Rect::new(rect.x, rect.y, rect.w, rect.h / 2),
        |_, y| with_alpha(0xffffff, (40 - (y - rect.y) * 80 / h).clamp(0, 40) as u8),
        rounded(c.x, c.y, c.s, c.s, c.len(230)),
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Terminal,
    Settings,
    Monitor,
    Paint,
    Plasma,
    Editor,
    About,
    Calculator,
    Store,
    Panel,
    Files,
    Viewer,
    Notes,
    Music,
    // Ayar sayfaları
    Appearance,
    Wallpaper,
    Display,
    Sound,
    Power,
    Input,
    Clock,
    Dock,
    Network,
    Users,
    Info,
}

impl Icon {
    /// `app.conf` içindeki `icon=` adı.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "terminal" => Icon::Terminal,
            "settings" => Icon::Settings,
            "monitor" => Icon::Monitor,
            "paint" => Icon::Paint,
            "plasma" => Icon::Plasma,
            "editor" => Icon::Editor,
            "about" => Icon::About,
            "calculator" => Icon::Calculator,
            "store" => Icon::Store,
            "panel" => Icon::Panel,
            "files" => Icon::Files,
            "viewer" => Icon::Viewer,
            "notes" => Icon::Notes,
            "music" => Icon::Music,
            "appearance" => Icon::Appearance,
            "wallpaper" => Icon::Wallpaper,
            "display" => Icon::Display,
            "sound" => Icon::Sound,
            "power" => Icon::Power,
            "input" => Icon::Input,
            "clock" => Icon::Clock,
            "dock" => Icon::Dock,
            "network" => Icon::Network,
            "users" => Icon::Users,
            "info" => Icon::Info,
            _ => return None,
        })
    }
}

/// `rect` kare olmalıdır.
pub fn draw(canvas: &mut Canvas, rect: Rect, icon: Icon) {
    if rect.intersect(&canvas.clip).is_empty() {
        return;
    }
    let c = Ctx::new(rect);
    let white = rgb(0xff, 0xff, 0xff);
    let w = move |p| c.len(p);
    match icon {
        Icon::Terminal => {
            base(canvas, &c, rgb(0x34, 0x3b, 0x4b), rgb(0x14, 0x18, 0x21));
            let g = rgb(0x6c, 0xe8, 0x9a);
            paint(
                canvas,
                rect,
                solid(g),
                capsule(c.px(270), c.py(330), c.px(450), c.py(480), w(45)),
            );
            paint(
                canvas,
                rect,
                solid(g),
                capsule(c.px(450), c.py(480), c.px(270), c.py(630), w(45)),
            );
            paint(
                canvas,
                rect,
                solid(white),
                capsule(c.px(520), c.py(650), c.px(740), c.py(650), w(42)),
            );
        }
        Icon::Settings => {
            base(canvas, &c, rgb(0x9a, 0xa3, 0xb5), rgb(0x4b, 0x54, 0x68));
            gear(canvas, &c, rgb(0xf2, 0xf4, 0xf8), rgb(0x6a, 0x73, 0x87));
        }
        Icon::Monitor => {
            base(canvas, &c, rgb(0x34, 0xd3, 0x8f), rgb(0x0f, 0x7a, 0x4c));
            let pts = [(220, 640), (380, 470), (500, 560), (640, 330), (780, 420)];
            for p in pts.windows(2) {
                paint(
                    canvas,
                    rect,
                    solid(white),
                    capsule(
                        c.px(p[0].0),
                        c.py(p[0].1),
                        c.px(p[1].0),
                        c.py(p[1].1),
                        w(40),
                    ),
                );
            }
            paint(
                canvas,
                rect,
                solid(with_alpha(white, 140)),
                capsule(c.px(220), c.py(770), c.px(780), c.py(770), w(28)),
            );
        }
        Icon::Paint => {
            base(canvas, &c, rgb(0xff, 0x9a, 0x5b), rgb(0xe0, 0x3e, 0x7c));
            paint(
                canvas,
                rect,
                solid(white),
                circle(c.px(470), c.py(500), w(270)),
            );
            for (x, y, col) in [
                (370, 400, rgb(0xff, 0x5f, 0x57)),
                (520, 360, rgb(0x5f, 0xb4, 0xff)),
                (620, 480, rgb(0x4f, 0xd1, 0x8b)),
                (380, 560, rgb(0xf2, 0xc8, 0x4b)),
            ] {
                paint(canvas, rect, solid(col), circle(c.px(x), c.py(y), w(55)));
            }
            paint(
                canvas,
                rect,
                solid(rgb(0xe0, 0x3e, 0x7c)),
                circle(c.px(560), c.py(620), w(60)),
            );
            paint(
                canvas,
                rect,
                solid(rgb(0x3a, 0x2a, 0x22)),
                capsule(c.px(640), c.py(700), c.px(800), c.py(840), w(38)),
            );
        }
        Icon::Plasma => {
            base(canvas, &c, rgb(0x9b, 0x6c, 0xff), rgb(0x30, 0x22, 0x8c));
            for (r, col) in [
                (300, rgb(0xff, 0x6f, 0xb1)),
                (210, rgb(0xff, 0xc8, 0x5a)),
                (120, rgb(0x5f, 0xe0, 0xff)),
            ] {
                paint(
                    canvas,
                    rect,
                    solid(col),
                    ring(c.px(500), c.py(500), w(r), w(38)),
                );
            }
        }
        Icon::Editor => {
            base(canvas, &c, rgb(0x5a, 0xa8, 0xff), rgb(0x23, 0x5f, 0xd6));
            paint(
                canvas,
                rect,
                solid(white),
                rounded(c.px(270), c.py(200), w(460), w(600), w(60)),
            );
            for (y, len) in [(340, 300), (440, 300), (540, 300), (640, 180)] {
                paint(
                    canvas,
                    rect,
                    solid(rgb(0x8a, 0xa8, 0xd8)),
                    capsule(c.px(350), c.py(y), c.px(350 + len), c.py(y), w(22)),
                );
            }
        }
        Icon::About | Icon::Info => {
            base(canvas, &c, rgb(0x6c, 0xbc, 0xff), rgb(0x2a, 0x62, 0xd0));
            paint(
                canvas,
                rect,
                solid(white),
                circle(c.px(500), c.py(290), w(70)),
            );
            paint(
                canvas,
                rect,
                solid(white),
                capsule(c.px(500), c.py(450), c.px(500), c.py(740), w(62)),
            );
        }
        Icon::Calculator => {
            base(canvas, &c, rgb(0xff, 0xae, 0x52), rgb(0xe0, 0x63, 0x1b));
            for (x, y) in [(260, 260), (530, 260), (260, 530), (530, 530)] {
                paint(
                    canvas,
                    rect,
                    solid(with_alpha(white, 235)),
                    rounded(c.px(x), c.py(y), w(210), w(210), w(50)),
                );
            }
        }
        Icon::Store => {
            base(canvas, &c, rgb(0x7a, 0x6c, 0xff), rgb(0x2f, 0x5b, 0xe0));
            // Alışveriş çantası: iki ince sap, geniş gövde ve kapak şeridi.
            for hx in [390, 610] {
                paint(canvas, rect, solid(white), move |x, y| {
                    let d = ring(c.px(hx), c.py(380), w(90), w(26))(x, y);
                    if y < c.py(380) { d } else { d.max(w(60)) }
                });
            }
            paint(
                canvas,
                rect,
                solid(white),
                rounded(c.px(200), c.py(370), w(600), w(440), w(60)),
            );
            paint(
                canvas,
                rect,
                solid(rgb(0xc9, 0xc4, 0xff)),
                rounded(c.px(200), c.py(370), w(600), w(70), w(30)),
            );
        }
        Icon::Panel => {
            base(canvas, &c, rgb(0x2b, 0x3a, 0x67), rgb(0x10, 0x16, 0x2e));
            let tiles = [
                (200, 200, 360, 280, rgb(0x5f, 0xb4, 0xff)),
                (600, 200, 200, 280, rgb(0xff, 0x6f, 0xb1)),
                (200, 520, 200, 280, rgb(0x4f, 0xd1, 0x8b)),
                (440, 520, 360, 280, rgb(0xff, 0xc8, 0x5a)),
            ];
            for (x, y, tw, th, col) in tiles {
                paint(
                    canvas,
                    rect,
                    solid(col),
                    rounded(c.px(x), c.py(y), w(tw), w(th), w(50)),
                );
            }
        }
        Icon::Files => {
            base(canvas, &c, rgb(0x4f, 0xb8, 0xff), rgb(0x1d, 0x6f, 0xd8));
            paint(
                canvas,
                rect,
                solid(rgb(0xff, 0xd8, 0x6a)),
                rounded(c.px(200), c.py(270), w(260), w(160), w(40)),
            );
            paint(
                canvas,
                rect,
                solid(rgb(0xff, 0xc4, 0x3d)),
                rounded(c.px(200), c.py(340), w(600), w(420), w(50)),
            );
            paint(
                canvas,
                rect,
                solid(rgb(0xff, 0xe2, 0x8a)),
                rounded(c.px(200), c.py(420), w(600), w(340), w(50)),
            );
        }
        Icon::Viewer => {
            base(canvas, &c, rgb(0x46, 0xd6, 0xc3), rgb(0x16, 0x7f, 0x9a));
            paint(
                canvas,
                rect,
                solid(white),
                rounded(c.px(190), c.py(240), w(620), w(520), w(60)),
            );
            paint(
                canvas,
                rect,
                solid(rgb(0xff, 0xc8, 0x5a)),
                circle(c.px(640), c.py(390), w(60)),
            );
            for (peak, left, right, col) in [
                ((400, 400), 230, 600, rgb(0x2f, 0x9e, 0x6b)),
                ((610, 500), 430, 770, rgb(0x4f, 0xc1, 0x8b)),
            ] {
                paint(
                    canvas,
                    rect,
                    solid(col),
                    triangle(
                        (c.px(peak.0), c.py(peak.1)),
                        (c.px(right), c.py(700)),
                        (c.px(left), c.py(700)),
                    ),
                );
            }
        }
        Icon::Notes => {
            base(canvas, &c, rgb(0xff, 0xd2, 0x5c), rgb(0xf0, 0x92, 0x2a));
            paint(
                canvas,
                rect,
                solid(white),
                rounded(c.px(240), c.py(190), w(520), w(620), w(60)),
            );
            for (y, len) in [(360, 340), (460, 340), (560, 340), (660, 200)] {
                paint(
                    canvas,
                    rect,
                    solid(rgb(0xe0, 0xa8, 0x50)),
                    capsule(c.px(330), c.py(y), c.px(330 + len), c.py(y), w(22)),
                );
            }
            paint(
                canvas,
                rect,
                solid(rgb(0x3a, 0x2a, 0x22)),
                capsule(c.px(600), c.py(700), c.px(790), c.py(510), w(34)),
            );
        }
        Icon::Music => {
            // B-Player: mor-pembe cam zemin üzerinde çift sekizlik nota.
            base(canvas, &c, rgb(0xff, 0x6a, 0xa8), rgb(0x6c, 0x3c, 0xe8));
            paint(
                canvas,
                Rect::new(rect.x, rect.y, rect.w, rect.h),
                |_, y| {
                    with_alpha(
                        0xffffff,
                        (60 - (y - rect.y) * 60 / rect.h.max(1)).clamp(0, 60) as u8,
                    )
                },
                circle(c.px(500), c.py(250), w(420)),
            );
            for (x, y) in [(360, 700), (680, 630)] {
                paint(canvas, rect, solid(white), circle(c.px(x), c.py(y), w(105)));
            }
            for (x, y) in [(445, 700), (765, 630)] {
                paint(
                    canvas,
                    rect,
                    solid(white),
                    capsule(c.px(x), c.py(y), c.px(x), c.py(y - 400), w(30)),
                );
            }
            paint(
                canvas,
                rect,
                solid(white),
                capsule(c.px(445), c.py(330), c.px(765), c.py(260), w(58)),
            );
        }
        Icon::Appearance => {
            base(canvas, &c, rgb(0xff, 0x8a, 0x5b), rgb(0xd9, 0x46, 0x8a));
            paint(
                canvas,
                rect,
                solid(white),
                circle(c.px(500), c.py(500), w(290)),
            );
            for (x, y, col) in [
                (400, 400, rgb(0xff, 0x5f, 0x57)),
                (600, 400, rgb(0x5f, 0xb4, 0xff)),
                (400, 600, rgb(0x4f, 0xd1, 0x8b)),
                (600, 600, rgb(0xf2, 0xc8, 0x4b)),
            ] {
                paint(canvas, rect, solid(col), circle(c.px(x), c.py(y), w(70)));
            }
        }
        Icon::Wallpaper => {
            base(canvas, &c, rgb(0x5f, 0xc8, 0xff), rgb(0x2a, 0x7a, 0xd8));
            paint(
                canvas,
                rect,
                solid(rgb(0xff, 0xe2, 0x7a)),
                circle(c.px(650), c.py(330), w(90)),
            );
            paint(canvas, rect, solid(white), move |x, y| {
                // Üçgen dağ: eğik iki kenar ve taban.
                let (dx, dy) = (x - c.px(400), y - c.py(380));
                ((3 * dx.abs() - 2 * dy) * 10 / 36).max(y - c.py(760))
            });
            paint(canvas, rect, solid(with_alpha(white, 200)), move |x, y| {
                let (dx, dy) = (x - c.px(650), y - c.py(520));
                ((3 * dx.abs() - 2 * dy) * 10 / 36).max(y - c.py(760))
            });
        }
        Icon::Display => {
            base(canvas, &c, rgb(0x5a, 0x9c, 0xff), rgb(0x2f, 0x4f, 0xc9));
            paint(canvas, rect, solid(white), move |x, y| {
                let outer = rounded(c.px(200), c.py(240), w(600), w(400), w(50))(x, y);
                let inner = rounded(c.px(260), c.py(300), w(480), w(280), w(20))(x, y);
                outer.max(-inner)
            });
            paint(
                canvas,
                rect,
                solid(white),
                capsule(c.px(380), c.py(760), c.px(620), c.py(760), w(40)),
            );
        }
        Icon::Sound => {
            base(canvas, &c, rgb(0xff, 0x6f, 0x8a), rgb(0xd0, 0x2a, 0x5a));
            paint(
                canvas,
                rect,
                solid(white),
                rounded(c.px(220), c.py(400), w(150), w(200), w(30)),
            );
            paint(canvas, rect, solid(white), move |x, y| {
                let tip = c.px(540);
                let (cy, half) = (c.py(500), w(260));
                let t = ((x - c.px(330)) * 1024 / (tip - c.px(330)).max(1)).clamp(0, 1024);
                let h = w(100) + (half - w(100)) * t / 1024;
                ((y - cy).abs() - h).max(c.px(330) - x).max(x - tip)
            });
            for r in [300, 420] {
                paint(canvas, rect, solid(white), move |x, y| {
                    let a = angle(x - c.px(450), y - c.py(500));
                    let d = ring(c.px(450), c.py(500), w(r), w(34))(x, y);
                    if (3500..=4096).contains(&a) || (0..=600).contains(&a) {
                        d
                    } else {
                        d.max(w(80))
                    }
                });
            }
        }
        Icon::Power => {
            base(canvas, &c, rgb(0x5f, 0xd7, 0x8f), rgb(0x1f, 0x9a, 0x5a));
            paint(canvas, rect, solid(white), move |x, y| {
                let a = angle(x - c.px(500), y - c.py(530));
                let d = ring(c.px(500), c.py(530), w(250), w(45))(x, y);
                if (2560..=3584).contains(&a) {
                    d.max(w(60))
                } else {
                    d
                }
            });
            paint(
                canvas,
                rect,
                solid(white),
                capsule(c.px(500), c.py(200), c.px(500), c.py(500), w(48)),
            );
        }
        Icon::Input => {
            base(canvas, &c, rgb(0x8a, 0x94, 0xa6), rgb(0x4b, 0x54, 0x68));
            paint(
                canvas,
                rect,
                solid(white),
                rounded(c.px(150), c.py(300), w(700), w(400), w(60)),
            );
            let k = rgb(0x5a, 0x63, 0x78);
            for row in 0..2 {
                for col in 0..5 {
                    paint(
                        canvas,
                        rect,
                        solid(k),
                        rounded(
                            c.px(220 + col * 120),
                            c.py(360 + row * 110),
                            w(80),
                            w(80),
                            w(14),
                        ),
                    );
                }
            }
            paint(
                canvas,
                rect,
                solid(k),
                rounded(c.px(320), c.py(580), w(360), w(70), w(14)),
            );
        }
        Icon::Clock => {
            base(canvas, &c, rgb(0xff, 0xae, 0x52), rgb(0xe0, 0x63, 0x1b));
            paint(
                canvas,
                rect,
                solid(white),
                circle(c.px(500), c.py(500), w(320)),
            );
            let k = rgb(0x3a, 0x2a, 0x22);
            paint(
                canvas,
                rect,
                solid(k),
                capsule(c.px(500), c.py(500), c.px(500), c.py(290), w(36)),
            );
            paint(
                canvas,
                rect,
                solid(k),
                capsule(c.px(500), c.py(500), c.px(650), c.py(580), w(36)),
            );
        }
        Icon::Dock => {
            base(canvas, &c, rgb(0x9b, 0x6c, 0xff), rgb(0x4a, 0x32, 0xb8));
            paint(
                canvas,
                rect,
                solid(with_alpha(white, 90)),
                rounded(c.px(150), c.py(600), w(700), w(220), w(60)),
            );
            for (i, col) in [
                rgb(0xff, 0x6f, 0xb1),
                rgb(0x5f, 0xe0, 0xff),
                rgb(0xff, 0xc8, 0x5a),
            ]
            .into_iter()
            .enumerate()
            {
                paint(
                    canvas,
                    rect,
                    solid(col),
                    rounded(c.px(220 + i as i64 * 200), c.py(630), w(160), w(160), w(40)),
                );
            }
        }
        Icon::Network => {
            base(canvas, &c, rgb(0x4a, 0xa8, 0xff), rgb(0x1f, 0x6a, 0xd8));
            paint(
                canvas,
                rect,
                solid(white),
                circle(c.px(500), c.py(720), w(65)),
            );
            for r in [200, 340, 480] {
                paint(canvas, rect, solid(white), move |x, y| {
                    let a = angle(x - c.px(500), y - c.py(720));
                    let d = ring(c.px(500), c.py(720), w(r), w(40))(x, y);
                    if (2560..=3584).contains(&a) {
                        d
                    } else {
                        d.max(w(70))
                    }
                });
            }
        }
        Icon::Users => {
            base(canvas, &c, rgb(0x3c, 0xc9, 0xd6), rgb(0x16, 0x8a, 0x9a));
            paint(
                canvas,
                rect,
                solid(white),
                circle(c.px(500), c.py(370), w(150)),
            );
            paint(canvas, rect, solid(white), move |x, y| {
                let body = rounded(c.px(240), c.py(560), w(520), w(420), w(200))(x, y);
                body.max(y - c.py(800))
            });
        }
    }
}

fn gear(canvas: &mut Canvas, c: &Ctx, fg: Argb, hole: Argb) {
    let rect = c.rect;
    let (cx, cy) = (c.px(500), c.py(500));
    let (outer, inner) = (c.len(330), c.len(255));
    paint(canvas, rect, solid(fg), move |x, y| {
        let r = isqrt((x - cx).pow(2) + (y - cy).pow(2));
        let a = angle(x - cx, y - cy) as i64;
        // 8 diş: her dişin merkezine açısal uzaklık (0..256).
        let phase = (a * 8) % 4096;
        let off = (phase - 2048).abs();
        let tooth = if off > 1100 {
            outer
        } else if off > 700 {
            inner + (outer - inner) * (off - 700) / 400
        } else {
            inner
        };
        r - tooth
    });
    paint(canvas, rect, solid(hole), circle(cx, cy, c.len(110)));
}

/// Panelin sol üstündeki "B" logosu.
pub fn logo(canvas: &mut Canvas, rect: Rect, font: &Font, background: Argb, letter: Argb) {
    let c = Ctx::new(rect);
    paint(
        canvas,
        rect,
        solid(background),
        rounded(c.x, c.y, c.s, c.s, c.len(260)),
    );
    let bw = font.measure("B");
    font.draw(
        canvas,
        rect.x + rect.w / 2 - bw / 2,
        rect.y + (rect.h - font.ascent - font.descent) / 2,
        "B",
        letter,
    );
}

/// Bilinmeyen uygulama: baş harfli gri simge.
/// Görüntü simgesinin içeriğe sığdırılmış boyutu (simge zemininin %84'ü, oran korunur).
pub fn image_fit(rect: Rect, width: u32, height: u32) -> (u32, u32) {
    let room = (rect.w * 84 / 100).max(1) as u32;
    let (w, h) = (width.max(1), height.max(1));
    if w >= h {
        (room, (h * room / w).max(1))
    } else {
        ((w * room / h).max(1), room)
    }
}

/// Görüntü simgesi: açık renkli simge zemini üzerinde ortalanmış, `image_fit` boyutunda
/// küçültülmüş PNG pikselleri (0xAARRGGBB).
pub fn image(canvas: &mut Canvas, rect: Rect, pixels: &[u32], w: u32, h: u32) {
    if rect.intersect(&canvas.clip).is_empty() {
        return;
    }
    let c = Ctx::new(rect);
    base(canvas, &c, rgb(0xff, 0xff, 0xff), rgb(0xe6, 0xe9, 0xee));
    let (w, h) = (w as i32, h as i32);
    canvas.blend_image(
        pixels,
        w,
        h,
        rect.x + rect.w / 2 - w / 2,
        rect.y + rect.h / 2 - h / 2,
    );
}

pub fn letter(canvas: &mut Canvas, rect: Rect, text: &str, font: &Font) {
    let c = Ctx::new(rect);
    base(canvas, &c, rgb(0x7a, 0x84, 0x96), rgb(0x45, 0x4d, 0x5e));
    let first: alloc::string::String = text
        .chars()
        .take(1)
        .flat_map(|c| c.to_uppercase())
        .collect();
    let lw = font.measure(&first);
    font.draw(
        canvas,
        rect.x + rect.w / 2 - lw / 2,
        rect.y + (rect.h - font.ascent - font.descent) / 2,
        &first,
        rgb(0xff, 0xff, 0xff),
    );
}

/// Kenar yumuşatmalı daire (düğmeler, renk örnekleri, kaydırıcı topuzu).
pub fn disc(canvas: &mut Canvas, cx: i32, cy: i32, r: i32, color: Argb) {
    let area = Rect::new(cx - r - 1, cy - r - 1, 2 * r + 2, 2 * r + 2);
    paint(
        canvas,
        area,
        solid(color),
        circle(
            cx as i64 * Q as i64,
            cy as i64 * Q as i64,
            r as i64 * Q as i64,
        ),
    );
}

/// Kenar yumuşatmalı halka.
pub fn circle_outline(canvas: &mut Canvas, cx: i32, cy: i32, r: i32, width: i32, color: Argb) {
    let area = Rect::new(
        cx - r - width,
        cy - r - width,
        2 * (r + width),
        2 * (r + width),
    );
    paint(
        canvas,
        area,
        solid(color),
        ring(
            cx as i64 * Q as i64,
            cy as i64 * Q as i64,
            r as i64 * Q as i64,
            width as i64 * Q as i64 / 2,
        ),
    );
}

/// Kenar yumuşatmalı yuvarlak köşeli dikdörtgen (yarı saydam renkler dahil).
pub fn rounded_rect(canvas: &mut Canvas, rect: Rect, radius: i32, color: Argb) {
    let q = Q as i64;
    paint(
        canvas,
        rect,
        solid(color),
        rounded(
            rect.x as i64 * q,
            rect.y as i64 * q,
            rect.w as i64 * q,
            rect.h as i64 * q,
            radius as i64 * q,
        ),
    );
}

/// Onay işareti (açık anahtarlar ve seçili öğeler).
pub fn check(canvas: &mut Canvas, rect: Rect, color: Argb) {
    let c = Ctx::new(rect);
    let w = c.len(90);
    paint(
        canvas,
        rect,
        solid(color),
        capsule(c.px(220), c.py(520), c.px(420), c.py(720), w),
    );
    paint(
        canvas,
        rect,
        solid(color),
        capsule(c.px(420), c.py(720), c.px(780), c.py(300), w),
    );
}
