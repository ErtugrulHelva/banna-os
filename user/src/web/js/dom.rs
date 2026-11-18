//! JavaScript ↔ DOM bağlantısı: `document`, öğeler (özellikler, yöntemler), `style`,
//! `classList`, `dataset`, `localStorage`, `location`, olay dinleyicileri ve olay yayma.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Interp, Kind, Listener, ObjId, R, Target, Value, throw};
use crate::web::dom::{Kind as NodeKind, NodeId, ROOT};
use crate::web::{css, html};

const NODE_METHODS: &[&str] = &[
    "getAttribute", "setAttribute", "removeAttribute", "hasAttribute", "toggleAttribute",
    "appendChild", "removeChild", "insertBefore", "replaceChild", "remove", "append", "prepend",
    "before", "after", "replaceWith", "querySelector", "querySelectorAll", "getElementsByTagName",
    "getElementsByClassName", "getElementById", "addEventListener", "removeEventListener",
    "dispatchEvent", "click", "focus", "blur", "closest", "matches", "contains", "cloneNode",
    "getBoundingClientRect", "scrollIntoView", "insertAdjacentHTML", "insertAdjacentElement",
    "insertAdjacentText", "createElement", "createTextNode", "createDocumentFragment",
    "createComment", "hasChildNodes", "getClientRects", "scrollTo", "attachShadow", "animate",
    "setPointerCapture", "releasePointerCapture", "hasFocus", "createEvent", "createElementNS",
    "submit", "reset", "select", "showModal", "show", "requestSubmit", "checkValidity",
    "reportValidity", "setSelectionRange", "getAttributeNames", "isEqualNode", "normalize",
];

pub fn install_node_proto(it: &mut Interp, proto: ObjId) {
    for name in NODE_METHODS {
        let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.{}", name).into_boxed_str());
        let f = it.native(n);
        it.set_hidden(proto, name, f);
    }
}

pub fn install_globals(it: &mut Interp) {
    let g = it.global;
    let document = node_value(it, ROOT);
    it.set_hidden(g, "document", document);
    let ls = it.alloc(Some(it.protos.object), Kind::Storage(false));
    let ss = it.alloc(Some(it.protos.object), Kind::Storage(true));
    for o in [ls, ss] {
        for name in ["getItem", "setItem", "removeItem", "clear", "key"] {
            let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.storage.{}", name).into_boxed_str());
            let f = it.native(n);
            it.set_hidden(o, name, f);
        }
    }
    it.set_hidden(g, "localStorage", Value::Obj(ls));
    it.set_hidden(g, "sessionStorage", Value::Obj(ss));
    let navigator = it.new_object();
    it.set_prop(navigator, "userAgent", Value::str("Mozilla/5.0 (BannaOS; x86_64) BannaSearch/1.0 Gecko/20100101 Firefox/128.0"));
    it.set_prop(navigator, "language", Value::str("tr-TR"));
    let langs = it.new_array(alloc::vec![Value::str("tr-TR"), Value::str("tr"), Value::str("en")]);
    it.set_prop(navigator, "languages", langs);
    it.set_prop(navigator, "platform", Value::str("BannaOS x86_64"));
    it.set_prop(navigator, "vendor", Value::str("Banna"));
    it.set_prop(navigator, "onLine", Value::Bool(true));
    it.set_prop(navigator, "cookieEnabled", Value::Bool(true));
    it.set_prop(navigator, "hardwareConcurrency", Value::Num(4.0));
    let clipboard = it.new_object();
    let write = it.native("dom.clipboard.writeText");
    it.set_prop(clipboard, "writeText", write);
    it.set_prop(navigator, "clipboard", Value::Obj(clipboard));
    it.set_hidden(g, "navigator", Value::Obj(navigator));
    let location = it.new_object();
    for name in ["reload", "assign", "replace", "toString"] {
        let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.location.{}", name).into_boxed_str());
        let f = it.native(n);
        it.set_hidden(location, name, f);
    }
    it.set_hidden(g, "location", Value::Obj(location));
    refresh_location(it);
    let history = it.new_object();
    for name in ["back", "forward", "go", "pushState", "replaceState"] {
        let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.history.{}", name).into_boxed_str());
        let f = it.native(n);
        it.set_hidden(history, name, f);
    }
    it.set_prop(history, "length", Value::Num(1.0));
    it.set_prop(history, "state", Value::Null);
    it.set_hidden(g, "history", Value::Obj(history));
    let screen = it.new_object();
    it.set_hidden(g, "screen", Value::Obj(screen));
    for name in ["IntersectionObserver", "MutationObserver", "ResizeObserver", "PerformanceObserver"] {
        let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.observer.{}", name).into_boxed_str());
        let f = it.native(n);
        it.set_hidden(g, name, f);
    }
    for name in ["HTMLElement", "Element", "Node", "HTMLDivElement", "HTMLInputElement", "HTMLAnchorElement", "HTMLButtonElement", "HTMLFormElement", "HTMLImageElement", "Text", "DocumentFragment", "EventTarget", "SVGElement", "HTMLCanvasElement", "HTMLTemplateElement"] {
        let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.ctor.{}", name).into_boxed_str());
        let f = it.native(n);
        if let Value::Obj(c) = f {
            let p = it.protos.node;
            it.set_hidden(c, "prototype", Value::Obj(p));
        }
        it.set_hidden(g, name, f);
    }
    let custom = it.new_object();
    for name in ["define", "get", "whenDefined"] {
        let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.customElements.{}", name).into_boxed_str());
        let f = it.native(n);
        it.set_hidden(custom, name, f);
    }
    it.set_hidden(g, "customElements", Value::Obj(custom));
    let url_ctor = it.native("dom.URL");
    if let Value::Obj(u) = url_ctor {
        let f = it.native("dom.URL.createObjectURL");
        it.set_hidden(u, "createObjectURL", f);
    }
    it.set_hidden(g, "URL", url_ctor);
    let usp = it.native("dom.URLSearchParams");
    it.set_hidden(g, "URLSearchParams", usp);
    let fd = it.native("dom.FormData");
    it.set_hidden(g, "FormData", fd);
    let te = it.native("dom.TextEncoder");
    it.set_hidden(g, "TextEncoder", te);
}

/// Geçerli adresten `location` alanlarını doldurur.
pub fn refresh_location(it: &mut Interp) {
    let g = Value::Obj(it.global);
    let Ok(location) = it.get(&g, "location") else { return };
    let Value::Obj(l) = location else { return };
    let url = it.url.clone();
    let parts = crate::web::url::Url::parse(&url);
    it.set_prop(l, "href", Value::str(&url));
    if let Some(p) = parts {
        it.set_prop(l, "protocol", Value::str(&alloc::format!("{}:", p.scheme)));
        it.set_prop(l, "host", Value::str(&p.host_port()));
        it.set_prop(l, "hostname", Value::str(&p.host));
        it.set_prop(l, "port", Value::str(&p.port.map(|n| alloc::format!("{}", n)).unwrap_or_default()));
        it.set_prop(l, "pathname", Value::str(&p.path));
        it.set_prop(l, "search", Value::str(&p.query.as_ref().map(|q| alloc::format!("?{}", q)).unwrap_or_default()));
        it.set_prop(l, "hash", Value::str(&p.fragment.as_ref().map(|f| alloc::format!("#{}", f)).unwrap_or_default()));
        it.set_prop(l, "origin", Value::str(&p.origin()));
    }
}

/// Düğümün JavaScript nesnesi (her düğüm için tek, kimlik korunur).
pub fn node_value(it: &mut Interp, node: NodeId) -> Value {
    if let Some(&o) = it.node_objs.get(&node) {
        return Value::Obj(o);
    }
    let p = it.protos.node;
    let o = it.alloc(Some(p), Kind::Node(node));
    it.node_objs.insert(node, o);
    Value::Obj(o)
}

fn node_or_null(it: &mut Interp, node: Option<NodeId>) -> Value {
    match node {
        Some(n) => node_value(it, n),
        None => Value::Null,
    }
}

fn node_list(it: &mut Interp, nodes: Vec<NodeId>) -> Value {
    let items = nodes.into_iter().map(|n| node_value(it, n)).collect();
    let arr = it.new_array(items);
    if let Value::Obj(a) = arr {
        let item = it.native("dom.list.item");
        it.set_hidden(a, "item", item);
    }
    arr
}

pub fn node_of(it: &Interp, v: &Value) -> Option<NodeId> {
    match v {
        Value::Obj(o) => match it.obj(*o).kind {
            Kind::Node(n) => Some(n),
            _ => None,
        },
        _ => None,
    }
}

fn camel_to_kebab(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if out.starts_with("webkit-") || out.starts_with("moz-") || out.starts_with("ms-") {
        out.insert(0, '-');
    }
    out
}

fn style_map(it: &Interp, node: NodeId) -> Vec<(String, String)> {
    it.doc
        .attr(node, "style")
        .map(css::parse_declarations)
        .unwrap_or_default()
        .into_iter()
        .map(|d| (d.name, d.value))
        .collect()
}

fn write_style(it: &mut Interp, node: NodeId, map: &[(String, String)]) {
    let text: Vec<String> = map.iter().map(|(k, v)| alloc::format!("{}: {}", k, v)).collect();
    it.doc.set_attr(node, "style", &text.join("; "));
}

fn rect(it: &Interp, node: NodeId) -> crate::web::layout::RectF {
    it.rects
        .iter()
        .find(|(n, _)| *n == node)
        .map(|(_, r)| *r)
        .unwrap_or_default()
}

fn rect_object(it: &mut Interp, node: NodeId) -> Value {
    let r = rect(it, node);
    let o = it.new_object();
    for (k, v) in [
        ("x", r.x),
        ("y", r.y),
        ("left", r.x),
        ("top", r.y),
        ("width", r.w),
        ("height", r.h),
        ("right", r.x + r.w),
        ("bottom", r.y + r.h),
    ] {
        it.set_prop(o, k, Value::Num(v as f64));
    }
    Value::Obj(o)
}

/// Form denetiminin değeri (yazılan metin `value` özniteliğinde tutulur).
pub fn control_value(it: &Interp, node: NodeId) -> String {
    match it.doc.tag(node) {
        Some("textarea") => it
            .doc
            .attr(node, "data-banna-value")
            .map(String::from)
            .unwrap_or_else(|| it.doc.text_content(node)),
        Some("select") => {
            let options: Vec<NodeId> = it
                .doc
                .descendants(node)
                .into_iter()
                .filter(|&n| it.doc.is(n, "option"))
                .collect();
            let chosen = options
                .iter()
                .copied()
                .find(|&o| it.doc.attr(o, "selected").is_some())
                .or(options.first().copied());
            chosen
                .map(|o| {
                    it.doc
                        .attr(o, "value")
                        .map(String::from)
                        .unwrap_or_else(|| crate::web::dom::collapse(&it.doc.text_content(o)))
                })
                .unwrap_or_default()
        }
        _ => String::from(it.doc.attr(node, "value").unwrap_or("")),
    }
}

pub fn set_control_value(it: &mut Interp, node: NodeId, value: &str) {
    match it.doc.tag(node) {
        Some("textarea") => it.doc.set_attr(node, "data-banna-value", value),
        Some("select") => {
            for o in it.doc.descendants(node) {
                if it.doc.is(o, "option") {
                    let v = it
                        .doc
                        .attr(o, "value")
                        .map(String::from)
                        .unwrap_or_else(|| it.doc.text_content(o));
                    if v == value {
                        it.doc.set_attr(o, "selected", "");
                    } else {
                        it.doc.remove_attr(o, "selected");
                    }
                }
            }
        }
        _ => it.doc.set_attr(node, "value", value),
    }
}

fn inner_text(it: &Interp, node: NodeId) -> String {
    let mut out = String::new();
    for n in it.doc.descendants(node) {
        match &it.doc.nodes[n].kind {
            NodeKind::Text(t) => {
                let parent_hidden = it.doc.nodes[n]
                    .parent
                    .and_then(|p| it.doc.tag(p))
                    .is_some_and(|t| matches!(t, "script" | "style"));
                if !parent_hidden {
                    out.push_str(t);
                }
            }
            NodeKind::Element { tag, .. } if matches!(tag.as_str(), "br" | "p" | "div" | "li") && !out.is_empty() => {
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

/// Özellik okuma: `None` ise sıradan nesne özelliklerine düşülür.
pub fn get(it: &mut Interp, o: ObjId, key: &str) -> R<Option<Value>> {
    match it.obj(o).kind {
        Kind::Storage(session) => {
            let prefix = if session { "s:" } else { "l:" };
            let k = alloc::format!("{}{}", prefix, key);
            if key == "length" {
                let n = it.storage.iter().filter(|(k, _)| k.starts_with(prefix)).count();
                return Ok(Some(Value::Num(n as f64)));
            }
            if it.own(o, key).is_some() {
                return Ok(None);
            }
            return Ok(it.storage.iter().find(|(sk, _)| *sk == k).map(|(_, v)| Value::str(v)));
        }
        Kind::Style(node) => {
            if key == "cssText" {
                return Ok(Some(Value::str(it.doc.attr(node, "style").unwrap_or(""))));
            }
            if matches!(key, "setProperty" | "removeProperty" | "getPropertyValue") {
                return Ok(None);
            }
            let prop = camel_to_kebab(key);
            let v = style_map(it, node)
                .into_iter()
                .find(|(k, _)| *k == prop)
                .map(|(_, v)| v)
                .unwrap_or_default();
            return Ok(Some(Value::str(&v)));
        }
        Kind::ClassList(node) => {
            return Ok(match key {
                "length" => Some(Value::Num(it.doc.classes(node).count() as f64)),
                "value" => Some(Value::str(it.doc.attr(node, "class").unwrap_or(""))),
                _ => match super::index(key) {
                    Some(i) => Some(it.doc.classes(node).nth(i).map(Value::str).unwrap_or_default()),
                    None => None,
                },
            });
        }
        Kind::Dataset(node) => {
            let attr = alloc::format!("data-{}", camel_to_kebab(key));
            return Ok(it.doc.attr(node, &attr).map(Value::str));
        }
        _ => {}
    }
    let Kind::Node(node) = it.obj(o).kind else { return Ok(None) };
    if it.own(o, key).is_some() {
        return Ok(None);
    }
    let doc = &it.doc;
    let is_document = node == ROOT;
    let v = match key {
        "nodeType" => Value::Num(match &doc.nodes[node].kind {
            NodeKind::Document => 9.0,
            NodeKind::Element { .. } => 1.0,
            NodeKind::Text(_) => 3.0,
            NodeKind::Comment => 8.0,
        }),
        "nodeName" | "tagName" | "localName" => match doc.tag(node) {
            Some(t) if key == "localName" => Value::str(t),
            Some(t) => Value::str(&t.to_ascii_uppercase()),
            None if is_document => Value::str("#document"),
            None => Value::str("#text"),
        },
        "body" if is_document => {
            let b = doc.body();
            node_or_null(it, b)
        }
        "head" if is_document => {
            let h = doc.find_tag("head");
            node_or_null(it, h)
        }
        "documentElement" if is_document => {
            let h = doc.find_tag("html");
            node_or_null(it, h)
        }
        "title" if is_document => Value::str(&doc.title()),
        "readyState" if is_document => Value::str("complete"),
        "cookie" if is_document => Value::str(&crate::web::cookies::header_for(&it.url, true)),
        "URL" | "documentURI" if is_document => Value::str(&it.url.clone()),
        "domain" if is_document => Value::str(&crate::web::url::Url::parse(&it.url).map(|u| u.host).unwrap_or_default()),
        "activeElement" if is_document => {
            let f = doc.focus.or(doc.body());
            node_or_null(it, f)
        }
        "defaultView" if is_document => Value::Obj(it.global),
        "visibilityState" if is_document => Value::str("visible"),
        "hidden" if is_document => Value::Bool(false),
        "characterSet" | "charset" if is_document => Value::str("UTF-8"),
        "forms" | "images" | "links" | "scripts" if is_document => {
            let tag = match key {
                "forms" => "form",
                "images" => "img",
                "links" => "a",
                _ => "script",
            };
            let list: Vec<NodeId> = doc.descendants(ROOT).into_iter().filter(|&n| doc.is(n, tag)).collect();
            node_list(it, list)
        }
        "ownerDocument" => node_value(it, ROOT),
        "isConnected" => Value::Bool(doc.connected(node)),
        "parentNode" | "parentElement" => {
            let p = doc.nodes[node].parent.filter(|&p| key == "parentNode" || p != ROOT);
            node_or_null(it, p)
        }
        "childNodes" => {
            let c = doc.nodes[node].children.clone();
            node_list(it, c)
        }
        "children" => {
            let c = doc.element_children(node);
            node_list(it, c)
        }
        "childElementCount" => Value::Num(doc.element_children(node).len() as f64),
        "firstChild" => {
            let c = doc.nodes[node].children.first().copied();
            node_or_null(it, c)
        }
        "lastChild" => {
            let c = doc.nodes[node].children.last().copied();
            node_or_null(it, c)
        }
        "firstElementChild" => {
            let c = doc.element_children(node).first().copied();
            node_or_null(it, c)
        }
        "lastElementChild" => {
            let c = doc.element_children(node).last().copied();
            node_or_null(it, c)
        }
        "nextSibling" | "previousSibling" | "nextElementSibling" | "previousElementSibling" => {
            let sibling = doc.nodes[node].parent.and_then(|p| {
                let list = if key.contains("Element") {
                    doc.element_children(p)
                } else {
                    doc.nodes[p].children.clone()
                };
                let i = list.iter().position(|&c| c == node)?;
                if key.starts_with("next") {
                    list.get(i + 1).copied()
                } else {
                    i.checked_sub(1).map(|j| list[j])
                }
            });
            node_or_null(it, sibling)
        }
        "textContent" | "nodeValue" | "data" => match &doc.nodes[node].kind {
            NodeKind::Text(t) => Value::str(t),
            _ if key == "textContent" && !is_document => Value::str(&doc.text_content(node)),
            _ => Value::Null,
        },
        "innerText" | "outerText" => Value::str(&inner_text(it, node)),
        "innerHTML" | "outerHTML" => {
            let mut s = String::new();
            doc.serialize(node, key == "outerHTML", &mut s);
            Value::str(&s)
        }
        "id" | "className" | "href" | "src" | "name" | "type" | "alt" | "title" | "rel" | "target" | "placeholder" | "action" | "method" | "lang" | "dir" | "role" | "htmlFor" | "accept" | "download" | "pattern" => {
            if node == ROOT {
                return Ok(None);
            }
            let attr = match key {
                "className" => "class",
                "htmlFor" => "for",
                k => k,
            };
            let raw = doc.attr(node, attr).unwrap_or("");
            let value = if matches!(key, "href" | "src" | "action") && !raw.is_empty() {
                crate::web::url::resolve(&it.url, raw)
            } else if key == "type" && raw.is_empty() && doc.is(node, "input") {
                String::from("text")
            } else {
                String::from(raw)
            };
            Value::str(&value)
        }
        "value" => Value::str(&control_value(it, node)),
        "checked" | "disabled" | "selected" | "hidden" | "required" | "readOnly" | "multiple" | "open" | "autofocus" => {
            let attr = if key == "readOnly" { "readonly" } else { key };
            Value::Bool(doc.attr(node, attr).is_some())
        }
        "selectedIndex" => {
            let options: Vec<NodeId> = doc.descendants(node).into_iter().filter(|&n| doc.is(n, "option")).collect();
            Value::Num(options.iter().position(|&o| doc.attr(o, "selected").is_some()).unwrap_or(0) as f64)
        }
        "options" => {
            let options: Vec<NodeId> = doc.descendants(node).into_iter().filter(|&n| doc.is(n, "option")).collect();
            node_list(it, options)
        }
        "form" => {
            let mut p = doc.nodes[node].parent;
            while let Some(n) = p {
                if doc.is(n, "form") {
                    break;
                }
                p = doc.nodes[n].parent;
            }
            node_or_null(it, p)
        }
        "elements" => {
            let list: Vec<NodeId> = doc
                .descendants(node)
                .into_iter()
                .filter(|&n| matches!(doc.tag(n), Some("input" | "select" | "textarea" | "button")))
                .collect();
            node_list(it, list)
        }
        "style" => {
            let p = it.protos.object;
            let s = it.alloc(Some(p), Kind::Style(node));
            for name in ["setProperty", "removeProperty", "getPropertyValue"] {
                let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.style.{}", name).into_boxed_str());
                let f = it.native_with(n, Value::Num(node as f64));
                it.set_hidden(s, name, f);
            }
            Value::Obj(s)
        }
        "classList" => {
            let p = it.protos.object;
            let c = it.alloc(Some(p), Kind::ClassList(node));
            for name in ["add", "remove", "toggle", "contains", "replace", "item", "forEach"] {
                let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.classList.{}", name).into_boxed_str());
                let f = it.native_with(n, Value::Num(node as f64));
                it.set_hidden(c, name, f);
            }
            Value::Obj(c)
        }
        "dataset" => {
            let p = it.protos.object;
            Value::Obj(it.alloc(Some(p), Kind::Dataset(node)))
        }
        "attributes" => {
            let attrs = match &doc.nodes[node].kind {
                NodeKind::Element { attrs, .. } => attrs.clone(),
                _ => Vec::new(),
            };
            let mut items = Vec::new();
            for (k, v) in attrs {
                let a = it.new_object();
                it.set_prop(a, "name", Value::str(&k));
                it.set_prop(a, "value", Value::str(&v));
                items.push(Value::Obj(a));
            }
            it.new_array(items)
        }
        "offsetWidth" | "clientWidth" | "scrollWidth" => Value::Num(rect(it, node).w as f64),
        "offsetHeight" | "clientHeight" | "scrollHeight" => Value::Num(rect(it, node).h as f64),
        "offsetLeft" => Value::Num(rect(it, node).x as f64),
        "offsetTop" => Value::Num(rect(it, node).y as f64),
        "offsetParent" => {
            let b = doc.body();
            node_or_null(it, b)
        }
        "scrollTop" | "scrollLeft" => Value::Num(0.0),
        "content" if doc.is(node, "template") => node_value(it, node),
        "shadowRoot" => Value::Null,
        "naturalWidth" | "naturalHeight" | "width" | "height" if doc.is(node, "img") => {
            let r = rect(it, node);
            Value::Num(if key.contains("idth") { r.w } else { r.h } as f64)
        }
        "files" => it.new_array(Vec::new()),
        "validity" => {
            let v = it.new_object();
            it.set_prop(v, "valid", Value::Bool(true));
            Value::Obj(v)
        }
        _ => {
            // on<olay> özellikleri öznitelikten okunmaz; diğerleri yok.
            return Ok(None);
        }
    };
    Ok(Some(v))
}

/// Özellik yazma: işlendiyse `true`.
pub fn set(it: &mut Interp, o: ObjId, key: &str, value: Value) -> R<bool> {
    match it.obj(o).kind {
        Kind::Storage(session) => {
            let k = alloc::format!("{}{}", if session { "s:" } else { "l:" }, key);
            let v = it.string(&value);
            store(it, k, v, session);
            return Ok(true);
        }
        Kind::Style(node) => {
            let v = it.string(&value);
            if key == "cssText" {
                it.doc.set_attr(node, "style", &v);
                return Ok(true);
            }
            let prop = camel_to_kebab(key);
            let mut map = style_map(it, node);
            map.retain(|(k, _)| *k != prop);
            if !v.is_empty() {
                map.push((prop, v));
            }
            write_style(it, node, &map);
            return Ok(true);
        }
        Kind::Dataset(node) => {
            let v = it.string(&value);
            it.doc.set_attr(node, &alloc::format!("data-{}", camel_to_kebab(key)), &v);
            return Ok(true);
        }
        Kind::ClassList(_) => return Ok(true),
        _ => {}
    }
    let Kind::Node(node) = it.obj(o).kind else { return Ok(false) };
    match key {
        "textContent" | "innerText" | "nodeValue" | "data" => {
            let v = match value {
                Value::Null | Value::Undefined => String::new(),
                v => it.string(&v),
            };
            it.doc.set_text_content(node, &v);
        }
        "innerHTML" => {
            let v = it.string(&value);
            it.doc.clear_children(node);
            html::parse_into(&mut it.doc, node, &v, false);
            run_inserted_scripts(it, node);
        }
        "outerHTML" => {
            let v = it.string(&value);
            if let Some(parent) = it.doc.nodes[node].parent {
                let holder = it.doc.element("div");
                html::parse_into(&mut it.doc, holder, &v, false);
                for c in it.doc.nodes[holder].children.clone() {
                    it.doc.insert_before(parent, c, Some(node));
                }
                it.doc.detach(node);
            }
        }
        "title" if node == ROOT => {
            let v = it.string(&value);
            if let Some(t) = it.doc.find_tag("title") {
                it.doc.set_text_content(t, &v);
            } else if let Some(head) = it.doc.find_tag("head") {
                let t = it.doc.element("title");
                it.doc.append(head, t);
                it.doc.set_text_content(t, &v);
            }
            it.host.title_changed = true;
        }
        "cookie" if node == ROOT => {
            let v = it.string(&value);
            let url = it.url.clone();
            crate::web::cookies::store(&url, &v, true);
        }
        "id" | "className" | "href" | "src" | "name" | "type" | "alt" | "title" | "rel" | "target" | "placeholder" | "action" | "method" | "lang" | "dir" | "role" | "htmlFor" | "download" => {
            let attr = match key {
                "className" => "class",
                "htmlFor" => "for",
                k => k,
            };
            let v = it.string(&value);
            it.doc.set_attr(node, attr, &v);
        }
        "value" => {
            let v = it.string(&value);
            set_control_value(it, node, &v);
        }
        "checked" | "disabled" | "selected" | "hidden" | "required" | "readOnly" | "multiple" | "open" => {
            let attr = if key == "readOnly" { "readonly" } else { key };
            if it.truthy(&value) {
                if key == "checked" && it.doc.attr(node, "type") == Some("radio") {
                    uncheck_radio_group(it, node);
                }
                it.doc.set_attr(node, attr, "");
            } else {
                it.doc.remove_attr(node, attr);
            }
        }
        "selectedIndex" => {
            let i = it.to_number(&value)? as usize;
            let options: Vec<NodeId> = it.doc.descendants(node).into_iter().filter(|&n| it.doc.is(n, "option")).collect();
            for (k, o) in options.iter().enumerate() {
                if k == i {
                    it.doc.set_attr(*o, "selected", "");
                } else {
                    it.doc.remove_attr(*o, "selected");
                }
            }
        }
        "scrollTop" | "scrollLeft" => {}
        _ => return Ok(false),
    }
    Ok(true)
}

fn uncheck_radio_group(it: &mut Interp, node: NodeId) {
    let Some(name) = it.doc.attr(node, "name").map(String::from) else { return };
    for n in it.doc.descendants(ROOT) {
        if n != node && it.doc.is(n, "input") && it.doc.attr(n, "type") == Some("radio") && it.doc.attr(n, "name") == Some(&name) {
            it.doc.remove_attr(n, "checked");
        }
    }
}

fn store(it: &mut Interp, key: String, value: String, session: bool) {
    match it.storage.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => it.storage.push((key, value)),
    }
    if !session {
        it.storage_dirty = true;
    }
}

pub fn delete(it: &mut Interp, o: ObjId, key: &str) -> R<()> {
    match it.obj(o).kind {
        Kind::Storage(session) => {
            let k = alloc::format!("{}{}", if session { "s:" } else { "l:" }, key);
            it.storage.retain(|(sk, _)| *sk != k);
            it.storage_dirty = true;
        }
        Kind::Dataset(node) => it.doc.remove_attr(node, &alloc::format!("data-{}", camel_to_kebab(key))),
        _ => {}
    }
    Ok(())
}

/// innerHTML ile eklenen betikler çalışmaz (tarayıcı davranışı); `appendChild` ile
/// eklenen `<script>` öğeleri ise çalıştırılır.
fn run_inserted_scripts(_it: &mut Interp, _node: NodeId) {}

fn run_script_element(it: &mut Interp, node: NodeId) {
    if !it.doc.is(node, "script") || it.doc.attr(node, "data-banna-ran").is_some() {
        return;
    }
    it.doc.set_attr(node, "data-banna-ran", "1");
    let kind = it.doc.attr(node, "type").unwrap_or("").to_ascii_lowercase();
    if !(kind.is_empty() || kind.contains("javascript")) {
        return;
    }
    let src = match it.doc.attr(node, "src") {
        Some(src) => {
            let url = crate::web::url::resolve(&it.url, src);
            match (it.fetch)(&url, "GET", &[], "") {
                Ok((200, text, _)) => text,
                _ => return,
            }
        }
        None => it.doc.text_content(node),
    };
    let saved = it.steps;
    it.run_script(&src);
    it.steps = saved;
    // onload
    let v = node_value(it, node);
    if let Ok(f) = it.get(&v, "onload")
        && it.is_callable(&f)
    {
        let _ = it.call(&f, v, Vec::new());
    }
}

fn insert(it: &mut Interp, parent: NodeId, child: NodeId, before: Option<NodeId>) {
    // Belge parçası: çocukları taşınır.
    if it.doc.attr(child, "data-banna-fragment").is_some() {
        for c in it.doc.nodes[child].children.clone() {
            it.doc.insert_before(parent, c, before);
            if it.doc.connected(c) {
                for d in it.doc.descendants(c) {
                    run_script_element(it, d);
                }
            }
        }
        return;
    }
    it.doc.insert_before(parent, child, before);
    if it.doc.connected(child) {
        for d in it.doc.descendants(child) {
            run_script_element(it, d);
        }
    }
}

fn to_node(it: &mut Interp, v: &Value) -> R<NodeId> {
    match node_of(it, v) {
        Some(n) => Ok(n),
        None => {
            // Dize → metin düğümü (append/prepend).
            let s = it.string(v);
            Ok(it.doc.text(&s))
        }
    }
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

fn data_node(data: &Value) -> NodeId {
    match data {
        Value::Num(n) => *n as NodeId,
        _ => ROOT,
    }
}

pub fn call(it: &mut Interp, name: &str, data: Value, this: Value, args: Vec<Value>) -> R<Value> {
    let a0 = arg(&args, 0);
    // Yardımcı nesnelerin yöntemleri.
    if let Some(m) = name.strip_prefix("classList.") {
        let node = data_node(&data);
        let mut classes: Vec<String> = it.doc.classes(node).map(String::from).collect();
        let result = match m {
            "add" => {
                for a in &args {
                    let c = it.string(a);
                    if !classes.contains(&c) {
                        classes.push(c);
                    }
                }
                Value::Undefined
            }
            "remove" => {
                for a in &args {
                    let c = it.string(a);
                    classes.retain(|x| *x != c);
                }
                Value::Undefined
            }
            "toggle" => {
                let c = it.string(&a0);
                let force = match arg(&args, 1) {
                    Value::Undefined => None,
                    v => Some(it.truthy(&v)),
                };
                let has = classes.contains(&c);
                let want = force.unwrap_or(!has);
                classes.retain(|x| *x != c);
                if want {
                    classes.push(c);
                }
                Value::Bool(want)
            }
            "contains" => {
                let c = it.string(&a0);
                return Ok(Value::Bool(classes.contains(&c)));
            }
            "replace" => {
                let old = it.string(&a0);
                let new = it.string(&arg(&args, 1));
                let found = classes.iter_mut().find(|x| **x == old).map(|x| *x = new).is_some();
                Value::Bool(found)
            }
            "item" => {
                let i = it.to_number(&a0)? as usize;
                return Ok(classes.get(i).map(|c| Value::str(c)).unwrap_or(Value::Null));
            }
            "forEach" => {
                for (i, c) in classes.clone().iter().enumerate() {
                    it.call(&a0, Value::Undefined, alloc::vec![Value::str(c), Value::Num(i as f64)])?;
                }
                return Ok(Value::Undefined);
            }
            _ => Value::Undefined,
        };
        it.doc.set_attr(node, "class", &classes.join(" "));
        return Ok(result);
    }
    if let Some(m) = name.strip_prefix("style.") {
        let node = data_node(&data);
        let prop = it.string(&a0);
        let mut map = style_map(it, node);
        return Ok(match m {
            "setProperty" => {
                let v = it.string(&arg(&args, 1));
                map.retain(|(k, _)| *k != prop);
                if !v.is_empty() {
                    map.push((prop, v));
                }
                write_style(it, node, &map);
                Value::Undefined
            }
            "removeProperty" => {
                map.retain(|(k, _)| *k != prop);
                write_style(it, node, &map);
                Value::str("")
            }
            _ => Value::str(&map.into_iter().find(|(k, _)| *k == prop).map(|(_, v)| v).unwrap_or_default()),
        });
    }
    if let Some(m) = name.strip_prefix("storage.") {
        let session = match &this {
            Value::Obj(o) => matches!(it.obj(*o).kind, Kind::Storage(true)),
            _ => false,
        };
        let prefix = if session { "s:" } else { "l:" };
        return Ok(match m {
            "getItem" => {
                let k = alloc::format!("{}{}", prefix, it.string(&a0));
                it.storage.iter().find(|(sk, _)| *sk == k).map(|(_, v)| Value::str(v)).unwrap_or(Value::Null)
            }
            "setItem" => {
                let k = alloc::format!("{}{}", prefix, it.string(&a0));
                let v = it.string(&arg(&args, 1));
                store(it, k, v, session);
                Value::Undefined
            }
            "removeItem" => {
                let k = alloc::format!("{}{}", prefix, it.string(&a0));
                it.storage.retain(|(sk, _)| *sk != k);
                it.storage_dirty = true;
                Value::Undefined
            }
            "clear" => {
                it.storage.retain(|(sk, _)| !sk.starts_with(prefix));
                it.storage_dirty = true;
                Value::Undefined
            }
            _ => {
                let i = it.to_number(&a0)? as usize;
                it.storage
                    .iter()
                    .filter(|(k, _)| k.starts_with(prefix))
                    .nth(i)
                    .map(|(k, _)| Value::str(&k[2..]))
                    .unwrap_or(Value::Null)
            }
        });
    }
    if let Some(m) = name.strip_prefix("location.") {
        return Ok(match m {
            "reload" => {
                it.host.navigate = Some(it.url.clone());
                Value::Undefined
            }
            "assign" | "replace" => {
                let u = it.string(&a0);
                it.host.navigate = Some(crate::web::url::resolve(&it.url, &u));
                Value::Undefined
            }
            _ => Value::str(&it.url.clone()),
        });
    }
    if let Some(m) = name.strip_prefix("history.") {
        if m == "back" || (m == "go" && it.to_number(&a0).unwrap_or(0.0) < 0.0) {
            it.host.navigate = Some(String::from("banna:back"));
        } else if (m == "pushState" || m == "replaceState")
            && let Value::Str(u) = arg(&args, 2) {
                it.url = crate::web::url::resolve(&it.url, &u);
                refresh_location(it);
            }
        return Ok(Value::Undefined);
    }
    if let Some(m) = name.strip_prefix("customElements.") {
        return Ok(match m {
            "whenDefined" => {
                let p = it.new_promise();
                it.resolve_promise(p, Value::Undefined)?;
                Value::Obj(p)
            }
            _ => Value::Undefined,
        });
    }
    if name.starts_with("observer.") {
        let o = it.new_object();
        for m in ["observe", "unobserve", "disconnect", "takeRecords"] {
            let f = it.native("dom.noop");
            it.set_prop(o, m, f);
        }
        return Ok(Value::Obj(o));
    }
    if name.starts_with("ctor.") {
        throw!(it, TypeError, "geçersiz kurucu");
    }
    match name {
        "noop" | "clipboard.writeText" | "URL.createObjectURL" => {
            if name == "clipboard.writeText" {
                let t = it.string(&a0);
                crate::gui::clipboard::set(&t);
                let p = it.new_promise();
                it.resolve_promise(p, Value::Undefined)?;
                return Ok(Value::Obj(p));
            }
            return Ok(if name == "URL.createObjectURL" { Value::str("blob:banna") } else { Value::Undefined });
        }
        "list.item" => {
            let i = it.to_number(&a0)?;
            let k = alloc::format!("{}", i as usize);
            let v = it.get(&this, &k)?;
            return Ok(if v == Value::Undefined { Value::Null } else { v });
        }
        "URL" => {
            let base = match arg(&args, 1) {
                Value::Undefined => it.url.clone(),
                v => it.string(&v),
            };
            let href = crate::web::url::resolve(&base, &it.string(&a0));
            let Some(u) = crate::web::url::Url::parse(&href) else {
                throw!(it, TypeError, "geçersiz URL");
            };
            let o = it.new_object();
            it.set_prop(o, "href", Value::str(&href));
            it.set_prop(o, "protocol", Value::str(&alloc::format!("{}:", u.scheme)));
            it.set_prop(o, "host", Value::str(&u.host_port()));
            it.set_prop(o, "hostname", Value::str(&u.host));
            it.set_prop(o, "pathname", Value::str(&u.path));
            it.set_prop(o, "search", Value::str(&u.query.as_ref().map(|q| alloc::format!("?{}", q)).unwrap_or_default()));
            it.set_prop(o, "hash", Value::str(&u.fragment.as_ref().map(|f| alloc::format!("#{}", f)).unwrap_or_default()));
            it.set_prop(o, "origin", Value::str(&u.origin()));
            let q = u.query.clone().unwrap_or_default();
            let usp = it.native("dom.URLSearchParams");
            let params = it.construct(&usp, alloc::vec![Value::str(&q)])?;
            it.set_prop(o, "searchParams", params);
            let ts = it.native_with("dom.url.toString", Value::str(&href));
            it.set_hidden(o, "toString", ts);
            return Ok(Value::Obj(o));
        }
        "url.toString" => return Ok(data),
        "URLSearchParams" | "FormData" => {
            let o = it.new_object();
            let mut pairs: Vec<(String, String)> = Vec::new();
            if name == "FormData" {
                if let Some(form) = node_of(it, &a0) {
                    pairs = form_pairs(it, form);
                }
            } else {
                match &a0 {
                    Value::Str(s) => {
                        for part in s.trim_start_matches('?').split('&').filter(|p| !p.is_empty()) {
                            let (k, v) = part.split_once('=').unwrap_or((part, ""));
                            pairs.push((super::builtins::uri_decode(&k.replace('+', " ")), super::builtins::uri_decode(&v.replace('+', " "))));
                        }
                    }
                    Value::Obj(src) => {
                        for k in it.own_keys(*src) {
                            let v = it.get(&a0, &k)?;
                            pairs.push((String::from(&*k), it.string(&v)));
                        }
                    }
                    _ => {}
                }
            }
            let list: Vec<Value> = pairs
                .into_iter()
                .map(|(k, v)| it.new_array(alloc::vec![Value::str(&k), Value::str(&v)]))
                .collect();
            let arr = it.new_array(list);
            it.set_hidden(o, "__pairs", arr);
            for m in ["get", "getAll", "set", "append", "has", "delete", "toString", "forEach", "entries", "keys", "values"] {
                let n: &'static str = alloc::boxed::Box::leak(alloc::format!("dom.params.{}", m).into_boxed_str());
                let f = it.native(n);
                it.set_hidden(o, m, f);
            }
            return Ok(Value::Obj(o));
        }
        "TextEncoder" => {
            let o = it.new_object();
            let f = it.native("dom.encode");
            it.set_hidden(o, "encode", f);
            return Ok(Value::Obj(o));
        }
        "encode" => {
            let s = it.string(&a0);
            let bytes = s.bytes().map(|b| Value::Num(b as f64)).collect();
            return Ok(it.new_array(bytes));
        }
        _ => {}
    }
    if let Some(m) = name.strip_prefix("params.") {
        let pairs_v = it.get(&this, "__pairs")?;
        let mut pairs = Vec::new();
        for p in it.iterate(&pairs_v)? {
            let k = it.get(&p, "0")?;
            let v = it.get(&p, "1")?;
            pairs.push((it.string(&k), it.string(&v)));
        }
        let key = it.string(&a0);
        let result = match m {
            "get" => pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| Value::str(v)).unwrap_or(Value::Null),
            "getAll" => {
                let all = pairs.iter().filter(|(k, _)| *k == key).map(|(_, v)| Value::str(v)).collect();
                it.new_array(all)
            }
            "has" => Value::Bool(pairs.iter().any(|(k, _)| *k == key)),
            "set" | "append" | "delete" => {
                if m != "append" {
                    pairs.retain(|(k, _)| *k != key);
                }
                if m != "delete" {
                    pairs.push((key, it.string(&arg(&args, 1))));
                }
                let list: Vec<Value> = pairs.iter().map(|(k, v)| it.new_array(alloc::vec![Value::str(k), Value::str(v)])).collect();
                let arr = it.new_array(list);
                it.set_hidden(match this { Value::Obj(o) => o, _ => return Ok(Value::Undefined) }, "__pairs", arr);
                Value::Undefined
            }
            "forEach" => {
                for (k, v) in pairs {
                    it.call(&a0, Value::Undefined, alloc::vec![Value::str(&v), Value::str(&k)])?;
                }
                Value::Undefined
            }
            "entries" => pairs_v,
            "keys" | "values" => {
                let list = pairs.iter().map(|(k, v)| Value::str(if m == "keys" { k } else { v })).collect();
                it.new_array(list)
            }
            _ => {
                let parts: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| alloc::format!("{}={}", encode_form(k), encode_form(v)))
                    .collect();
                Value::str(&parts.join("&"))
            }
        };
        return Ok(result);
    }

    // Öğe/belge yöntemleri.
    let Some(node) = node_of(it, &this) else {
        // window.addEventListener vb. genel yöntemler `call_global`'dadır.
        if let Some(r) = call_global(it, name, this.clone(), args.clone()) {
            return r;
        }
        throw!(it, TypeError, "{} bir DOM düğümünde çağrılmalı", name);
    };
    Ok(match name {
        "getAttribute" => {
            let k = it.string(&a0).to_ascii_lowercase();
            it.doc.attr(node, &k).map(Value::str).unwrap_or(Value::Null)
        }
        "getAttributeNames" => {
            let names = match &it.doc.nodes[node].kind {
                NodeKind::Element { attrs, .. } => attrs.iter().map(|(k, _)| Value::str(k)).collect(),
                _ => Vec::new(),
            };
            it.new_array(names)
        }
        "setAttribute" => {
            let k = it.string(&a0).to_ascii_lowercase();
            let v = it.string(&arg(&args, 1));
            if k == "value" {
                set_control_value(it, node, &v);
            } else {
                it.doc.set_attr(node, &k, &v);
            }
            Value::Undefined
        }
        "removeAttribute" => {
            let k = it.string(&a0).to_ascii_lowercase();
            it.doc.remove_attr(node, &k);
            Value::Undefined
        }
        "hasAttribute" => {
            let k = it.string(&a0).to_ascii_lowercase();
            Value::Bool(it.doc.attr(node, &k).is_some())
        }
        "toggleAttribute" => {
            let k = it.string(&a0).to_ascii_lowercase();
            let has = it.doc.attr(node, &k).is_some();
            if has {
                it.doc.remove_attr(node, &k);
            } else {
                it.doc.set_attr(node, &k, "");
            }
            Value::Bool(!has)
        }
        "appendChild" | "append" => {
            for a in &args {
                let child = to_node(it, a)?;
                insert(it, node, child, None);
            }
            if name == "appendChild" { a0 } else { Value::Undefined }
        }
        "prepend" => {
            let first = it.doc.nodes[node].children.first().copied();
            for a in &args {
                let child = to_node(it, a)?;
                insert(it, node, child, first);
            }
            Value::Undefined
        }
        "insertBefore" => {
            let child = to_node(it, &a0)?;
            let before = node_of(it, &arg(&args, 1));
            insert(it, node, child, before);
            a0
        }
        "replaceChild" => {
            let new = to_node(it, &a0)?;
            if let Some(old) = node_of(it, &arg(&args, 1)) {
                insert(it, node, new, Some(old));
                it.doc.detach(old);
            }
            arg(&args, 1)
        }
        "removeChild" => {
            if let Some(child) = node_of(it, &a0) {
                it.doc.detach(child);
            }
            a0
        }
        "remove" => {
            it.doc.detach(node);
            Value::Undefined
        }
        "before" | "after" | "replaceWith" => {
            if let Some(parent) = it.doc.nodes[node].parent {
                let siblings = it.doc.nodes[parent].children.clone();
                let i = siblings.iter().position(|&c| c == node).unwrap_or(0);
                let anchor = if name == "after" { siblings.get(i + 1).copied() } else { Some(node) };
                for a in &args {
                    let child = to_node(it, a)?;
                    insert(it, parent, child, anchor);
                }
                if name == "replaceWith" {
                    it.doc.detach(node);
                }
            }
            Value::Undefined
        }
        "insertAdjacentHTML" | "insertAdjacentElement" | "insertAdjacentText" => {
            let pos = it.string(&a0).to_ascii_lowercase();
            let holder = it.doc.element("div");
            match name {
                "insertAdjacentHTML" => {
                    let h = it.string(&arg(&args, 1));
                    html::parse_into(&mut it.doc, holder, &h, false);
                }
                "insertAdjacentText" => {
                    let t = it.string(&arg(&args, 1));
                    let n = it.doc.text(&t);
                    it.doc.append(holder, n);
                }
                _ => {
                    if let Some(n) = node_of(it, &arg(&args, 1)) {
                        it.doc.append(holder, n);
                    }
                }
            }
            let parent = it.doc.nodes[node].parent;
            let new: Vec<NodeId> = it.doc.nodes[holder].children.clone();
            for c in new {
                match pos.as_str() {
                    "beforebegin" => {
                        if let Some(p) = parent {
                            insert(it, p, c, Some(node));
                        }
                    }
                    "afterend" => {
                        if let Some(p) = parent {
                            let sib = it.doc.nodes[p].children.clone();
                            let i = sib.iter().position(|&x| x == node).unwrap_or(0);
                            insert(it, p, c, sib.get(i + 1).copied());
                        }
                    }
                    "afterbegin" => {
                        let first = it.doc.nodes[node].children.first().copied();
                        insert(it, node, c, first);
                    }
                    _ => insert(it, node, c, None),
                }
            }
            arg(&args, 1)
        }
        "querySelector" | "querySelectorAll" => {
            let sel = it.string(&a0);
            let found = css::select(&it.doc, node, &sel);
            if name == "querySelector" {
                node_or_null(it, found.first().copied())
            } else {
                node_list(it, found)
            }
        }
        "getElementsByTagName" => {
            let tag = it.string(&a0).to_ascii_lowercase();
            let list: Vec<NodeId> = it
                .doc
                .descendants(node)
                .into_iter()
                .skip(1)
                .filter(|&n| tag == "*" && it.doc.tag(n).is_some() || it.doc.is(n, &tag))
                .collect();
            node_list(it, list)
        }
        "getElementsByClassName" => {
            let wanted: Vec<String> = it.string(&a0).split_whitespace().map(String::from).collect();
            let list: Vec<NodeId> = it
                .doc
                .descendants(node)
                .into_iter()
                .skip(1)
                .filter(|&n| wanted.iter().all(|c| it.doc.has_class(n, c)))
                .collect();
            node_list(it, list)
        }
        "getElementById" => {
            let id = it.string(&a0);
            let found = it.doc.by_id(&id);
            node_or_null(it, found)
        }
        "createElement" | "createElementNS" => {
            let tag = it.string(if name == "createElementNS" { &args[1.min(args.len().saturating_sub(1))] } else { &a0 }).to_ascii_lowercase();
            let n = it.doc.element(&tag);
            node_value(it, n)
        }
        "createTextNode" | "createComment" => {
            let t = it.string(&a0);
            let n = if name == "createTextNode" { it.doc.text(&t) } else { it.doc.create(NodeKind::Comment) };
            node_value(it, n)
        }
        "createDocumentFragment" => {
            let n = it.doc.element("div");
            it.doc.set_attr(n, "data-banna-fragment", "");
            node_value(it, n)
        }
        "createEvent" => {
            let e = super::builtins::call_native(it, "Event", Value::Undefined, Value::Undefined, alloc::vec![Value::str("")], Value::Undefined)?;
            let init = it.native("dom.initEvent");
            if let Value::Obj(o) = e {
                it.set_hidden(o, "initEvent", init);
            }
            e
        }
        "initEvent" => {
            it.put(&this, "type", a0)?;
            Value::Undefined
        }
        "addEventListener" | "removeEventListener" => {
            let kind = it.to_str(&a0)?;
            let callback = arg(&args, 1);
            let target = if node == ROOT { Target::Document } else { Target::Node(node) };
            listen(it, target, kind, callback, &arg(&args, 2), name == "addEventListener");
            Value::Undefined
        }
        "dispatchEvent" => {
            let kind = it.get(&a0, "type")?;
            let kind = it.to_str(&kind)?;
            let prevented = dispatch_event(it, node, &kind, a0)?;
            Value::Bool(!prevented)
        }
        "click" => {
            activate(it, node)?;
            Value::Undefined
        }
        "focus" => {
            it.doc.focus = Some(node);
            it.doc.dirty = true;
            Value::Undefined
        }
        "blur" => {
            if it.doc.focus == Some(node) {
                it.doc.focus = None;
                it.doc.dirty = true;
            }
            Value::Undefined
        }
        "hasFocus" => Value::Bool(true),
        "closest" | "matches" => {
            let sel = it.string(&a0);
            let list: Vec<css::Selector> = sel.split(',').filter_map(|s| css::parse_selector(s.trim())).collect();
            let test = |doc: &crate::web::dom::Document, n: NodeId| list.iter().any(|s| css::matches(doc, n, s));
            if name == "matches" {
                Value::Bool(test(&it.doc, node))
            } else {
                let mut cur = Some(node);
                let mut found = None;
                while let Some(n) = cur {
                    if it.doc.tag(n).is_some() && test(&it.doc, n) {
                        found = Some(n);
                        break;
                    }
                    cur = it.doc.nodes[n].parent;
                }
                node_or_null(it, found)
            }
        }
        "contains" => {
            let Some(other) = node_of(it, &a0) else { return Ok(Value::Bool(false)) };
            let mut cur = Some(other);
            let mut found = false;
            while let Some(n) = cur {
                if n == node {
                    found = true;
                    break;
                }
                cur = it.doc.nodes[n].parent;
            }
            Value::Bool(found)
        }
        "hasChildNodes" => Value::Bool(!it.doc.nodes[node].children.is_empty()),
        "cloneNode" => {
            let deep = it.truthy(&a0);
            let copy = clone(it, node, deep);
            node_value(it, copy)
        }
        "getBoundingClientRect" => rect_object(it, node),
        "getClientRects" => {
            let r = rect_object(it, node);
            it.new_array(alloc::vec![r])
        }
        "submit" => {
            submit_form(it, node, None);
            Value::Undefined
        }
        "requestSubmit" => {
            // submit() olay göndermez; requestSubmit() gönderir ve iptal edilebilir.
            let submitter = match &a0 {
                Value::Obj(_) => node_of(it, &a0),
                _ => None,
            }
            .or_else(|| default_submitter(it, node));
            let e = make_event(it, "submit", node, &[]);
            if !dispatch_event(it, node, "submit", e)? {
                submit_form(it, node, submitter);
            }
            Value::Undefined
        }
        "checkValidity" | "reportValidity" => Value::Bool(true),
        "reset" | "select" | "scrollIntoView" | "scrollTo" | "setPointerCapture" | "releasePointerCapture" | "setSelectionRange" | "normalize" => Value::Undefined,
        "showModal" | "show" => {
            it.doc.set_attr(node, "open", "");
            Value::Undefined
        }
        "isEqualNode" => Value::Bool(node_of(it, &a0) == Some(node)),
        "attachShadow" => this,
        "animate" => {
            let o = it.new_object();
            Value::Obj(o)
        }
        _ => throw!(it, TypeError, "{} desteklenmiyor", name),
    })
}

fn encode_form(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "-_.*".contains(c) {
            out.push(c);
        } else if c == ' ' {
            out.push('+');
        } else {
            out.push_str(&alloc::format!("%{:02X}", b));
        }
    }
    out
}

fn clone(it: &mut Interp, node: NodeId, deep: bool) -> NodeId {
    let kind = it.doc.nodes[node].kind.clone();
    let copy = it.doc.create(kind);
    if deep {
        for c in it.doc.nodes[node].children.clone() {
            let cc = clone(it, c, true);
            it.doc.append(copy, cc);
        }
    }
    copy
}

fn listen(it: &mut Interp, target: Target, kind: Rc<str>, callback: Value, options: &Value, add: bool) {
    let same_target = |a: &Target, b: &Target| match (a, b) {
        (Target::Node(x), Target::Node(y)) => x == y,
        (Target::Document, Target::Document) | (Target::Window, Target::Window) => true,
        _ => false,
    };
    if add {
        if callback == Value::Undefined || callback == Value::Null {
            return;
        }
        let once = match options {
            Value::Obj(_) => it.get(options, "once").map(|v| it.truthy(&v)).unwrap_or(false),
            _ => false,
        };
        if it
            .listeners
            .iter()
            .any(|l| same_target(&l.target, &target) && l.kind == kind && l.callback == callback)
        {
            return;
        }
        it.listeners.push(Listener {
            target,
            kind,
            callback,
            once,
        });
    } else {
        it.listeners
            .retain(|l| !(same_target(&l.target, &target) && l.kind == kind && l.callback == callback));
    }
}

/// Genel (window) yöntemleri; DOM'a ait olmayanlar için `None`.
pub fn call_global(it: &mut Interp, name: &str, _this: Value, args: Vec<Value>) -> Option<R<Value>> {
    let a0 = arg(&args, 0);
    Some(match name {
        "addEventListener" | "removeEventListener" => {
            let kind = match it.to_str(&a0) {
                Ok(k) => k,
                Err(e) => return Some(Err(e)),
            };
            listen(it, Target::Window, kind, arg(&args, 1), &arg(&args, 2), name == "addEventListener");
            Ok(Value::Undefined)
        }
        "dispatchEvent" => Ok(Value::Bool(true)),
        "getComputedStyle" => {
            let node = node_of(it, &a0);
            let p = it.protos.object;
            let o = it.alloc(Some(p), Kind::Style(node.unwrap_or(ROOT)));
            let f = it.native_with("dom.style.getPropertyValue", Value::Num(node.unwrap_or(ROOT) as f64));
            it.set_hidden(o, "getPropertyValue", f);
            Ok(Value::Obj(o))
        }
        "scrollTo" | "scroll" => {
            it.host.console.push((9, String::from("scroll")));
            Ok(Value::Undefined)
        }
        "open" => {
            let u = it.string(&a0);
            if !u.is_empty() {
                it.host.navigate = Some(crate::web::url::resolve(&it.url, &u));
            }
            Ok(Value::Null)
        }
        _ => return None,
    })
}

/// Olay nesnesi oluşturur.
pub fn make_event(it: &mut Interp, kind: &str, target: NodeId, fields: &[(&str, Value)]) -> Value {
    let p = it.protos.event;
    let e = it.alloc(Some(p), Kind::Plain);
    it.set_prop(e, "type", Value::str(kind));
    let t = node_value(it, target);
    it.set_prop(e, "target", t.clone());
    it.set_prop(e, "srcElement", t);
    it.set_prop(e, "bubbles", Value::Bool(true));
    it.set_prop(e, "defaultPrevented", Value::Bool(false));
    it.set_prop(e, "timeStamp", Value::Num((it.now_ms)() as f64));
    for (k, v) in fields {
        it.set_prop(e, k, v.clone());
    }
    Value::Obj(e)
}

/// Olayı hedeften belgeye ve pencereye doğru yayar; varsayılan engellendiyse `true`.
pub fn dispatch_event(it: &mut Interp, target: NodeId, kind: &str, event: Value) -> R<bool> {
    let tv = node_value(it, target);
    if it.get(&event, "target")? == Value::Undefined {
        it.put(&event, "target", tv)?;
    }
    let mut path = Vec::new();
    let mut cur = Some(target);
    while let Some(n) = cur {
        path.push(n);
        cur = it.doc.nodes[n].parent;
    }
    let bubbles = it.get(&event, "bubbles")?;
    let bubbles = it.truthy(&bubbles) || matches!(kind, "click" | "input" | "change" | "submit" | "keydown" | "keyup" | "keypress" | "mousedown" | "mouseup" | "focusin" | "focusout");
    for (depth, node) in path.iter().enumerate() {
        if depth > 0 && !bubbles {
            break;
        }
        let current = node_value(it, *node);
        it.put(&event, "currentTarget", current.clone())?;
        // on<olay> özelliği ya da özniteliği.
        let handler = it.get(&current, &alloc::format!("on{}", kind))?;
        if it.is_callable(&handler) {
            it.call(&handler, current.clone(), alloc::vec![event.clone()])?;
        } else if let Some(code) = it.doc.attr(*node, &alloc::format!("on{}", kind)).map(String::from) {
            run_inline_handler(it, &code, current.clone(), event.clone());
        }
        let callbacks: Vec<(Value, bool)> = it
            .listeners
            .iter()
            .filter(|l| l.kind.as_ref() == kind && match l.target {
                Target::Node(n) => n == *node,
                Target::Document => *node == ROOT,
                Target::Window => false,
            })
            .map(|l| (l.callback.clone(), l.once))
            .collect();
        for (cb, once) in callbacks {
            if once {
                it.listeners.retain(|l| l.callback != cb);
            }
            call_listener(it, &cb, current.clone(), event.clone())?;
        }
        let stopped = it.get(&event, "__stopped")?;
        if it.truthy(&stopped) {
            break;
        }
    }
    if bubbles || matches!(kind, "load" | "resize" | "DOMContentLoaded" | "popstate" | "hashchange") {
        let window_cbs: Vec<Value> = it
            .listeners
            .iter()
            .filter(|l| matches!(l.target, Target::Window) && l.kind.as_ref() == kind)
            .map(|l| l.callback.clone())
            .collect();
        let g = Value::Obj(it.global);
        for cb in window_cbs {
            call_listener(it, &cb, g.clone(), event.clone())?;
        }
    }
    let prevented = it.get(&event, "defaultPrevented")?;
    Ok(it.truthy(&prevented))
}

fn call_listener(it: &mut Interp, cb: &Value, this: Value, event: Value) -> R<()> {
    let result = if it.is_callable(cb) {
        it.call(cb, this, alloc::vec![event])
    } else {
        // { handleEvent() } nesnesi.
        let h = it.get(cb, "handleEvent")?;
        it.call(&h, cb.clone(), alloc::vec![event])
    };
    if let Err(e) = result {
        let text = it.error_text(&e);
        it.host.console.push((2, alloc::format!("Yakalanmamış {}", text)));
    }
    it.run_microtasks();
    Ok(())
}

fn run_inline_handler(it: &mut Interp, code: &str, this: Value, event: Value) {
    let src = alloc::format!("(function(event){{{}\n}})", code);
    let Ok(program) = super::parser::parse(&src) else { return };
    let Some(super::parser::Stmt::Expr(e)) = program.first() else { return };
    let env = it.global_env;
    if let Ok(f) = it.eval(e, env)
        && let Err(err) = it.call(&f, this, alloc::vec![event.clone()])
    {
        let text = it.error_text(&err);
        it.host.console.push((2, alloc::format!("Yakalanmamış {}", text)));
    }
    if let Ok(v) = it.get(&event, "returnValue")
        && v == Value::Bool(false)
    {
        let _ = it.put(&event, "defaultPrevented", Value::Bool(true));
    }
}

/// Tıklama: olay yayılır, engellenmediyse varsayılan davranış (bağlantı, onay
/// kutusu, gönder düğmesi) uygulanır.
pub fn activate(it: &mut Interp, node: NodeId) -> R<()> {
    // Onay kutusu/radyo değeri olaydan önce değişir.
    let is_input = it.doc.is(node, "input");
    let kind = it.doc.attr(node, "type").unwrap_or("").to_ascii_lowercase();
    if is_input && (kind == "checkbox" || kind == "radio") {
        let checked = it.doc.attr(node, "checked").is_some();
        if kind == "radio" {
            uncheck_radio_group(it, node);
            it.doc.set_attr(node, "checked", "");
        } else if checked {
            it.doc.remove_attr(node, "checked");
        } else {
            it.doc.set_attr(node, "checked", "");
        }
    }
    let event = make_event(it, "click", node, &[("button", Value::Num(0.0)), ("detail", Value::Num(1.0))]);
    let prevented = dispatch_event(it, node, "click", event)?;
    if is_input && (kind == "checkbox" || kind == "radio") {
        let e = make_event(it, "change", node, &[]);
        dispatch_event(it, node, "change", e)?;
        let e = make_event(it, "input", node, &[]);
        dispatch_event(it, node, "input", e)?;
    }
    if prevented {
        return Ok(());
    }
    // Varsayılan eylem: en yakın bağlantı ya da gönder düğmesi.
    let mut cur = Some(node);
    while let Some(n) = cur {
        if it.doc.is(n, "a")
            && let Some(href) = it.doc.attr(n, "href").map(String::from)
        {
            if href.starts_with("javascript:") {
                let code = String::from(&href[11..]);
                it.run_script(&super::builtins::uri_decode(&code));
            } else if !href.starts_with('#') || href.len() > 1 {
                it.host.navigate = Some(crate::web::url::resolve(&it.url, &href));
            }
            return Ok(());
        }
        let submit = (it.doc.is(n, "button") && it.doc.attr(n, "type").is_none_or(|t| t.eq_ignore_ascii_case("submit")))
            || (it.doc.is(n, "input") && it.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("submit") || t.eq_ignore_ascii_case("image")));
        if submit {
            let mut f = it.doc.nodes[n].parent;
            while let Some(p) = f {
                if it.doc.is(p, "form") {
                    let e = make_event(it, "submit", p, &[]);
                    if !dispatch_event(it, p, "submit", e)? {
                        submit_form(it, p, Some(n));
                    }
                    break;
                }
                f = it.doc.nodes[p].parent;
            }
            return Ok(());
        }
        cur = it.doc.nodes[n].parent;
    }
    Ok(())
}

fn form_pairs(it: &Interp, form: NodeId) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for n in it.doc.descendants(form) {
        let Some(tag) = it.doc.tag(n) else { continue };
        if !matches!(tag, "input" | "select" | "textarea") || it.doc.attr(n, "disabled").is_some() {
            continue;
        }
        let Some(name) = it.doc.attr(n, "name") else { continue };
        let kind = it.doc.attr(n, "type").unwrap_or("text").to_ascii_lowercase();
        if (kind == "checkbox" || kind == "radio") && it.doc.attr(n, "checked").is_none() {
            continue;
        }
        if matches!(kind.as_str(), "submit" | "button" | "reset" | "image" | "file") {
            continue;
        }
        let value = if kind == "checkbox" && it.doc.attr(n, "value").is_none() {
            String::from("on")
        } else {
            control_value(it, n)
        };
        pairs.push((String::from(name), value));
    }
    pairs
}

/// Formun ilk gönder düğmesi (Enter ile örtük gönderimde gönderen sayılır).
pub fn default_submitter(it: &Interp, form: NodeId) -> Option<NodeId> {
    it.doc.descendants(form).into_iter().find(|&n| {
        (it.doc.is(n, "button") && it.doc.attr(n, "type").is_none_or(|t| t.eq_ignore_ascii_case("submit")))
            || (it.doc.is(n, "input") && it.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("submit")))
    })
}

/// Formu gönderir (GET sorgu dizgesi; POST, tarayıcıya gövdeyle bildirilir). Gönderen
/// düğmenin adı/değeri ve `formaction` / `formmethod` öznitelikleri dikkate alınır.
pub fn submit_form(it: &mut Interp, form: NodeId, submitter: Option<NodeId>) {
    let mut pairs = form_pairs(it, form);
    if let Some(b) = submitter
        && let Some(name) = it.doc.attr(b, "name").filter(|n| !n.is_empty())
    {
        let value = it.doc.attr(b, "value").unwrap_or(if it.doc.is(b, "button") { "" } else { "Submit" });
        pairs.push((String::from(name), String::from(value)));
    }
    let query: Vec<String> = pairs
        .iter()
        .map(|(k, v)| alloc::format!("{}={}", encode_form(k), encode_form(v)))
        .collect();
    let override_attr = |name: &str| submitter.and_then(|b| it.doc.attr(b, name)).map(String::from);
    let action = override_attr("formaction").unwrap_or_else(|| String::from(it.doc.attr(form, "action").unwrap_or("")));
    let base = crate::web::url::resolve(&it.url, if action.is_empty() { &it.url } else { &action });
    let method = override_attr("formmethod")
        .unwrap_or_else(|| String::from(it.doc.attr(form, "method").unwrap_or("get")))
        .to_ascii_lowercase();
    let target = if method == "post" {
        alloc::format!("banna:post:{}\n{}", base, query.join("&"))
    } else {
        let base = base.split('?').next().unwrap_or(&base).split('#').next().unwrap_or("");
        alloc::format!("{}?{}", base, query.join("&"))
    };
    it.host.navigate = Some(target);
}
