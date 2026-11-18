//! Sağ tık menüsü: masaüstü, masaüstü simgeleri ve dock öğeleri için.

use alloc::string::String;
use alloc::vec::Vec;

use super::m;
use super::prefs::Theme;
use crate::gfx::text::UiFonts;
use crate::gfx::{Canvas, Rect, Surface, with_alpha};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    NewFolder,
    TerminalAt(String),
    Wallpaper,
    DisplaySettings,
    DockSettings,
    /// Uygulamayı (ek argümanlarla) yeni pencerede başlat.
    Launch(String, Vec<String>),
    /// Uygulamanın pencerelerini öne getir.
    Show(String),
    /// Uygulamanın tüm pencerelerini kapat.
    Quit(String),
    /// Uygulamasız tek pencereyi kapat.
    CloseWindow(u64),
    /// Pencereyi ekranın tamamına ya da bir yarısına yasla.
    SnapWindow(u64, super::Snap),
    /// Yaslanmış pencereyi eski boyutuna döndür.
    RestoreWindow(u64),
    /// Pencereyi başka sanal masaüstüne taşı.
    MoveToDesktop(u64, u8),
    Pin(String),
    Unpin(String),
    Open(String),
    Delete(String),
    ToggleIcons,
}

pub struct Item {
    pub label: String,
    /// `None`: ayırıcı çizgi.
    pub action: Option<Action>,
}

impl Item {
    pub fn new(label: &str, action: Action) -> Self {
        Self {
            label: String::from(label),
            action: Some(action),
        }
    }

    pub fn separator() -> Self {
        Self {
            label: String::new(),
            action: None,
        }
    }
}

pub struct Menu {
    pub rect: Rect,
    pub items: Vec<Item>,
    pub hover: Option<usize>,
}

fn row_height(f: &UiFonts) -> i32 {
    f.body.line_height() + 12 * m().unit
}

fn separator_height() -> i32 {
    9 * m().unit
}

impl Menu {
    /// `(x, y)` noktasında açılır; ekrana sığması için kaydırılır.
    pub fn new(x: i32, y: i32, items: Vec<Item>, f: &UiFonts, screen: (i32, i32)) -> Self {
        let u = m().unit;
        let width = items
            .iter()
            .map(|i| f.body.measure(&i.label))
            .max()
            .unwrap_or(0)
            .max(140 * u)
            + 40 * u;
        let height = items
            .iter()
            .map(|i| {
                if i.action.is_some() {
                    row_height(f)
                } else {
                    separator_height()
                }
            })
            .sum::<i32>()
            + 12 * u;
        let x = x.min(screen.0 - width - 4 * u).max(4 * u);
        let y = if y + height > screen.1 - 4 * u {
            (y - height).max(4 * u)
        } else {
            y
        };
        Self {
            rect: Rect::new(x, y, width, height),
            items,
            hover: None,
        }
    }

    /// Gölge dahil kapladığı alan (hasar bölgesi).
    pub fn footprint(&self) -> Rect {
        self.rect.inset(-24 * m().unit)
    }

    fn rows(&self, f: &UiFonts) -> Vec<(Rect, usize)> {
        let u = m().unit;
        let mut y = self.rect.y + 6 * u;
        let mut out = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let h = if item.action.is_some() {
                row_height(f)
            } else {
                separator_height()
            };
            out.push((
                Rect::new(self.rect.x + 6 * u, y, self.rect.w - 12 * u, h),
                i,
            ));
            y += h;
        }
        out
    }

    /// Noktadaki seçilebilir öğenin sırası.
    pub fn item_at(&self, x: i32, y: i32, f: &UiFonts) -> Option<usize> {
        self.rows(f)
            .into_iter()
            .find(|(r, i)| r.contains(x, y) && self.items[*i].action.is_some())
            .map(|(_, i)| i)
    }

    pub fn draw(&self, canvas: &mut Canvas, t: &Theme, f: &UiFonts, backdrop: &Surface) {
        let u = m().unit;
        let radius = 12 * u;
        canvas.shadow(self.rect, radius, 18 * u, if t.dark { 120 } else { 70 });
        canvas.glass(
            self.rect,
            radius,
            backdrop,
            (0, 0),
            &super::draw::sheet_glass(t),
        );
        let line = with_alpha(if t.dark { 0xffffff } else { 0 }, 30);
        for (row, i) in self.rows(f) {
            let item = &self.items[i];
            if item.action.is_none() {
                canvas.fill(
                    Rect::new(row.x + 8 * u, row.y + row.h / 2, row.w - 16 * u, u.max(1)),
                    line,
                );
                continue;
            }
            if self.hover == Some(i) {
                canvas.fill_rounded(row, 7 * u, t.accent);
            }
            let color = if self.hover == Some(i) {
                0xffff_ffff
            } else {
                t.text
            };
            f.body.draw(
                canvas,
                row.x + 12 * u,
                row.y + (row.h - f.body.ascent - f.body.descent) / 2,
                &item.label,
                color,
            );
        }
    }
}
