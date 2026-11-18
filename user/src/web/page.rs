//! Bir sekmedeki sayfa: belge + stil sayfaları + JavaScript + yerleşim + görseller.
//! Yükleme adım adım ilerler (`load_step`): önce stil sayfaları, sonra betikler
//! (belge sırasıyla), en son görseller; her adımdan sonra sayfa yeniden çizilebilir.

use alloc::string::String;
use alloc::vec::Vec;

use super::css::{self, Sheet};
use super::dom::{Document, Kind, NodeId, ROOT};
use super::js::{self, Interp, Value};
use super::layout::{self, Fonts, Layout, RectF};
use super::paint::{Images, Painter};
use super::style::{self, Style};
use super::{html, url};
use crate::gfx::{Canvas, Rect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Styles,
    Scripts,
    Images,
    Done,
}

/// Tarayıcıdan sayfaya iletilen sonuç (gezinme isteği vb.).
pub enum Action {
    None,
    Navigate(String),
    Post(String, String),
    Back,
    Redraw,
}

pub struct Page {
    pub interp: Interp,
    pub sheets: Vec<(usize, Sheet)>,
    pub styles: Vec<Style>,
    pub layout: Layout,
    pub images: Images,
    pub stage: Stage,
    pub scroll: f32,
    pub viewport: (f32, f32),
    pub scale: f32,
    pub javascript: bool,
    pub load_images: bool,
    pending_sheets: Vec<(usize, String)>,
    pending_scripts: Vec<NodeId>,
    pending_images: Vec<String>,
    pub errors: Vec<String>,
    pub needs_layout: bool,
    loaded_event: bool,
    /// Son stil + yerleşim süresi (ms); yavaş sayfalarda :hover yeniden hesaplanmaz.
    pub layout_ms: u64,
}

fn now() -> u64 {
    crate::time::uptime_ms()
}

impl Page {
    pub fn new(source: &str, page_url: &str, javascript: bool) -> Self {
        let doc = html::parse(source);
        let interp = Interp::new(doc, page_url, now, super::js_fetch);
        let mut page = Self {
            interp,
            sheets: Vec::new(),
            styles: Vec::new(),
            layout: Layout {
                items: Vec::new(),
                boxes: Vec::new(),
                height: 0.0,
                width: 0.0,
            },
            images: Images::new(),
            stage: Stage::Styles,
            scroll: 0.0,
            viewport: (800.0, 600.0),
            scale: 1.0,
            javascript,
            load_images: true,
            pending_sheets: Vec::new(),
            pending_scripts: Vec::new(),
            pending_images: Vec::new(),
            errors: Vec::new(),
            needs_layout: true,
            loaded_event: false,
            layout_ms: 0,
        };
        page.collect_resources();
        page
    }

    pub fn doc(&self) -> &Document {
        &self.interp.doc
    }

    pub fn url(&self) -> &str {
        &self.interp.url
    }

    pub fn title(&self) -> String {
        self.interp.doc.title()
    }

    fn base_url(&self) -> String {
        let doc = &self.interp.doc;
        doc.find_tag("base")
            .and_then(|b| doc.attr(b, "href"))
            .map(|h| url::resolve(&self.interp.url, h))
            .unwrap_or_else(|| self.interp.url.clone())
    }

    fn collect_resources(&mut self) {
        let base = self.base_url();
        let doc = &self.interp.doc;
        for (order, node) in doc.descendants(ROOT).into_iter().enumerate() {
            match doc.tag(node) {
                Some("style") => {
                    let text = doc.text_content(node);
                    self.sheets.push((order, css::parse_sheet(&text)));
                }
                Some("link") => {
                    let rel = doc.attr(node, "rel").unwrap_or("").to_ascii_lowercase();
                    let media = doc.attr(node, "media").unwrap_or("all").to_ascii_lowercase();
                    if rel.split_whitespace().any(|r| r == "stylesheet")
                        && !media.contains("print")
                        && let Some(href) = doc.attr(node, "href")
                    {
                        self.pending_sheets.push((order, url::resolve(&base, href)));
                    }
                }
                Some("script") if self.javascript => self.pending_scripts.push(node),
                _ => {}
            }
        }
        self.sheets.sort_by_key(|(o, _)| *o);
    }

    /// Sıradaki kaynağı yükler; bir şey değiştiyse `true` (yeniden çizim).
    pub fn load_step(&mut self) -> bool {
        match self.stage {
            Stage::Styles => {
                if let Some((order, href)) = self.pending_sheets.first().cloned() {
                    self.pending_sheets.remove(0);
                    match super::fetch(&href, "GET", &[], "") {
                        Ok(r) if r.status == 200 => {
                            let text = String::from_utf8_lossy(&r.body);
                            let sheet = css::parse_sheet(&text);
                            let at = self.sheets.iter().position(|(o, _)| *o > order).unwrap_or(self.sheets.len());
                            self.sheets.insert(at, (order, sheet));
                        }
                        Ok(r) => self.errors.push(alloc::format!("CSS {}: HTTP {}", href, r.status)),
                        Err(e) => self.errors.push(alloc::format!("CSS {}: {}", href, e)),
                    }
                    self.needs_layout = true;
                    return true;
                }
                self.stage = Stage::Scripts;
                self.needs_layout = true;
                true
            }
            Stage::Scripts => {
                if let Some(node) = self.pending_scripts.first().copied() {
                    self.pending_scripts.remove(0);
                    self.run_script_node(node);
                    return true;
                }
                if !self.loaded_event {
                    self.loaded_event = true;
                    self.fire_document_event("DOMContentLoaded");
                    self.fire_document_event("readystatechange");
                    self.fire_window_event("load");
                }
                self.stage = Stage::Images;
                self.queue_images();
                true
            }
            Stage::Images => {
                if let Some(src) = self.pending_images.first().cloned() {
                    self.pending_images.remove(0);
                    if !self.images.map.contains_key(&src)
                        && let Ok(r) = super::fetch(&src, "GET", &[], "")
                            && r.status == 200
                            && let Some(img) = super::decode_image(&r.body)
                            && (img.width as u64) * (img.height as u64) < 25_000_000
                        {
                            self.images.map.insert(src, img);
                            self.needs_layout = true;
                        }
                    return true;
                }
                self.stage = Stage::Done;
                false
            }
            Stage::Done => false,
        }
    }

    fn queue_images(&mut self) {
        if !self.load_images {
            return;
        }
        let doc = &self.interp.doc;
        let mut wanted = Vec::new();
        for node in doc.descendants(ROOT) {
            if doc.is(node, "img") {
                let src = doc.attr(node, "src").or_else(|| doc.attr(node, "data-src"));
                if let Some(src) = src.filter(|s| !s.is_empty()) {
                    let full = url::resolve(&self.interp.url, src);
                    // Göreli adres DOM'da tam adrese çevrilir (görsel önbelleği anahtarı).
                    wanted.push((node, full));
                }
            }
        }
        for (node, full) in wanted {
            self.interp.doc.set_attr(node, "src", &full);
            if !self.pending_images.contains(&full) {
                self.pending_images.push(full);
            }
        }
        for st in &self.styles {
            if let Some(bg) = &st.background_image {
                let full = url::resolve(&self.interp.url, bg);
                if !self.pending_images.contains(&full) {
                    self.pending_images.push(full);
                }
            }
        }
    }

    fn run_script_node(&mut self, node: NodeId) {
        let doc = &self.interp.doc;
        if doc.attr(node, "data-banna-ran").is_some() {
            return;
        }
        let kind = doc.attr(node, "type").unwrap_or("").to_ascii_lowercase();
        if !(kind.is_empty() || kind.contains("javascript") || kind == "module") || kind == "module" && doc.attr(node, "src").is_none() && false {
            return;
        }
        if kind == "module" {
            self.errors.push(String::from("ES modülü atlandı"));
            return;
        }
        let source = match doc.attr(node, "src") {
            Some(src) => {
                let full = url::resolve(&self.interp.url, src);
                match super::fetch(&full, "GET", &[], "") {
                    Ok(r) if r.status == 200 => String::from_utf8_lossy(&r.body).into_owned(),
                    Ok(r) => {
                        self.errors.push(alloc::format!("JS {}: HTTP {}", full, r.status));
                        return;
                    }
                    Err(e) => {
                        self.errors.push(alloc::format!("JS {}: {}", full, e));
                        return;
                    }
                }
            }
            None => doc.text_content(node),
        };
        self.interp.doc.set_attr(node, "data-banna-ran", "1");
        self.interp.run_script(&source);
        self.after_js();
    }

    fn fire_document_event(&mut self, kind: &str) {
        if !self.javascript {
            return;
        }
        let ev = js::dom::make_event(&mut self.interp, kind, ROOT, &[]);
        let _ = js::dom::dispatch_event(&mut self.interp, ROOT, kind, ev);
        self.after_js();
    }

    fn fire_window_event(&mut self, kind: &str) {
        if !self.javascript {
            return;
        }
        let g = Value::Obj(self.interp.global);
        let ev = js::dom::make_event(&mut self.interp, kind, ROOT, &[]);
        let listeners: Vec<Value> = self
            .interp
            .listeners
            .iter()
            .filter(|l| matches!(l.target, js::Target::Window) && l.kind.as_ref() == kind)
            .map(|l| l.callback.clone())
            .collect();
        for cb in listeners {
            if let Err(e) = self.interp.call(&cb, g.clone(), alloc::vec![ev.clone()]) {
                let t = self.interp.error_text(&e);
                self.interp.host.console.push((2, t));
            }
        }
        if let Ok(onload) = self.interp.get(&g, &alloc::format!("on{}", kind))
            && self.interp.is_callable(&onload)
        {
            let _ = self.interp.call(&onload, g, alloc::vec![ev]);
        }
        self.interp.run_microtasks();
        self.after_js();
    }

    fn after_js(&mut self) {
        if self.interp.doc.dirty {
            self.needs_layout = true;
        }
        self.interp.maybe_gc();
    }

    /// Zamanlayıcılar ve mikro görevler; gezinme isteği varsa döndürür.
    pub fn tick(&mut self) -> Action {
        if self.javascript && !self.interp.timers.is_empty() {
            self.interp.run_due_timers(now(), false);
            self.after_js();
        }
        self.take_action()
    }

    pub fn take_action(&mut self) -> Action {
        if let Some(target) = self.interp.host.navigate.take() {
            if target == "banna:back" {
                return Action::Back;
            }
            if let Some(rest) = target.strip_prefix("banna:post:") {
                let (u, body) = rest.split_once('\n').unwrap_or((rest, ""));
                return Action::Post(String::from(u), String::from(body));
            }
            return Action::Navigate(target);
        }
        if self.needs_layout || self.interp.doc.dirty {
            return Action::Redraw;
        }
        Action::None
    }

    /// En yakın zamanlayıcıya kalan süre (olay döngüsü bekleme süresi için).
    pub fn next_timer_ms(&self) -> Option<u64> {
        let t = now();
        self.interp
            .timers
            .iter()
            .map(|timer| timer.due.saturating_sub(t))
            .min()
    }

    pub fn relayout(&mut self, fonts: &dyn Fonts) {
        let started = now();
        let ctx = style::Context {
            viewport: self.viewport,
            root_font: 16.0,
        };
        let sheets: Vec<Sheet> = self.sheets.iter().map(|(_, s)| s.clone()).collect();
        self.styles = style::compute(&self.interp.doc, &sheets, &ctx);
        self.layout = layout::layout(&self.interp.doc, &self.styles, fonts, &self.images, self.scale, self.viewport);
        self.interp.rects = self.layout.boxes.clone();
        self.interp.viewport = self.viewport;
        self.interp.doc.dirty = false;
        self.needs_layout = false;
        self.layout_ms = now() - started;
        let max = (self.layout.height - self.viewport.1).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    pub fn paint(&mut self, canvas: Canvas, area: Rect, fonts: &dyn Fonts, caret: bool) {
        let mut painter = Painter {
            canvas,
            fonts,
            scale: self.scale,
            origin: (area.x, area.y),
            scroll: self.scroll,
            viewport: area,
            caret,
        };
        painter.paint(&self.layout.items, &self.interp.doc, &self.styles, &mut self.images);
    }

    pub fn scroll_by(&mut self, dy: f32) -> bool {
        let max = (self.layout.height - self.viewport.1).max(0.0);
        let old = self.scroll;
        self.scroll = (self.scroll + dy).clamp(0.0, max);
        self.scroll != old
    }

    /// Sayfa koordinatındaki (CSS px, kaydırma dahil) öğe.
    pub fn hit(&self, x: f32, y: f32) -> Option<NodeId> {
        self.layout.hit(x, y + self.scroll)
    }

    pub fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let mut node = self.hit(x, y);
        let doc = &self.interp.doc;
        while let Some(n) = node {
            if doc.is(n, "a")
                && let Some(href) = doc.attr(n, "href")
            {
                return Some(url::resolve(&self.interp.url, href));
            }
            node = doc.nodes[n].parent;
        }
        None
    }

    pub fn set_hover(&mut self, x: f32, y: f32) -> bool {
        let node = self.hit(x, y);
        if node != self.interp.doc.hover {
            self.interp.doc.hover = node;
            // :hover stilleri yalnızca sayfada :hover kuralı varsa yeniden hesaplanır.
            let uses_hover = self.sheets.iter().any(|(_, s)| {
                s.rules.iter().any(|r| r.selectors.iter().any(|sel| sel.parts.iter().any(|(c, _)| c.contains(&css::Simple::Hover))))
            });
            if uses_hover && self.layout_ms < 80 {
                self.needs_layout = true;
                return true;
            }
        }
        false
    }

    pub fn is_editable(&self, node: NodeId) -> bool {
        let doc = &self.interp.doc;
        match doc.tag(node) {
            Some("textarea") => true,
            Some("input") => !matches!(
                doc.attr(node, "type").unwrap_or("text").to_ascii_lowercase().as_str(),
                "checkbox" | "radio" | "submit" | "button" | "reset" | "image" | "file" | "hidden" | "range" | "color"
            ),
            _ => false,
        }
    }

    /// Tıklama: odak, olaylar ve varsayılan eylem.
    pub fn click(&mut self, x: f32, y: f32) -> Action {
        let Some(node) = self.hit(x, y) else {
            self.interp.doc.focus = None;
            return Action::Redraw;
        };
        let target = if matches!(self.interp.doc.nodes[node].kind, Kind::Text(_)) {
            self.interp.doc.nodes[node].parent.unwrap_or(node)
        } else {
            node
        };
        // Etiket tıklaması ilişkili denetime gider.
        let target = if self.interp.doc.is(target, "label") {
            self.interp
                .doc
                .attr(target, "for")
                .and_then(|id| self.interp.doc.by_id(id))
                .unwrap_or(target)
        } else {
            target
        };
        let old_focus = self.interp.doc.focus;
        self.interp.doc.focus = if self.is_editable(target) || self.interp.doc.is(target, "select") {
            Some(target)
        } else {
            None
        };
        if old_focus != self.interp.doc.focus {
            self.interp.doc.dirty = true;
        }
        if self.interp.doc.is(target, "select") {
            self.cycle_select(target);
        }
        if self.javascript {
            for kind in ["mousedown", "mouseup"] {
                let ev = js::dom::make_event(&mut self.interp, kind, target, &[("clientX", Value::Num(x as f64)), ("clientY", Value::Num(y as f64))]);
                let _ = js::dom::dispatch_event(&mut self.interp, target, kind, ev);
            }
            if let Err(e) = js::dom::activate(&mut self.interp, target) {
                let t = self.interp.error_text(&e);
                self.interp.host.console.push((2, t));
            }
            self.interp.run_microtasks();
            self.after_js();
        } else {
            self.activate_without_js(target);
        }
        let action = self.take_action();
        if matches!(action, Action::None) {
            Action::Redraw
        } else {
            action
        }
    }

    /// Açılır listede sıradaki seçeneğe geçer (basit seçim arayüzü).
    fn cycle_select(&mut self, node: NodeId) {
        let doc = &mut self.interp.doc;
        let options: Vec<NodeId> = doc.descendants(node).into_iter().filter(|&n| doc.is(n, "option")).collect();
        if options.is_empty() {
            return;
        }
        let current = options.iter().position(|&o| doc.attr(o, "selected").is_some()).unwrap_or(0);
        let next = (current + 1) % options.len();
        for (i, &o) in options.iter().enumerate() {
            if i == next {
                doc.set_attr(o, "selected", "");
            } else {
                doc.remove_attr(o, "selected");
            }
        }
        if self.javascript {
            let ev = js::dom::make_event(&mut self.interp, "change", node, &[]);
            let _ = js::dom::dispatch_event(&mut self.interp, node, "change", ev);
        }
    }

    fn activate_without_js(&mut self, node: NodeId) {
        let mut cur = Some(node);
        while let Some(n) = cur {
            let doc = &self.interp.doc;
            if doc.is(n, "a")
                && let Some(href) = doc.attr(n, "href")
            {
                self.interp.host.navigate = Some(url::resolve(&self.interp.url, href));
                return;
            }
            if doc.is(n, "input") && matches!(doc.attr(n, "type"), Some("checkbox")) {
                if doc.attr(n, "checked").is_some() {
                    self.interp.doc.remove_attr(n, "checked");
                } else {
                    self.interp.doc.set_attr(n, "checked", "");
                }
                return;
            }
            let submit = (doc.is(n, "button") && doc.attr(n, "type").is_none_or(|t| t == "submit"))
                || (doc.is(n, "input") && doc.attr(n, "type") == Some("submit"));
            if submit {
                let mut f = doc.nodes[n].parent;
                while let Some(p) = f {
                    if self.interp.doc.is(p, "form") {
                        js::dom::submit_form(&mut self.interp, p, Some(n));
                        return;
                    }
                    f = self.interp.doc.nodes[p].parent;
                }
            }
            cur = self.interp.doc.nodes[n].parent;
        }
    }

    /// Odaktaki metin kutusuna yazma. `key`: karakter, ya da '\u{8}' (geri sil), '\n' (Enter).
    pub fn type_key(&mut self, ch: char, key_name: &str) -> Action {
        let Some(node) = self.interp.doc.focus else { return Action::None };
        if !self.is_editable(node) {
            return Action::None;
        }
        if self.javascript {
            let ev = js::dom::make_event(&mut self.interp, "keydown", node, &[("key", Value::str(key_name)), ("code", Value::str(key_name))]);
            let prevented = js::dom::dispatch_event(&mut self.interp, node, "keydown", ev).unwrap_or(false);
            if prevented {
                self.after_js();
                return self.take_action_or_redraw();
            }
        }
        let is_textarea = self.interp.doc.is(node, "textarea");
        let mut value = js::dom::control_value(&self.interp, node);
        match ch {
            '\u{8}' => {
                value.pop();
            }
            '\n' if !is_textarea => {
                // Enter: formu gönder.
                let mut f = self.interp.doc.nodes[node].parent;
                while let Some(p) = f {
                    if self.interp.doc.is(p, "form") {
                        let mut prevented = false;
                        if self.javascript {
                            let ev = js::dom::make_event(&mut self.interp, "submit", p, &[]);
                            prevented = js::dom::dispatch_event(&mut self.interp, p, "submit", ev).unwrap_or(false);
                            self.after_js();
                        }
                        if !prevented {
                            let submitter = js::dom::default_submitter(&self.interp, p);
                            js::dom::submit_form(&mut self.interp, p, submitter);
                        }
                        break;
                    }
                    f = self.interp.doc.nodes[p].parent;
                }
                return self.take_action_or_redraw();
            }
            c => value.push(c),
        }
        js::dom::set_control_value(&mut self.interp, node, &value);
        if self.javascript {
            for kind in ["input", "keyup"] {
                let ev = js::dom::make_event(&mut self.interp, kind, node, &[("key", Value::str(key_name)), ("data", Value::str(&alloc::format!("{}", ch)))]);
                let _ = js::dom::dispatch_event(&mut self.interp, node, kind, ev);
            }
            self.after_js();
        }
        self.needs_layout = true;
        self.take_action_or_redraw()
    }

    fn take_action_or_redraw(&mut self) -> Action {
        match self.take_action() {
            Action::None => Action::Redraw,
            a => a,
        }
    }

    pub fn focused_rect(&self) -> Option<RectF> {
        self.interp.doc.focus.and_then(|n| self.layout.rect_of(n))
    }

    /// Konsol iletileri (geliştirici paneli).
    pub fn take_console(&mut self) -> Vec<(u8, String)> {
        core::mem::take(&mut self.interp.host.console)
    }

    pub fn take_alerts(&mut self) -> Vec<String> {
        core::mem::take(&mut self.interp.host.alerts)
    }

    /// Kaydırma isteği (window.scrollTo) — sayfa başına döner.
    pub fn take_scroll_request(&mut self) -> bool {
        let before = self.interp.host.console.len();
        self.interp.host.console.retain(|(l, _)| *l != 9);
        before != self.interp.host.console.len()
    }

    /// Adres parçasına (#bölüm) kaydırır.
    pub fn scroll_to_fragment(&mut self) {
        let Some(frag) = url::Url::parse(&self.interp.url).and_then(|u| u.fragment) else { return };
        let doc = &self.interp.doc;
        let target = doc.by_id(&frag).or_else(|| {
            doc.descendants(ROOT)
                .into_iter()
                .find(|&n| doc.is(n, "a") && doc.attr(n, "name") == Some(frag.as_str()))
        });
        if let Some(n) = target
            && let Some(r) = self.layout.rect_of(n)
        {
            self.scroll = r.y.min((self.layout.height - self.viewport.1).max(0.0));
        }
    }
}
