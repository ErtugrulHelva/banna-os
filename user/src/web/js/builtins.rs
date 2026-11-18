//! Yerleşik nesneler ve fonksiyonlar. Her yerleşik, adıyla (`"Array.prototype.map"`)
//! tanınan bir `Kind::Native` fonksiyonudur; `call_native` adına göre çalıştırır.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Interp, Kind, ObjId, PromiseState, R, Value, number_to_string, regex, throw};
use crate::web::fmath;

const OBJECT_STATIC: &[&str] = &[
    "keys", "values", "entries", "assign", "freeze", "isFrozen", "seal", "create", "getPrototypeOf",
    "setPrototypeOf", "defineProperty", "defineProperties", "getOwnPropertyNames", "fromEntries",
    "is", "getOwnPropertyDescriptor", "preventExtensions",
];
const OBJECT_PROTO: &[&str] = &[
    "hasOwnProperty", "toString", "valueOf", "isPrototypeOf", "propertyIsEnumerable", "toLocaleString",
];
const FUNCTION_PROTO: &[&str] = &["call", "apply", "bind", "toString"];
const ARRAY_STATIC: &[&str] = &["isArray", "from", "of"];
const ARRAY_PROTO: &[&str] = &[
    "push", "pop", "shift", "unshift", "slice", "splice", "concat", "join", "reverse", "indexOf",
    "lastIndexOf", "includes", "find", "findIndex", "findLast", "findLastIndex", "filter", "map",
    "forEach", "reduce", "reduceRight", "some", "every", "sort", "fill", "keys", "values", "entries",
    "flat", "flatMap", "at", "toString", "copyWithin", "toReversed", "toSorted",
];
const STRING_STATIC: &[&str] = &["fromCharCode", "fromCodePoint", "raw"];
const STRING_PROTO: &[&str] = &[
    "charAt", "charCodeAt", "codePointAt", "indexOf", "lastIndexOf", "includes", "startsWith",
    "endsWith", "slice", "substring", "substr", "toUpperCase", "toLowerCase", "toLocaleUpperCase",
    "toLocaleLowerCase", "trim", "trimStart", "trimEnd", "split", "replace", "replaceAll", "repeat",
    "padStart", "padEnd", "concat", "match", "matchAll", "search", "toString", "valueOf", "at",
    "localeCompare", "normalize",
];
const NUMBER_STATIC: &[&str] = &["isInteger", "isFinite", "isNaN", "parseFloat", "parseInt", "isSafeInteger"];
const NUMBER_PROTO: &[&str] = &["toFixed", "toString", "valueOf", "toPrecision", "toLocaleString"];
const MATH: &[&str] = &[
    "abs", "floor", "ceil", "round", "trunc", "sign", "sqrt", "cbrt", "pow", "exp", "log", "log10",
    "log2", "sin", "cos", "tan", "atan", "atan2", "asin", "acos", "min", "max", "random", "hypot",
];
const PROMISE_STATIC: &[&str] = &["resolve", "reject", "all", "allSettled", "race", "any"];
const PROMISE_PROTO: &[&str] = &["then", "catch", "finally"];
const MAP_PROTO: &[&str] = &["get", "set", "has", "delete", "clear", "forEach", "keys", "values", "entries"];
const SET_PROTO: &[&str] = &["add", "has", "delete", "clear", "forEach", "values", "keys", "entries"];
const DATE_PROTO: &[&str] = &[
    "getTime", "valueOf", "getFullYear", "getMonth", "getDate", "getDay", "getHours", "getMinutes",
    "getSeconds", "getMilliseconds", "toISOString", "toString", "toLocaleDateString",
    "toLocaleTimeString", "toLocaleString", "getTimezoneOffset", "setTime", "toJSON",
    "getUTCFullYear", "getUTCMonth", "getUTCDate", "getUTCHours", "getUTCMinutes", "getUTCDay",
];
const REGEXP_PROTO: &[&str] = &["test", "exec", "toString"];
const GLOBAL_FNS: &[&str] = &[
    "parseInt", "parseFloat", "isNaN", "isFinite", "encodeURIComponent", "decodeURIComponent",
    "encodeURI", "decodeURI", "escape", "unescape", "setTimeout", "setInterval", "clearTimeout",
    "clearInterval", "requestAnimationFrame", "cancelAnimationFrame", "queueMicrotask", "alert",
    "confirm", "prompt", "fetch", "atob", "btoa", "getComputedStyle", "structuredClone",
    "requestIdleCallback", "matchMedia", "scrollTo", "scroll", "scrollBy", "addEventListener",
    "removeEventListener", "dispatchEvent", "open", "close", "print", "postMessage", "focus", "blur",
];

fn static_name(prefix: &str, name: &str) -> &'static str {
    // Ad listeleri `'static` olduğundan birleşik ad sızdırılarak tutulur (bir kez kurulur).
    let s = alloc::format!("{}{}", prefix, name);
    alloc::boxed::Box::leak(s.into_boxed_str())
}

fn methods(it: &mut Interp, target: ObjId, prefix: &str, names: &[&str]) {
    for name in names {
        let f = it.native(static_name(prefix, name));
        it.set_hidden(target, name, f);
    }
}

fn constructor(it: &mut Interp, name: &'static str, proto: ObjId) -> ObjId {
    let Value::Obj(c) = it.native(name) else { unreachable!() };
    it.set_hidden(c, "prototype", Value::Obj(proto));
    it.set_hidden(proto, "constructor", Value::Obj(c));
    it.set_hidden(c, "name", Value::str(name));
    let g = it.global;
    it.set_hidden(g, name, Value::Obj(c));
    c
}

pub fn install(it: &mut Interp) {
    let object = it.alloc(None, Kind::Plain);
    it.protos.object = object;
    let function = it.alloc(Some(object), Kind::Native("Function.prototype", Value::Undefined));
    it.protos.function = function;
    let global = it.alloc(Some(object), Kind::Plain);
    it.global = global;
    macro_rules! proto {
        ($field:ident, $kind:expr) => {{
            let p = it.alloc(Some(object), $kind);
            it.protos.$field = p;
            p
        }};
    }
    let array = proto!(array, Kind::Array(Vec::new()));
    let string = proto!(string, Kind::Plain);
    let number = proto!(number, Kind::Plain);
    let boolean = proto!(boolean, Kind::Plain);
    let error = proto!(error, Kind::Plain);
    let promise = proto!(promise, Kind::Plain);
    let regexp = proto!(regexp, Kind::Plain);
    let date = proto!(date, Kind::Plain);
    let map = proto!(map, Kind::Plain);
    let set = proto!(set, Kind::Plain);
    let node = proto!(node, Kind::Plain);
    let event = proto!(event, Kind::Plain);

    methods(it, object, "Object.prototype.", OBJECT_PROTO);
    methods(it, function, "Function.prototype.", FUNCTION_PROTO);
    methods(it, array, "Array.prototype.", ARRAY_PROTO);
    methods(it, string, "String.prototype.", STRING_PROTO);
    methods(it, number, "Number.prototype.", NUMBER_PROTO);
    methods(it, boolean, "Boolean.prototype.", &["toString", "valueOf"]);
    methods(it, promise, "Promise.prototype.", PROMISE_PROTO);
    methods(it, map, "Map.prototype.", MAP_PROTO);
    methods(it, set, "Set.prototype.", SET_PROTO);
    methods(it, date, "Date.prototype.", DATE_PROTO);
    methods(it, regexp, "RegExp.prototype.", REGEXP_PROTO);
    methods(it, error, "Error.prototype.", &["toString"]);
    methods(it, event, "Event.prototype.", &["preventDefault", "stopPropagation", "stopImmediatePropagation"]);
    super::dom::install_node_proto(it, node);
    it.set_hidden(error, "name", Value::str("Error"));
    it.set_hidden(error, "message", Value::str(""));
    let iter = it.native("Array.prototype.values");
    it.set_hidden(array, "@@iterator", iter);

    let c = constructor(it, "Object", object);
    methods(it, c, "Object.", OBJECT_STATIC);
    constructor(it, "Function", function);
    let c = constructor(it, "Array", array);
    methods(it, c, "Array.", ARRAY_STATIC);
    let c = constructor(it, "String", string);
    methods(it, c, "String.", STRING_STATIC);
    let c = constructor(it, "Number", number);
    methods(it, c, "Number.", NUMBER_STATIC);
    for (k, v) in [
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
    ] {
        it.set_hidden(c, k, Value::Num(v));
    }
    constructor(it, "Boolean", boolean);
    let c = constructor(it, "Promise", promise);
    methods(it, c, "Promise.", PROMISE_STATIC);
    constructor(it, "Map", map);
    constructor(it, "WeakMap", map);
    constructor(it, "Set", set);
    constructor(it, "WeakSet", set);
    let c = constructor(it, "Date", date);
    methods(it, c, "Date.", &["now", "parse", "UTC"]);
    constructor(it, "RegExp", regexp);
    constructor(it, "Error", error);
    for (name, field) in [
        ("TypeError", 0),
        ("RangeError", 1),
        ("SyntaxError", 2),
        ("ReferenceError", 3),
        ("EvalError", 4),
        ("URIError", 5),
    ] {
        let p = it.alloc(Some(error), Kind::Plain);
        it.set_hidden(p, "name", Value::str(name));
        match field {
            0 => it.protos.type_error = p,
            1 => it.protos.range_error = p,
            2 => it.protos.syntax_error = p,
            3 => it.protos.reference_error = p,
            _ => {}
        }
        let n: &'static str = static_name("", name);
        constructor(it, n, p);
    }
    let c = constructor(it, "Event", event);
    let _ = c;
    constructor(it, "CustomEvent", event);
    constructor(it, "KeyboardEvent", event);
    constructor(it, "MouseEvent", event);

    let math = it.new_object();
    methods(it, math, "Math.", MATH);
    for (k, v) in [
        ("PI", core::f64::consts::PI),
        ("E", core::f64::consts::E),
        ("LN2", core::f64::consts::LN_2),
        ("LN10", core::f64::consts::LN_10),
        ("SQRT2", core::f64::consts::SQRT_2),
        ("LOG10E", core::f64::consts::LOG10_E),
        ("LOG2E", core::f64::consts::LOG2_E),
        ("SQRT1_2", core::f64::consts::FRAC_1_SQRT_2),
    ] {
        it.set_hidden(math, k, Value::Num(v));
    }
    it.set_hidden(global, "Math", Value::Obj(math));
    let json = it.new_object();
    methods(it, json, "JSON.", &["parse", "stringify"]);
    it.set_hidden(global, "JSON", Value::Obj(json));
    let console = it.new_object();
    methods(it, console, "console.", &["log", "info", "warn", "error", "debug", "trace", "table", "group", "groupEnd", "groupCollapsed", "dir", "assert", "time", "timeEnd", "count"]);
    it.set_hidden(global, "console", Value::Obj(console));
    let reflect = it.new_object();
    methods(it, reflect, "Reflect.", &["ownKeys", "has", "get", "set", "apply", "construct", "getPrototypeOf", "defineProperty"]);
    it.set_hidden(global, "Reflect", Value::Obj(reflect));
    let symbol = it.native("Symbol");
    if let Value::Obj(s) = symbol {
        it.set_hidden(s, "iterator", Value::str("@@iterator"));
        it.set_hidden(s, "asyncIterator", Value::str("@@asyncIterator"));
        it.set_hidden(s, "toStringTag", Value::str("@@toStringTag"));
        it.set_hidden(s, "hasInstance", Value::str("@@hasInstance"));
        let f = it.native("Symbol.for");
        it.set_hidden(s, "for", f);
    }
    it.set_hidden(global, "Symbol", symbol);
    let performance = it.new_object();
    methods(it, performance, "performance.", &["now", "mark", "measure"]);
    it.set_hidden(global, "performance", Value::Obj(performance));
    for name in GLOBAL_FNS {
        let f = it.native(static_name("", name));
        it.set_hidden(global, name, f);
    }
    it.set_hidden(global, "NaN", Value::Num(f64::NAN));
    it.set_hidden(global, "Infinity", Value::Num(f64::INFINITY));
    it.set_hidden(global, "undefined", Value::Undefined);
    it.set_hidden(global, "window", Value::Obj(global));
    it.set_hidden(global, "self", Value::Obj(global));
    it.set_hidden(global, "globalThis", Value::Obj(global));
    it.set_hidden(global, "top", Value::Obj(global));
    it.set_hidden(global, "parent", Value::Obj(global));
    let xhr = it.new_object();
    let xhr_ctor = constructor(it, "XMLHttpRequest", xhr);
    let _ = xhr_ctor;
    methods(it, xhr, "XMLHttpRequest.prototype.", &["open", "send", "setRequestHeader", "getResponseHeader", "getAllResponseHeaders", "abort", "addEventListener"]);
    super::dom::install_globals(it);
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or_default()
}

fn num(it: &mut Interp, args: &[Value], i: usize) -> R<f64> {
    it.to_number(&arg(args, i))
}

fn this_str(it: &mut Interp, this: &Value) -> R<String> {
    Ok(String::from(&*it.to_str(this)?))
}

/// Göreli başlangıç/bitiş dizinini [0, len] aralığına çevirir.
fn rel(n: f64, len: usize) -> usize {
    if n.is_nan() {
        return 0;
    }
    let n = fmath::trunc(n);
    if n < 0.0 {
        (len as f64 + n).max(0.0) as usize
    } else {
        (n as usize).min(len)
    }
}

fn array_items(it: &mut Interp, this: &Value) -> R<Vec<Value>> {
    match this {
        Value::Obj(o) => match &it.obj(*o).kind {
            Kind::Array(items) => Ok(items.clone()),
            _ => it.iterate(this),
        },
        _ => it.iterate(this),
    }
}

fn with_array<T>(it: &mut Interp, this: &Value, f: impl FnOnce(&mut Vec<Value>) -> T) -> R<T> {
    if let Value::Obj(o) = this
        && let Kind::Array(items) = &mut it.obj_mut(*o).kind
    {
        return Ok(f(items));
    }
    throw!(it, TypeError, "dizi bekleniyordu")
}

fn compare_default(it: &mut Interp, a: &Value, b: &Value) -> core::cmp::Ordering {
    match (a, b) {
        (Value::Undefined, Value::Undefined) => core::cmp::Ordering::Equal,
        (Value::Undefined, _) => core::cmp::Ordering::Greater,
        (_, Value::Undefined) => core::cmp::Ordering::Less,
        _ => {
            let x = it.string(a);
            let y = it.string(b);
            x.cmp(&y)
        }
    }
}

fn merge_sort(it: &mut Interp, items: &mut [Value], cmp: &Value) -> R<()> {
    if items.len() <= 1 {
        return Ok(());
    }
    let mid = items.len() / 2;
    merge_sort(it, &mut items[..mid], cmp)?;
    merge_sort(it, &mut items[mid..], cmp)?;
    let mut merged = Vec::with_capacity(items.len());
    let (mut i, mut j) = (0, mid);
    while i < mid && j < items.len() {
        let order = if *cmp == Value::Undefined {
            compare_default(it, &items[i], &items[j])
        } else {
            let r = it.call(cmp, Value::Undefined, alloc::vec![items[i].clone(), items[j].clone()])?;
            let n = it.to_number(&r)?;
            if n > 0.0 {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Less
            }
        };
        if order == core::cmp::Ordering::Greater {
            merged.push(items[j].clone());
            j += 1;
        } else {
            merged.push(items[i].clone());
            i += 1;
        }
    }
    merged.extend_from_slice(&items[i..mid]);
    merged.extend_from_slice(&items[j..]);
    items.clone_from_slice(&merged);
    Ok(())
}

fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => a == b,
    }
}

fn regex_of(it: &Interp, v: &Value) -> Option<(regex::Regex, ObjId)> {
    if let Value::Obj(o) = v
        && let Kind::RegExp(re, _) = &it.obj(*o).kind
    {
        return Some(((**re).clone(), *o));
    }
    None
}

fn char_index_to_string(chars: &[char], s: usize, e: usize) -> String {
    chars[s.min(chars.len())..e.min(chars.len())].iter().collect()
}

fn match_array(it: &mut Interp, chars: &[char], caps: &[Option<(usize, usize)>], re: &regex::Regex) -> Value {
    let items: Vec<Value> = caps
        .iter()
        .map(|c| match c {
            Some((s, e)) => Value::str(&char_index_to_string(chars, *s, *e)),
            None => Value::Undefined,
        })
        .collect();
    let arr = it.new_array(items);
    if let Value::Obj(a) = arr {
        it.set_prop(a, "index", Value::Num(caps[0].map_or(0, |c| c.0) as f64));
        let input: String = chars.iter().collect();
        it.set_prop(a, "input", Value::str(&input));
        if !re.names.is_empty() {
            let groups = it.new_object();
            for (name, i) in &re.names {
                let v = match caps.get(*i).copied().flatten() {
                    Some((s, e)) => Value::str(&char_index_to_string(chars, s, e)),
                    None => Value::Undefined,
                };
                it.set_prop(groups, name, v);
            }
            it.set_prop(a, "groups", Value::Obj(groups));
        }
    }
    arr
}

/// `replace` değiştirme dizgesindeki `$1`, `$&` gibi kalıplar.
fn expand_replacement(template: &str, chars: &[char], caps: &[Option<(usize, usize)>]) -> String {
    let mut out = String::new();
    let t: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < t.len() {
        if t[i] == '$' && i + 1 < t.len() {
            match t[i + 1] {
                '$' => {
                    out.push('$');
                    i += 2;
                    continue;
                }
                '&' => {
                    if let Some((s, e)) = caps[0] {
                        out.push_str(&char_index_to_string(chars, s, e));
                    }
                    i += 2;
                    continue;
                }
                d if d.is_ascii_digit() => {
                    let mut n = d.to_digit(10).unwrap() as usize;
                    let mut len = 2;
                    if let Some(d2) = t.get(i + 2).and_then(|c| c.to_digit(10)) {
                        let two = n * 10 + d2 as usize;
                        if two < caps.len() {
                            n = two;
                            len = 3;
                        }
                    }
                    if n > 0 && n < caps.len() {
                        if let Some((s, e)) = caps[n] {
                            out.push_str(&char_index_to_string(chars, s, e));
                        }
                        i += len;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(t[i]);
        i += 1;
    }
    out
}

fn string_replace(it: &mut Interp, s: &str, pattern: &Value, replacement: &Value, all: bool) -> R<Value> {
    let chars: Vec<char> = s.chars().collect();
    let mut matches: Vec<Vec<Option<(usize, usize)>>> = Vec::new();
    if let Some((re, _)) = regex_of(it, pattern) {
        let global = re.global || all;
        let mut pos = 0;
        while pos <= chars.len() {
            let Some(caps) = re.exec_at(&chars, pos) else { break };
            let (s0, e0) = caps[0].unwrap();
            pos = if e0 == s0 { e0 + 1 } else { e0 };
            matches.push(caps);
            if !global {
                break;
            }
        }
    } else {
        let needle: Vec<char> = it.to_str(pattern)?.chars().collect();
        let mut pos = 0;
        while pos + needle.len() <= chars.len() {
            if chars[pos..pos + needle.len()] == needle[..] {
                matches.push(alloc::vec![Some((pos, pos + needle.len()))]);
                pos += needle.len().max(1);
                if !all {
                    break;
                }
            } else {
                pos += 1;
            }
        }
    }
    let callable = it.is_callable(replacement);
    let template = if callable { String::new() } else { String::from(&*it.to_str(replacement)?) };
    let mut out = String::new();
    let mut last = 0;
    for caps in matches {
        let (s0, e0) = caps[0].unwrap();
        out.push_str(&char_index_to_string(&chars, last, s0));
        if callable {
            let mut args: Vec<Value> = caps
                .iter()
                .map(|c| c.map_or(Value::Undefined, |(a, b)| Value::str(&char_index_to_string(&chars, a, b))))
                .collect();
            args.push(Value::Num(s0 as f64));
            args.push(Value::str(s));
            let r = it.call(replacement, Value::Undefined, args)?;
            out.push_str(&it.to_str(&r)?);
        } else {
            out.push_str(&expand_replacement(&template, &chars, &caps));
        }
        last = e0;
    }
    out.push_str(&char_index_to_string(&chars, last, chars.len()));
    Ok(Value::str(&out))
}

// ---- JSON ----

fn json_quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_stringify(it: &mut Interp, v: &Value, indent: &str, level: usize, out: &mut String, seen: &mut Vec<ObjId>) -> R<bool> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            if n.is_finite() {
                out.push_str(&number_to_string(*n));
            } else {
                out.push_str("null");
            }
        }
        Value::Str(s) => json_quote(s, out),
        Value::Undefined => return Ok(false),
        Value::Obj(o) => {
            let o = *o;
            if it.is_callable(v) {
                return Ok(false);
            }
            if seen.contains(&o) {
                throw!(it, TypeError, "döngüsel yapı JSON'a çevrilemez");
            }
            let to_json = it.get(v, "toJSON")?;
            if it.is_callable(&to_json) {
                let r = it.call(&to_json, v.clone(), Vec::new())?;
                return json_stringify(it, &r, indent, level, out, seen);
            }
            if let Kind::Primitive(p) = &it.obj(o).kind {
                let p = p.clone();
                return json_stringify(it, &p, indent, level, out, seen);
            }
            seen.push(o);
            let pad = |level: usize| -> String {
                if indent.is_empty() {
                    String::new()
                } else {
                    let mut s = String::from("\n");
                    for _ in 0..level {
                        s.push_str(indent);
                    }
                    s
                }
            };
            if let Kind::Array(items) = &it.obj(o).kind {
                let items = items.clone();
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&pad(level + 1));
                    if !json_stringify(it, item, indent, level + 1, out, seen)? {
                        out.push_str("null");
                    }
                }
                if !items.is_empty() {
                    out.push_str(&pad(level));
                }
                out.push(']');
            } else {
                out.push('{');
                let mut first = true;
                for k in it.own_keys(o) {
                    let value = it.get(v, &k)?;
                    let mut part = String::new();
                    if !json_stringify(it, &value, indent, level + 1, &mut part, seen)? {
                        continue;
                    }
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    out.push_str(&pad(level + 1));
                    json_quote(&k, out);
                    out.push(':');
                    if !indent.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&part);
                }
                if !first {
                    out.push_str(&pad(level));
                }
                out.push('}');
            }
            seen.pop();
        }
    }
    Ok(true)
}

pub fn json_to_value(it: &mut Interp, v: &crate::json::Value) -> Value {
    use crate::json::Value as J;
    match v {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => Value::Num(*n),
        J::String(s) => Value::str(s),
        J::Array(items) => {
            let items = items.iter().map(|i| json_to_value(it, i)).collect();
            it.new_array(items)
        }
        J::Object(entries) => {
            let o = it.new_object();
            for (k, val) in entries {
                let val = json_to_value(it, val);
                it.set_prop(o, k, val);
            }
            Value::Obj(o)
        }
    }
}

// ---- tarih ----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Yerel saat dilimi: Türkiye (UTC+3).
const TZ_MS: f64 = 3.0 * 3_600_000.0;

fn date_parts(t: f64, local: bool) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    let t = if local { t + TZ_MS } else { t } as i64;
    let days = t.div_euclid(86_400_000);
    let ms = t.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let weekday = (days + 4).rem_euclid(7);
    (y, m - 1, d, ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000, weekday)
}

fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, t),
        None => (s, ""),
    };
    let parts: Vec<i64> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    if parts.is_empty() {
        return f64::NAN;
    }
    let (y, m, d) = (parts[0], *parts.get(1).unwrap_or(&1), *parts.get(2).unwrap_or(&1));
    let mut ms = 0i64;
    let mut utc = time.is_empty();
    let mut offset = 0i64;
    if !time.is_empty() {
        let (clock, zone) = if let Some(z) = time.strip_suffix('Z') {
            utc = true;
            (z, "")
        } else if let Some(i) = time.rfind(['+', '-']).filter(|&i| i > 0) {
            utc = true;
            (&time[..i], &time[i..])
        } else {
            (time, "")
        };
        let hms: Vec<f64> = clock.split(':').filter_map(|p| p.parse().ok()).collect();
        ms = (hms.first().copied().unwrap_or(0.0) * 3_600_000.0
            + hms.get(1).copied().unwrap_or(0.0) * 60_000.0
            + hms.get(2).copied().unwrap_or(0.0) * 1000.0) as i64;
        if !zone.is_empty() {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let z: Vec<i64> = zone[1..].split(':').filter_map(|p| p.parse().ok()).collect();
            let (h, mi) = match z.as_slice() {
                [hm] if *hm >= 100 => (hm / 100, hm % 100),
                [h] => (*h, 0),
                [h, m, ..] => (*h, *m),
                _ => (0, 0),
            };
            offset = sign * (h * 3_600_000 + mi * 60_000);
        }
    }
    let t = days_from_civil(y, m, d) * 86_400_000 + ms - offset;
    if utc { t as f64 } else { t as f64 - TZ_MS }
}

fn iso(t: f64) -> String {
    let (y, m, d, h, mi, s, ms, _) = date_parts(t, false);
    alloc::format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m + 1, d, h, mi, s, ms)
}

// ---- URI ----

fn uri_encode(s: &str, keep: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            out.push_str(&alloc::format!("%{:02X}", b));
        }
    }
    out
}

pub fn uri_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(core::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - i * 6)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let Some(v) = B64.iter().position(|&b| b == c) else { continue };
        buf = buf << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

fn random() -> f64 {
    static mut STATE: u64 = 0;
    unsafe {
        let s = &mut *core::ptr::addr_of_mut!(STATE);
        if *s == 0 {
            *s = crate::time::now_ns() | 1;
        }
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        (*s >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn add_timer(it: &mut Interp, args: &[Value], interval: bool) -> Value {
    let callback = arg(args, 0);
    let delay = match arg(args, 1) {
        Value::Num(n) if n.is_finite() && n > 0.0 => n as u64,
        _ => 0,
    };
    let id = it.next_timer;
    it.next_timer += 1;
    let now = (it.now_ms)();
    it.timers.push(super::Timer {
        id,
        due: now + delay,
        callback,
        args: args.get(2..).map(|a| a.to_vec()).unwrap_or_default(),
        interval: interval.then_some(delay),
    });
    Value::Num(id as f64)
}

fn promise_resolved(it: &mut Interp, v: Value) -> R<Value> {
    if let Value::Obj(o) = &v
        && let Kind::Promise(_) = it.obj(*o).kind
    {
        return Ok(v);
    }
    let p = it.new_promise();
    it.resolve_promise(p, v)?;
    Ok(Value::Obj(p))
}

/// Yanıt nesnesi (fetch).
fn response(it: &mut Interp, status: u16, body: String, headers: Vec<(String, String)>, url: &str) -> Value {
    let r = it.new_object();
    it.set_prop(r, "status", Value::Num(status as f64));
    it.set_prop(r, "ok", Value::Bool((200..300).contains(&status)));
    it.set_prop(r, "url", Value::str(url));
    it.set_prop(r, "statusText", Value::str(if status == 200 { "OK" } else { "" }));
    let h = it.new_object();
    for (k, v) in &headers {
        it.set_prop(h, &k.to_ascii_lowercase(), Value::str(v));
    }
    let get = it.native("Headers.get");
    it.set_hidden(h, "get", get);
    it.set_prop(r, "headers", Value::Obj(h));
    it.set_hidden(r, "__body", Value::str(&body));
    for name in ["text", "json", "blob", "arrayBuffer"] {
        let f = it.native(static_name("Response.", name));
        it.set_hidden(r, name, f);
    }
    Value::Obj(r)
}

pub fn call_native(it: &mut Interp, name: &'static str, data: Value, this: Value, args: Vec<Value>, new_target: Value) -> R<Value> {
    if let Some(rest) = name.strip_prefix("dom.") {
        return super::dom::call(it, rest, data, this, args);
    }
    let a0 = arg(&args, 0);
    Ok(match name {
        // ---- iç yardımcılar ----
        "__resolve" => {
            if let Value::Obj(p) = data {
                it.resolve_promise(p, a0)?;
            }
            Value::Undefined
        }
        "__reject" => {
            if let Value::Obj(p) = data {
                it.reject_promise(p, a0);
            }
            Value::Undefined
        }
        "__reaction" => {
            let Value::Obj(derived) = data else { return Ok(Value::Undefined) };
            let handler = a0;
            let value = arg(&args, 1);
            let ok = arg(&args, 2) == Value::Bool(true);
            if it.is_callable(&handler) {
                match it.call(&handler, Value::Undefined, alloc::vec![value]) {
                    Ok(v) => it.resolve_promise(derived, v)?,
                    Err(e) => it.reject_promise(derived, e),
                }
            } else if ok {
                it.resolve_promise(derived, value)?;
            } else {
                it.reject_promise(derived, value);
            }
            Value::Undefined
        }
        "__finally" => {
            // data = [geri çağırma, geçen değer, başarılı mı]
            let cb = it.get(&data, "0")?;
            let r = it.call(&cb, Value::Undefined, Vec::new())?;
            let _ = r;
            let ok = it.get(&data, "1")?;
            if it.truthy(&ok) {
                a0
            } else {
                return Err(a0);
            }
        }
        "__all_item" => {
            // data = [sonuçlar dizisi, dizin, kalan sayaç kutusu, söz, allSettled mı, durum]
            let results = it.get(&data, "0")?;
            let index = it.get(&data, "1")?;
            let counter = it.get(&data, "2")?;
            let promise = it.get(&data, "3")?;
            let settled = it.get(&data, "4")?;
            let status = it.get(&data, "5")?;
            let value = if it.truthy(&settled) {
                let o = it.new_object();
                let ok = it.truthy(&status);
                it.set_prop(o, "status", Value::str(if ok { "fulfilled" } else { "rejected" }));
                it.set_prop(o, if ok { "value" } else { "reason" }, a0);
                Value::Obj(o)
            } else {
                a0
            };
            let k = it.key_of(&index)?;
            it.put(&results, &k, value)?;
            let left = it.get(&counter, "n")?;
            let left = it.to_number(&left)? - 1.0;
            it.put(&counter, "n", Value::Num(left))?;
            if left == 0.0
                && let Value::Obj(p) = promise
            {
                it.resolve_promise(p, results)?;
            }
            Value::Undefined
        }
        "Function.prototype" => Value::Undefined,

        // ---- Object ----
        "Object" => match a0 {
            Value::Undefined | Value::Null => Value::Obj(it.new_object()),
            Value::Obj(_) => a0,
            other => {
                let proto = it.proto_of_value(&other);
                Value::Obj(it.alloc(proto, Kind::Primitive(other)))
            }
        },
        "Object.keys" | "Object.values" | "Object.entries" | "Object.getOwnPropertyNames" | "Reflect.ownKeys" => {
            let keys: Vec<Rc<str>> = match &a0 {
                Value::Obj(o) => it.own_keys(*o),
                Value::Str(s) => (0..s.chars().count()).map(|i| Rc::from(alloc::format!("{}", i).as_str())).collect(),
                _ => Vec::new(),
            };
            let mut out = Vec::new();
            for k in keys {
                out.push(match name {
                    "Object.values" => it.get(&a0, &k)?,
                    "Object.entries" => {
                        let v = it.get(&a0, &k)?;
                        it.new_array(alloc::vec![Value::Str(k), v])
                    }
                    _ => Value::Str(k),
                });
            }
            it.new_array(out)
        }
        "Object.assign" => {
            if let Value::Obj(t) = a0 {
                for src in args.iter().skip(1) {
                    it.copy_props(t, src)?;
                }
            }
            a0
        }
        "Object.freeze" | "Object.seal" | "Object.preventExtensions" => {
            if let Value::Obj(o) = a0 {
                it.obj_mut(o).frozen = name == "Object.freeze";
            }
            a0
        }
        "Object.isFrozen" => Value::Bool(matches!(a0, Value::Obj(o) if it.obj(o).frozen)),
        "Object.create" => {
            let proto = match a0 {
                Value::Obj(p) => Some(p),
                _ => None,
            };
            let o = it.alloc(proto, Kind::Plain);
            if let Value::Obj(props) = arg(&args, 1) {
                define_properties(it, o, props)?;
            }
            Value::Obj(o)
        }
        "Object.getPrototypeOf" | "Reflect.getPrototypeOf" => match it.proto_of_value(&a0) {
            Some(p) => Value::Obj(p),
            None => Value::Null,
        },
        "Object.setPrototypeOf" => {
            if let Value::Obj(o) = a0 {
                it.obj_mut(o).proto = match arg(&args, 1) {
                    Value::Obj(p) => Some(p),
                    _ => None,
                };
            }
            a0
        }
        "Object.defineProperty" | "Reflect.defineProperty" => {
            if let Value::Obj(o) = a0 {
                let key = it.key_of(&arg(&args, 1))?;
                define_property(it, o, &key, &arg(&args, 2))?;
            }
            a0
        }
        "Object.defineProperties" => {
            if let (Value::Obj(o), Value::Obj(props)) = (&a0, arg(&args, 1)) {
                define_properties(it, *o, props)?;
            }
            a0
        }
        "Object.getOwnPropertyDescriptor" => {
            let key = it.key_of(&arg(&args, 1))?;
            match &a0 {
                Value::Obj(o) => match it.own(*o, &key) {
                    Some(slot) => {
                        let (value, getter, setter, en) = (slot.value.clone(), slot.getter.clone(), slot.setter.clone(), slot.enumerable);
                        let d = it.new_object();
                        if let Some(g) = getter {
                            it.set_prop(d, "get", g);
                        }
                        if let Some(s) = setter {
                            it.set_prop(d, "set", s);
                        }
                        it.set_prop(d, "value", value);
                        it.set_prop(d, "enumerable", Value::Bool(en));
                        it.set_prop(d, "configurable", Value::Bool(true));
                        it.set_prop(d, "writable", Value::Bool(true));
                        Value::Obj(d)
                    }
                    None => Value::Undefined,
                },
                _ => Value::Undefined,
            }
        }
        "Object.fromEntries" => {
            let o = it.new_object();
            for entry in it.iterate(&a0)? {
                let k = it.get(&entry, "0")?;
                let v = it.get(&entry, "1")?;
                let k = it.key_of(&k)?;
                it.set_prop(o, &k, v);
            }
            Value::Obj(o)
        }
        "Object.is" => {
            let b = arg(&args, 1);
            Value::Bool(match (&a0, &b) {
                (Value::Num(x), Value::Num(y)) => (x.is_nan() && y.is_nan()) || (x == y && x.is_sign_negative() == y.is_sign_negative()),
                _ => a0 == b,
            })
        }
        "Object.prototype.hasOwnProperty" => {
            let key = it.key_of(&a0)?;
            Value::Bool(match &this {
                Value::Obj(o) => {
                    it.own(*o, &key).is_some()
                        || matches!(&it.obj(*o).kind, Kind::Array(items) if super::index(&key).is_some_and(|i| i < items.len()))
                        || (matches!(it.obj(*o).kind, Kind::Storage(_) | Kind::Dataset(_)) && super::dom::get(it, *o, &key)?.is_some_and(|v| v != Value::Undefined))
                }
                Value::Str(s) => super::index(&key).is_some_and(|i| i < s.chars().count()),
                _ => false,
            })
        }
        "Object.prototype.propertyIsEnumerable" => {
            let key = it.key_of(&a0)?;
            Value::Bool(matches!(&this, Value::Obj(o) if it.own(*o, &key).is_some_and(|s| s.enumerable)))
        }
        "Object.prototype.isPrototypeOf" => {
            let mut cursor = it.proto_of_value(&a0);
            let mut found = false;
            while let Some(p) = cursor {
                if Value::Obj(p) == this {
                    found = true;
                    break;
                }
                cursor = it.obj(p).proto;
            }
            Value::Bool(found)
        }
        "Object.prototype.toString" | "Object.prototype.toLocaleString" => {
            let tag = match &this {
                Value::Undefined => "Undefined",
                Value::Null => "Null",
                Value::Obj(o) => match it.obj(*o).kind {
                    Kind::Array(_) => "Array",
                    Kind::Function(_) | Kind::Native(..) | Kind::Bound(..) => "Function",
                    Kind::Error => "Error",
                    Kind::Date(_) => "Date",
                    Kind::RegExp(..) => "RegExp",
                    _ => "Object",
                },
                Value::Str(_) => "String",
                Value::Num(_) => "Number",
                Value::Bool(_) => "Boolean",
            };
            Value::str(&alloc::format!("[object {}]", tag))
        }
        "Object.prototype.valueOf" => match &this {
            Value::Obj(o) => match &it.obj(*o).kind {
                Kind::Primitive(p) => p.clone(),
                _ => this.clone(),
            },
            _ => this.clone(),
        },

        // ---- Function ----
        "Function" => throw!(it, Error, "Function() kurucusu desteklenmiyor"),
        "Function.prototype.call" => it.call(&this, a0, args.get(1..).map(|a| a.to_vec()).unwrap_or_default())?,
        "Function.prototype.apply" => {
            let list = match arg(&args, 1) {
                Value::Undefined | Value::Null => Vec::new(),
                v => it.iterate(&v)?,
            };
            it.call(&this, a0, list)?
        }
        "Function.prototype.bind" => {
            let fp = it.protos.function;
            let bound = it.alloc(Some(fp), Kind::Bound(this.clone(), a0, args.get(1..).map(|a| a.to_vec()).unwrap_or_default()));
            Value::Obj(bound)
        }
        "Function.prototype.toString" => Value::str("function () { [native code] }"),

        // ---- Array ----
        "Array" => {
            if args.len() == 1
                && let Value::Num(n) = a0
            {
                it.new_array(alloc::vec![Value::Undefined; n as usize])
            } else {
                it.new_array(args)
            }
        }
        "Array.isArray" => Value::Bool(matches!(a0, Value::Obj(o) if matches!(it.obj(o).kind, Kind::Array(_)))),
        "Array.of" => it.new_array(args),
        "Array.from" => {
            let items = match &a0 {
                Value::Undefined | Value::Null => Vec::new(),
                Value::Obj(o) if !matches!(it.obj(*o).kind, Kind::Array(_) | Kind::Set(_) | Kind::Map(_)) && it.get(&a0, "@@iterator")? == Value::Undefined => {
                    let len = it.get(&a0, "length")?;
                    let n = it.to_number(&len)?;
                    let n = if n.is_nan() { 0 } else { n as usize };
                    let mut out = Vec::new();
                    for i in 0..n.min(10_000_000) {
                        out.push(it.get(&a0, &alloc::format!("{}", i))?);
                    }
                    out
                }
                _ => it.iterate(&a0)?,
            };
            let f = arg(&args, 1);
            if it.is_callable(&f) {
                let mut out = Vec::new();
                for (i, v) in items.into_iter().enumerate() {
                    out.push(it.call(&f, Value::Undefined, alloc::vec![v, Value::Num(i as f64)])?);
                }
                it.new_array(out)
            } else {
                it.new_array(items)
            }
        }
        "Array.prototype.push" => with_array(it, &this, |items| {
            items.extend(args.iter().cloned());
            Value::Num(items.len() as f64)
        })?,
        "Array.prototype.pop" => with_array(it, &this, |items| items.pop().unwrap_or_default())?,
        "Array.prototype.shift" => with_array(it, &this, |items| {
            if items.is_empty() {
                Value::Undefined
            } else {
                items.remove(0)
            }
        })?,
        "Array.prototype.unshift" => with_array(it, &this, |items| {
            for (i, v) in args.iter().enumerate() {
                items.insert(i, v.clone());
            }
            Value::Num(items.len() as f64)
        })?,
        "Array.prototype.slice" => {
            let items = array_items(it, &this)?;
            let s = rel(num(it, &args, 0).unwrap_or(0.0), items.len());
            let e = match arg(&args, 1) {
                Value::Undefined => items.len(),
                v => rel(it.to_number(&v)?, items.len()),
            };
            it.new_array(if s < e { items[s..e].to_vec() } else { Vec::new() })
        }
        "Array.prototype.splice" => {
            let len = array_items(it, &this)?.len();
            let start = rel(num(it, &args, 0)?, len);
            let count = match arg(&args, 1) {
                Value::Undefined if args.len() < 2 => len - start,
                v => (it.to_number(&v)?.max(0.0) as usize).min(len - start),
            };
            let insert: Vec<Value> = args.get(2..).map(|a| a.to_vec()).unwrap_or_default();
            let removed = with_array(it, &this, |items| items.splice(start..start + count, insert).collect::<Vec<_>>())?;
            it.new_array(removed)
        }
        "Array.prototype.concat" => {
            let mut items = array_items(it, &this)?;
            for a in &args {
                match a {
                    Value::Obj(o) if matches!(it.obj(*o).kind, Kind::Array(_)) => items.extend(array_items(it, a)?),
                    other => items.push(other.clone()),
                }
            }
            it.new_array(items)
        }
        "Array.prototype.join" | "Array.prototype.toString" => {
            let items = array_items(it, &this)?;
            let sep = match a0 {
                Value::Undefined => String::from(","),
                v if name.ends_with("join") => String::from(&*it.to_str(&v)?),
                _ => String::from(","),
            };
            let mut parts = Vec::new();
            for v in &items {
                parts.push(match v {
                    Value::Undefined | Value::Null => String::new(),
                    v => String::from(&*it.to_str(v)?),
                });
            }
            Value::str(&parts.join(&sep))
        }
        "Array.prototype.reverse" => {
            with_array(it, &this, |items| items.reverse())?;
            this
        }
        "Array.prototype.toReversed" => {
            let mut items = array_items(it, &this)?;
            items.reverse();
            it.new_array(items)
        }
        "Array.prototype.indexOf" | "Array.prototype.lastIndexOf" | "Array.prototype.includes" => {
            let items = array_items(it, &this)?;
            let found = if name.ends_with("lastIndexOf") {
                items.iter().rposition(|v| it.strict_equal(v, &a0))
            } else if name.ends_with("includes") {
                items.iter().position(|v| same_value_zero(v, &a0))
            } else {
                let from = rel(num(it, &args, 1).unwrap_or(0.0), items.len());
                items[from..].iter().position(|v| it.strict_equal(v, &a0)).map(|i| i + from)
            };
            if name.ends_with("includes") {
                Value::Bool(found.is_some())
            } else {
                Value::Num(found.map_or(-1.0, |i| i as f64))
            }
        }
        "Array.prototype.find" | "Array.prototype.findIndex" | "Array.prototype.findLast" | "Array.prototype.findLastIndex" | "Array.prototype.some" | "Array.prototype.every" | "Array.prototype.filter" | "Array.prototype.map" | "Array.prototype.forEach" | "Array.prototype.flatMap" => {
            let items = array_items(it, &this)?;
            let f = a0;
            if !it.is_callable(&f) {
                throw!(it, TypeError, "{} bir fonksiyon bekliyor", name);
            }
            let this_arg = arg(&args, 1);
            let order: Vec<usize> = if name.contains("Last") {
                (0..items.len()).rev().collect()
            } else {
                (0..items.len()).collect()
            };
            let mut out = Vec::new();
            for i in order {
                let v = items[i].clone();
                let r = it.call(&f, this_arg.clone(), alloc::vec![v.clone(), Value::Num(i as f64), this.clone()])?;
                let t = it.truthy(&r);
                match name {
                    "Array.prototype.find" | "Array.prototype.findLast" if t => return Ok(v),
                    "Array.prototype.findIndex" | "Array.prototype.findLastIndex" if t => return Ok(Value::Num(i as f64)),
                    "Array.prototype.some" if t => return Ok(Value::Bool(true)),
                    "Array.prototype.every" if !t => return Ok(Value::Bool(false)),
                    "Array.prototype.filter" if t => out.push(v),
                    "Array.prototype.map" => out.push(r),
                    "Array.prototype.flatMap" => match &r {
                        Value::Obj(o) if matches!(it.obj(*o).kind, Kind::Array(_)) => out.extend(array_items(it, &r)?),
                        _ => out.push(r),
                    },
                    _ => {}
                }
            }
            match name {
                "Array.prototype.find" | "Array.prototype.findLast" | "Array.prototype.forEach" => Value::Undefined,
                "Array.prototype.findIndex" | "Array.prototype.findLastIndex" => Value::Num(-1.0),
                "Array.prototype.some" => Value::Bool(false),
                "Array.prototype.every" => Value::Bool(true),
                _ => it.new_array(out),
            }
        }
        "Array.prototype.reduce" | "Array.prototype.reduceRight" => {
            let mut items = array_items(it, &this)?;
            if name.ends_with("Right") {
                items.reverse();
            }
            let mut iter = items.into_iter().enumerate();
            let mut acc = if args.len() >= 2 {
                arg(&args, 1)
            } else {
                match iter.next() {
                    Some((_, v)) => v,
                    None => throw!(it, TypeError, "boş dizi başlangıç değeri olmadan indirgenemez"),
                }
            };
            for (i, v) in iter {
                acc = it.call(&a0, Value::Undefined, alloc::vec![acc, v, Value::Num(i as f64), this.clone()])?;
            }
            acc
        }
        "Array.prototype.sort" | "Array.prototype.toSorted" => {
            let mut items = array_items(it, &this)?;
            merge_sort(it, &mut items, &a0)?;
            if name.ends_with("toSorted") {
                it.new_array(items)
            } else {
                with_array(it, &this, |slot| *slot = items)?;
                this
            }
        }
        "Array.prototype.fill" => {
            let len = array_items(it, &this)?.len();
            let s = rel(num(it, &args, 1).unwrap_or(0.0), len);
            let e = match arg(&args, 2) {
                Value::Undefined => len,
                v => rel(it.to_number(&v)?, len),
            };
            with_array(it, &this, |items| {
                for item in items.iter_mut().take(e).skip(s) {
                    *item = a0.clone();
                }
            })?;
            this
        }
        "Array.prototype.copyWithin" => this,
        "Array.prototype.keys" | "Array.prototype.values" | "Array.prototype.entries" => {
            let items = it.iterate(&this)?;
            let out: Vec<Value> = match name {
                "Array.prototype.keys" => (0..items.len()).map(|i| Value::Num(i as f64)).collect(),
                "Array.prototype.entries" => items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| it.new_array(alloc::vec![Value::Num(i as f64), v]))
                    .collect(),
                _ => items,
            };
            it.new_array(out)
        }
        "Array.prototype.flat" => {
            let depth = match a0 {
                Value::Undefined => 1,
                v => it.to_number(&v)? as usize,
            };
            fn flatten(it: &mut Interp, items: Vec<Value>, depth: usize, out: &mut Vec<Value>) -> R<()> {
                for v in items {
                    if depth > 0
                        && let Value::Obj(o) = &v
                        && let Kind::Array(inner) = &it.obj(*o).kind
                    {
                        let inner = inner.clone();
                        flatten(it, inner, depth - 1, out)?;
                        continue;
                    }
                    out.push(v);
                }
                Ok(())
            }
            let items = array_items(it, &this)?;
            let mut out = Vec::new();
            flatten(it, items, depth, &mut out)?;
            it.new_array(out)
        }
        "Array.prototype.at" => {
            let items = array_items(it, &this)?;
            let i = num(it, &args, 0)? as i64;
            let i = if i < 0 { items.len() as i64 + i } else { i };
            items.get(i as usize).cloned().unwrap_or_default()
        }

        // ---- String ----
        "String" => {
            let s = match args.first() {
                None => Value::str(""),
                Some(v) => Value::Str(it.to_str(v)?),
            };
            if new_target != Value::Undefined {
                let p = it.protos.string;
                Value::Obj(it.alloc(Some(p), Kind::Primitive(s)))
            } else {
                s
            }
        }
        "String.fromCharCode" | "String.fromCodePoint" => {
            let mut s = String::new();
            for a in &args {
                let n = it.to_number(a)? as u32;
                s.push(char::from_u32(n).unwrap_or('\u{fffd}'));
            }
            Value::str(&s)
        }
        "String.raw" => {
            let strings = it.get(&a0, "raw")?;
            let parts = it.iterate(&strings)?;
            let mut s = String::new();
            for (i, p) in parts.iter().enumerate() {
                s.push_str(&it.to_str(p)?);
                if let Some(v) = args.get(i + 1) {
                    s.push_str(&it.to_str(v)?);
                }
            }
            Value::str(&s)
        }
        n if n.starts_with("String.prototype.") => {
            let s = this_str(it, &this)?;
            string_method(it, &n[17..], &s, &args)?
        }

        // ---- Number / Boolean ----
        "Number" => {
            let n = match args.first() {
                None => 0.0,
                Some(v) => it.to_number(v)?,
            };
            Value::Num(n)
        }
        "Boolean" => Value::Bool(it.truthy(&a0)),
        "Number.isInteger" | "Number.isSafeInteger" => Value::Bool(matches!(a0, Value::Num(n) if n.is_finite() && n == fmath::trunc(n))),
        "Number.isFinite" => Value::Bool(matches!(a0, Value::Num(n) if n.is_finite())),
        "Number.isNaN" => Value::Bool(matches!(a0, Value::Num(n) if n.is_nan())),
        "Number.parseFloat" | "parseFloat" => {
            let s = it.to_str(&a0)?;
            let t = s.trim_start();
            let mut end = 0;
            let mut seen_dot = false;
            let mut seen_e = false;
            for (i, c) in t.char_indices() {
                let ok = c.is_ascii_digit()
                    || (c == '.' && !seen_dot && !seen_e)
                    || ((c == '-' || c == '+') && (i == 0 || t[..i].ends_with(['e', 'E'])))
                    || ((c == 'e' || c == 'E') && !seen_e && i > 0);
                if !ok {
                    break;
                }
                seen_dot |= c == '.';
                seen_e |= c == 'e' || c == 'E';
                end = i + 1;
            }
            if t.starts_with("Infinity") {
                Value::Num(f64::INFINITY)
            } else {
                Value::Num(t[..end].trim_end_matches(['e', 'E', '+', '-']).parse().unwrap_or(f64::NAN))
            }
        }
        "Number.parseInt" | "parseInt" => {
            let s = it.to_str(&a0)?;
            let mut t = s.trim();
            let neg = t.starts_with('-');
            t = t.trim_start_matches(['-', '+']);
            let mut radix = match arg(&args, 1) {
                Value::Undefined => 10,
                v => it.to_number(&v)? as u32,
            };
            if radix == 0 {
                radix = 10;
            }
            if (radix == 16 || arg(&args, 1) == Value::Undefined) && (t.starts_with("0x") || t.starts_with("0X")) {
                t = &t[2..];
                radix = 16;
            }
            let digits: String = t.chars().take_while(|c| c.is_digit(radix)).collect();
            if digits.is_empty() {
                Value::Num(f64::NAN)
            } else {
                let mut v = 0f64;
                for c in digits.chars() {
                    v = v * radix as f64 + c.to_digit(radix).unwrap() as f64;
                }
                Value::Num(if neg { -v } else { v })
            }
        }
        "Number.prototype.toFixed" => {
            let n = it.to_number(&this)?;
            let d = num(it, &args, 0).unwrap_or(0.0).clamp(0.0, 20.0) as usize;
            Value::str(&alloc::format!("{:.*}", d, n))
        }
        "Number.prototype.toPrecision" => {
            let n = it.to_number(&this)?;
            match a0 {
                Value::Undefined => Value::str(&number_to_string(n)),
                p => {
                    let p = it.to_number(&p)?.clamp(1.0, 21.0) as i32;
                    let mag = if n == 0.0 { 0 } else { fmath::floor(fmath::log10(n.abs())) as i32 };
                    let decimals = (p - 1 - mag).max(0) as usize;
                    Value::str(&alloc::format!("{:.*}", decimals, n))
                }
            }
        }
        "Number.prototype.toString" => {
            let n = it.to_number(&this)?;
            let radix = match a0 {
                Value::Undefined => 10,
                v => it.to_number(&v)? as u32,
            };
            if radix == 10 || !n.is_finite() || n != fmath::trunc(n) {
                Value::str(&number_to_string(n))
            } else {
                let mut v = n.abs() as u64;
                let mut digits = Vec::new();
                loop {
                    digits.push(core::char::from_digit((v % radix as u64) as u32, radix).unwrap_or('0'));
                    v /= radix as u64;
                    if v == 0 {
                        break;
                    }
                }
                if n < 0.0 {
                    digits.push('-');
                }
                digits.reverse();
                Value::str(&digits.into_iter().collect::<String>())
            }
        }
        "Number.prototype.toLocaleString" => {
            let n = it.to_number(&this)?;
            let whole = fmath::trunc(n.abs()) as u64;
            let mut s = alloc::format!("{}", whole);
            let mut out = String::new();
            while s.len() > 3 {
                let tail = s.split_off(s.len() - 3);
                out = alloc::format!(".{}{}", tail, out);
            }
            out = alloc::format!("{}{}", s, out);
            let frac = n.abs() - whole as f64;
            if frac > 0.0005 {
                let f = alloc::format!("{:.2}", frac);
                out.push(',');
                out.push_str(f.trim_start_matches("0.").trim_end_matches('0'));
            }
            if n < 0.0 {
                out.insert(0, '-');
            }
            Value::str(&out)
        }
        "Number.prototype.valueOf" | "Boolean.prototype.valueOf" | "String.prototype.valueOf" => match &this {
            Value::Obj(o) => match &it.obj(*o).kind {
                Kind::Primitive(p) => p.clone(),
                _ => this.clone(),
            },
            _ => this.clone(),
        },
        "Boolean.prototype.toString" => Value::str(if it.truthy(&this) { "true" } else { "false" }),

        // ---- Math ----
        n if n.starts_with("Math.") => {
            let x = num(it, &args, 0).unwrap_or(f64::NAN);
            Value::Num(match &n[5..] {
                "abs" => x.abs(),
                "floor" => fmath::floor(x),
                "ceil" => fmath::ceil(x),
                "round" => fmath::round(x),
                "trunc" => fmath::trunc(x),
                "sign" => {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        x
                    }
                }
                "sqrt" => fmath::sqrt(x),
                "cbrt" => {
                    if x < 0.0 {
                        -fmath::pow(-x, 1.0 / 3.0)
                    } else {
                        fmath::pow(x, 1.0 / 3.0)
                    }
                }
                "pow" => fmath::pow(x, num(it, &args, 1)?),
                "exp" => fmath::exp(x),
                "log" => fmath::ln(x),
                "log10" => fmath::log10(x),
                "log2" => fmath::log2(x),
                "sin" => fmath::sin(x),
                "cos" => fmath::cos(x),
                "tan" => fmath::tan(x),
                "atan" => fmath::atan(x),
                "atan2" => fmath::atan2(x, num(it, &args, 1)?),
                "asin" => fmath::asin(x),
                "acos" => fmath::acos(x),
                "hypot" => {
                    let mut sum = 0.0;
                    for a in &args {
                        let v = it.to_number(a)?;
                        sum += v * v;
                    }
                    fmath::sqrt(sum)
                }
                "random" => random(),
                "min" | "max" => {
                    let max = &n[5..] == "max";
                    let mut best = if max { f64::NEG_INFINITY } else { f64::INFINITY };
                    for a in &args {
                        let v = it.to_number(a)?;
                        if v.is_nan() {
                            return Ok(Value::Num(f64::NAN));
                        }
                        if (max && v > best) || (!max && v < best) {
                            best = v;
                        }
                    }
                    best
                }
                _ => f64::NAN,
            })
        }

        // ---- JSON ----
        "JSON.parse" => {
            let s = it.to_str(&a0)?;
            match crate::json::parse(s.as_bytes()) {
                Some(v) => json_to_value(it, &v),
                None => throw!(it, SyntaxError, "geçersiz JSON"),
            }
        }
        "JSON.stringify" => {
            let indent = match arg(&args, 2) {
                Value::Num(n) => " ".repeat(n.clamp(0.0, 10.0) as usize),
                Value::Str(s) => String::from(&*s),
                _ => String::new(),
            };
            let mut out = String::new();
            if json_stringify(it, &a0, &indent, 0, &mut out, &mut Vec::new())? {
                Value::str(&out)
            } else {
                Value::Undefined
            }
        }

        // ---- console ----
        n if n.starts_with("console.") => {
            let level = match &n[8..] {
                "error" | "assert" => 2,
                "warn" => 1,
                _ => 0,
            };
            if &n[8..] == "assert" && it.truthy(&a0) {
                return Ok(Value::Undefined);
            }
            let mut parts = Vec::new();
            for a in &args {
                parts.push(match a {
                    Value::Obj(o) if !it.is_callable(a) && !matches!(it.obj(*o).kind, Kind::Error) => {
                        let mut s = String::new();
                        if json_stringify(it, a, "", 0, &mut s, &mut Vec::new()).is_err() {
                            s = String::from("[object]");
                        }
                        s
                    }
                    Value::Obj(_) => it.error_text(a),
                    other => it.string(other),
                });
            }
            it.host.console.push((level, parts.join(" ")));
            Value::Undefined
        }

        // ---- Promise ----
        "Promise" => {
            let p = it.new_promise();
            let resolve = it.native_with("__resolve", Value::Obj(p));
            let reject = it.native_with("__reject", Value::Obj(p));
            if let Err(e) = it.call(&a0, Value::Undefined, alloc::vec![resolve, reject]) {
                it.reject_promise(p, e);
            }
            Value::Obj(p)
        }
        "Promise.resolve" => promise_resolved(it, a0)?,
        "Promise.reject" => {
            let p = it.new_promise();
            it.reject_promise(p, a0);
            Value::Obj(p)
        }
        "Promise.prototype.then" | "Promise.prototype.catch" => {
            let Value::Obj(p) = this else { throw!(it, TypeError, "söz bekleniyordu") };
            let (ok, err) = if name.ends_with("catch") {
                (Value::Undefined, a0)
            } else {
                (a0, arg(&args, 1))
            };
            Value::Obj(it.promise_then(p, ok, err))
        }
        "Promise.prototype.finally" => {
            let Value::Obj(p) = this else { throw!(it, TypeError, "söz bekleniyordu") };
            let ok_data = it.new_array(alloc::vec![a0.clone(), Value::Bool(true)]);
            let err_data = it.new_array(alloc::vec![a0, Value::Bool(false)]);
            let ok = it.native_with("__finally", ok_data);
            let err = it.native_with("__finally", err_data);
            Value::Obj(it.promise_then(p, ok, err))
        }
        "Promise.all" | "Promise.allSettled" | "Promise.race" | "Promise.any" => {
            let items = it.iterate(&a0)?;
            let result = it.new_promise();
            if name == "Promise.race" || name == "Promise.any" {
                let resolve = it.native_with("__resolve", Value::Obj(result));
                let reject = it.native_with("__reject", Value::Obj(result));
                for item in items {
                    let pv = promise_resolved(it, item)?;
                    if let Value::Obj(p) = pv {
                        it.promise_then(p, resolve.clone(), if name == "Promise.any" { Value::Undefined } else { reject.clone() });
                    }
                }
                return Ok(Value::Obj(result));
            }
            let settled = name == "Promise.allSettled";
            let results = it.new_array(alloc::vec![Value::Undefined; items.len()]);
            let counter = it.new_object();
            it.set_prop(counter, "n", Value::Num(items.len() as f64));
            if items.is_empty() {
                it.resolve_promise(result, results.clone())?;
            }
            for (i, item) in items.into_iter().enumerate() {
                let pv = promise_resolved(it, item)?;
                let ok_data = it.new_array(alloc::vec![results.clone(), Value::Num(i as f64), Value::Obj(counter), Value::Obj(result), Value::Bool(settled), Value::Bool(true)]);
                let ok = it.native_with("__all_item", ok_data);
                let err = if settled {
                    let d = it.new_array(alloc::vec![results.clone(), Value::Num(i as f64), Value::Obj(counter), Value::Obj(result), Value::Bool(true), Value::Bool(false)]);
                    it.native_with("__all_item", d)
                } else {
                    it.native_with("__reject", Value::Obj(result))
                };
                if let Value::Obj(p) = pv {
                    it.promise_then(p, ok, err);
                }
            }
            Value::Obj(result)
        }

        // ---- Map / Set ----
        "Map" | "WeakMap" => {
            let p = it.protos.map;
            let m = it.alloc(Some(p), Kind::Map(Vec::new()));
            if a0 != Value::Undefined && a0 != Value::Null {
                for entry in it.iterate(&a0)? {
                    let k = it.get(&entry, "0")?;
                    let v = it.get(&entry, "1")?;
                    if let Kind::Map(e) = &mut it.obj_mut(m).kind {
                        e.push((k, v));
                    }
                }
            }
            Value::Obj(m)
        }
        "Set" | "WeakSet" => {
            let p = it.protos.set;
            let items = if a0 == Value::Undefined || a0 == Value::Null { Vec::new() } else { it.iterate(&a0)? };
            let mut unique: Vec<Value> = Vec::new();
            for v in items {
                if !unique.iter().any(|u| same_value_zero(u, &v)) {
                    unique.push(v);
                }
            }
            Value::Obj(it.alloc(Some(p), Kind::Set(unique)))
        }
        n if n.starts_with("Map.prototype.") || n.starts_with("Set.prototype.") => {
            let Value::Obj(o) = this else { throw!(it, TypeError, "Map/Set bekleniyordu") };
            let method = &n[14..];
            match (&mut it.obj_mut(o).kind, method) {
                (Kind::Map(e), "get") => e.iter().find(|(k, _)| same_value_zero(k, &a0)).map(|(_, v)| v.clone()).unwrap_or_default(),
                (Kind::Map(e), "set") => {
                    let v = arg(&args, 1);
                    match e.iter_mut().find(|(k, _)| same_value_zero(k, &a0)) {
                        Some(slot) => slot.1 = v,
                        None => e.push((a0, v)),
                    }
                    this
                }
                (Kind::Map(e), "has") => Value::Bool(e.iter().any(|(k, _)| same_value_zero(k, &a0))),
                (Kind::Map(e), "delete") => {
                    let before = e.len();
                    e.retain(|(k, _)| !same_value_zero(k, &a0));
                    Value::Bool(e.len() != before)
                }
                (Kind::Set(s), "add") => {
                    if !s.iter().any(|v| same_value_zero(v, &a0)) {
                        s.push(a0);
                    }
                    this
                }
                (Kind::Set(s), "has") => Value::Bool(s.iter().any(|v| same_value_zero(v, &a0))),
                (Kind::Set(s), "delete") => {
                    let before = s.len();
                    s.retain(|v| !same_value_zero(v, &a0));
                    Value::Bool(s.len() != before)
                }
                (Kind::Map(e), "clear") => {
                    e.clear();
                    Value::Undefined
                }
                (Kind::Set(s), "clear") => {
                    s.clear();
                    Value::Undefined
                }
                (_, "forEach") => {
                    let pairs: Vec<(Value, Value)> = match &it.obj(o).kind {
                        Kind::Map(e) => e.clone(),
                        Kind::Set(s) => s.iter().map(|v| (v.clone(), v.clone())).collect(),
                        _ => Vec::new(),
                    };
                    for (k, v) in pairs {
                        it.call(&a0, Value::Undefined, alloc::vec![v, k, this.clone()])?;
                    }
                    Value::Undefined
                }
                (_, "keys" | "values" | "entries") => {
                    let out: Vec<Value> = match &it.obj(o).kind {
                        Kind::Map(e) => {
                            let e = e.clone();
                            e.into_iter()
                                .map(|(k, v)| match method {
                                    "keys" => k,
                                    "values" => v,
                                    _ => it.new_array(alloc::vec![k, v]),
                                })
                                .collect()
                        }
                        Kind::Set(s) => {
                            let s = s.clone();
                            if method == "entries" {
                                s.into_iter().map(|v| it.new_array(alloc::vec![v.clone(), v])).collect()
                            } else {
                                s
                            }
                        }
                        _ => Vec::new(),
                    };
                    it.new_array(out)
                }
                _ => Value::Undefined,
            }
        }

        // ---- Date ----
        "Date" => {
            let now = (it.now_ms)() as f64 + boot_epoch_ms();
            let t = match args.len() {
                0 => now,
                1 => match &a0 {
                    Value::Str(s) => parse_date(s),
                    v => it.to_number(v)?,
                },
                _ => {
                    let mut f = [1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                    for (i, slot) in f.iter_mut().enumerate() {
                        if let Some(v) = args.get(i) {
                            *slot = it.to_number(v)?;
                        }
                    }
                    let days = days_from_civil(f[0] as i64, f[1] as i64 + 1, f[2] as i64);
                    days as f64 * 86_400_000.0 + f[3] * 3_600_000.0 + f[4] * 60_000.0 + f[5] * 1000.0 + f[6] - TZ_MS
                }
            };
            if new_target == Value::Undefined {
                let (y, m, d, h, mi, s, _, _) = date_parts(t, true);
                return Ok(Value::str(&alloc::format!("{:02}.{:02}.{} {:02}:{:02}:{:02} GMT+0300", d, m + 1, y, h, mi, s)));
            }
            let p = it.protos.date;
            Value::Obj(it.alloc(Some(p), Kind::Date(t)))
        }
        "Date.now" => Value::Num(fmath::floor((it.now_ms)() as f64 + boot_epoch_ms())),
        "Date.parse" => {
            let s = it.to_str(&a0)?;
            Value::Num(parse_date(&s))
        }
        "Date.UTC" => {
            let mut f = [1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            for (i, slot) in f.iter_mut().enumerate() {
                if let Some(v) = args.get(i) {
                    *slot = it.to_number(v)?;
                }
            }
            let days = days_from_civil(f[0] as i64, f[1] as i64 + 1, f[2] as i64);
            Value::Num(days as f64 * 86_400_000.0 + f[3] * 3_600_000.0 + f[4] * 60_000.0 + f[5] * 1000.0 + f[6])
        }
        n if n.starts_with("Date.prototype.") => {
            let t = match &this {
                Value::Obj(o) => match it.obj(*o).kind {
                    Kind::Date(t) => t,
                    _ => f64::NAN,
                },
                _ => f64::NAN,
            };
            let method = &n[15..];
            if method == "setTime" {
                let v = num(it, &args, 0)?;
                if let Value::Obj(o) = this {
                    it.obj_mut(o).kind = Kind::Date(v);
                }
                return Ok(Value::Num(v));
            }
            let utc = method.contains("UTC");
            let (y, m, d, h, mi, s, ms, wd) = date_parts(t, !utc);
            match method.replace("UTC", "").as_str() {
                "getTime" | "valueOf" => Value::Num(t),
                "getFullYear" => Value::Num(y as f64),
                "getMonth" => Value::Num(m as f64),
                "getDate" => Value::Num(d as f64),
                "getDay" => Value::Num(wd as f64),
                "getHours" => Value::Num(h as f64),
                "getMinutes" => Value::Num(mi as f64),
                "getSeconds" => Value::Num(s as f64),
                "getMilliseconds" => Value::Num(ms as f64),
                "getTimezoneOffset" => Value::Num(-180.0),
                "toISOString" | "toJSON" => Value::str(&iso(t)),
                "toLocaleDateString" => Value::str(&alloc::format!("{:02}.{:02}.{}", d, m + 1, y)),
                "toLocaleTimeString" => Value::str(&alloc::format!("{:02}:{:02}:{:02}", h, mi, s)),
                "toLocaleString" => Value::str(&alloc::format!("{:02}.{:02}.{} {:02}:{:02}:{:02}", d, m + 1, y, h, mi, s)),
                _ => Value::str(&alloc::format!("{:02}.{:02}.{} {:02}:{:02}:{:02} GMT+0300", d, m + 1, y, h, mi, s)),
            }
        }

        // ---- RegExp ----
        "RegExp" => {
            let (source, flags) = match regex_of(it, &a0) {
                Some((re, _)) => (re.source.clone(), re.flags.clone()),
                None => (String::from(&*it.to_str(&a0)?), String::new()),
            };
            let flags = match arg(&args, 1) {
                Value::Undefined => flags,
                v => String::from(&*it.to_str(&v)?),
            };
            match regex::Regex::new(&source, &flags) {
                Ok(re) => {
                    let p = it.protos.regexp;
                    Value::Obj(it.alloc(Some(p), Kind::RegExp(alloc::boxed::Box::new(re), 0)))
                }
                Err(e) => throw!(it, SyntaxError, "{}", e),
            }
        }
        "RegExp.prototype.test" | "RegExp.prototype.exec" => {
            let Some((re, o)) = regex_of(it, &this) else { throw!(it, TypeError, "RegExp bekleniyordu") };
            let s = it.to_str(&a0)?;
            let chars: Vec<char> = s.chars().collect();
            let start = if re.global || re.sticky {
                match &it.obj(o).kind {
                    Kind::RegExp(_, last) => *last,
                    _ => 0,
                }
            } else {
                0
            };
            let found = if start > chars.len() { None } else { re.exec_at(&chars, start) };
            if re.global || re.sticky {
                let next = found.as_ref().map_or(0, |c| {
                    let (s0, e0) = c[0].unwrap();
                    if e0 == s0 { e0 + 1 } else { e0 }
                });
                if let Kind::RegExp(_, last) = &mut it.obj_mut(o).kind {
                    *last = next;
                }
            }
            match (name.ends_with("test"), found) {
                (true, f) => Value::Bool(f.is_some()),
                (false, Some(caps)) => match_array(it, &chars, &caps, &re),
                (false, None) => Value::Null,
            }
        }
        "RegExp.prototype.toString" => match regex_of(it, &this) {
            Some((re, _)) => Value::str(&alloc::format!("/{}/{}", re.source, re.flags)),
            None => Value::str("/(?:)/"),
        },

        // ---- Error ----
        "Error" | "TypeError" | "RangeError" | "SyntaxError" | "ReferenceError" | "EvalError" | "URIError" => {
            let msg = match a0 {
                Value::Undefined => String::new(),
                v => String::from(&*it.to_str(&v)?),
            };
            let e = it.make_error(name, &msg);
            if new_target != Value::Undefined
                && let (Value::Obj(eo), Value::Obj(proto)) = (&e, it.get(&new_target, "prototype")?)
            {
                it.obj_mut(*eo).proto = Some(proto);
            }
            e
        }
        "Error.prototype.toString" => Value::str(&it.error_text(&this)),

        // ---- Symbol / Reflect ----
        "Symbol" | "Symbol.for" => {
            let d = it.string(&a0);
            Value::str(&alloc::format!("@@sym:{}:{}", d, random()))
        }
        "Reflect.has" => {
            
            it.binary("in", &arg(&args, 1), &a0)?
        }
        "Reflect.get" => {
            let k = it.key_of(&arg(&args, 1))?;
            it.get(&a0, &k)?
        }
        "Reflect.set" => {
            let k = it.key_of(&arg(&args, 1))?;
            it.put(&a0, &k, arg(&args, 2))?;
            Value::Bool(true)
        }
        "Reflect.apply" => {
            let list = it.iterate(&arg(&args, 2))?;
            it.call(&a0, arg(&args, 1), list)?
        }
        "Reflect.construct" => {
            let list = it.iterate(&arg(&args, 1))?;
            it.construct(&a0, list)?
        }

        // ---- genel ----
        "isNaN" => Value::Bool(it.to_number(&a0)?.is_nan()),
        "isFinite" => Value::Bool(it.to_number(&a0)?.is_finite()),
        "encodeURIComponent" | "escape" => Value::str(&uri_encode(&it.to_str(&a0)?, "")),
        "encodeURI" => Value::str(&uri_encode(&it.to_str(&a0)?, ";,/?:@&=+$#")),
        "decodeURIComponent" | "decodeURI" | "unescape" => Value::str(&uri_decode(&it.to_str(&a0)?)),
        "atob" => {
            let bytes = base64_decode(&it.to_str(&a0)?);
            Value::str(&bytes.iter().map(|&b| b as char).collect::<String>())
        }
        "btoa" => {
            let s = it.to_str(&a0)?;
            let bytes: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
            Value::str(&base64_encode(&bytes))
        }
        "setTimeout" | "requestIdleCallback" => add_timer(it, &args, false),
        "requestAnimationFrame" => {
            let mut a = alloc::vec![a0, Value::Num(16.0)];
            let now = (it.now_ms)() as f64;
            a.push(Value::Num(now));
            add_timer(it, &a, false)
        }
        "setInterval" => add_timer(it, &args, true),
        "clearTimeout" | "clearInterval" | "cancelAnimationFrame" => {
            let id = it.to_number(&a0).unwrap_or(0.0) as u32;
            it.timers.retain(|t| t.id != id);
            Value::Undefined
        }
        "queueMicrotask" => {
            it.microtasks.push_back((a0, Vec::new()));
            Value::Undefined
        }
        "structuredClone" => {
            let mut s = String::new();
            if json_stringify(it, &a0, "", 0, &mut s, &mut Vec::new())? {
                match crate::json::parse(s.as_bytes()) {
                    Some(v) => json_to_value(it, &v),
                    None => Value::Undefined,
                }
            } else {
                Value::Undefined
            }
        }
        "alert" | "confirm" | "prompt" => {
            let text = it.string(&a0);
            it.host.alerts.push(text);
            match name {
                "confirm" => Value::Bool(true),
                "prompt" => Value::Null,
                _ => Value::Undefined,
            }
        }
        "performance.now" => Value::Num((it.now_ms)() as f64),
        "performance.mark" | "performance.measure" | "print" | "close" | "focus" | "blur" | "postMessage" | "scrollBy" => Value::Undefined,
        "fetch" => {
            let url = match &a0 {
                Value::Obj(_) => {
                    let u = it.get(&a0, "url")?;
                    it.string(&u)
                }
                v => it.string(v),
            };
            let url = crate::web::url::resolve(&it.url, &url);
            let opts = arg(&args, 1);
            let (method, body, headers) = request_options(it, &opts)?;
            let p = it.new_promise();
            match (it.fetch)(&url, &method, &headers, &body) {
                Ok((status, text, headers)) => {
                    let r = response(it, status, text, headers, &url);
                    it.resolve_promise(p, r)?;
                }
                Err(e) => {
                    let err = it.make_error("TypeError", &alloc::format!("ağ hatası: {}", e));
                    it.reject_promise(p, err);
                }
            }
            Value::Obj(p)
        }
        "Response.text" | "Response.json" | "Response.blob" | "Response.arrayBuffer" => {
            let body = it.get(&this, "__body")?;
            let v = if name == "Response.json" {
                let s = it.to_str(&body)?;
                match crate::json::parse(s.as_bytes()) {
                    Some(j) => json_to_value(it, &j),
                    None => {
                        let p = it.new_promise();
                        let e = it.make_error("SyntaxError", "yanıt geçerli JSON değil");
                        it.reject_promise(p, e);
                        return Ok(Value::Obj(p));
                    }
                }
            } else {
                body
            };
            promise_resolved(it, v)?
        }
        "Headers.get" => {
            let k = it.to_str(&a0)?.to_ascii_lowercase();
            let v = it.get(&this, &k)?;
            if v == Value::Undefined { Value::Null } else { v }
        }
        "XMLHttpRequest" => {
            let proto = it.get(&new_target, "prototype")?;
            let proto = match proto {
                Value::Obj(p) => Some(p),
                _ => None,
            };
            let o = it.alloc(proto, Kind::Plain);
            it.set_prop(o, "readyState", Value::Num(0.0));
            it.set_prop(o, "status", Value::Num(0.0));
            it.set_prop(o, "responseText", Value::str(""));
            Value::Obj(o)
        }
        "XMLHttpRequest.prototype.open" => {
            it.put(&this, "__method", a0)?;
            it.put(&this, "__url", arg(&args, 1))?;
            it.put(&this, "readyState", Value::Num(1.0))?;
            Value::Undefined
        }
        "XMLHttpRequest.prototype.setRequestHeader" | "XMLHttpRequest.prototype.abort" => Value::Undefined,
        "XMLHttpRequest.prototype.addEventListener" => {
            let ev = it.to_str(&a0)?;
            it.put(&this, &alloc::format!("on{}", ev), arg(&args, 1))?;
            Value::Undefined
        }
        "XMLHttpRequest.prototype.getResponseHeader" => Value::Null,
        "XMLHttpRequest.prototype.getAllResponseHeaders" => Value::str(""),
        "XMLHttpRequest.prototype.send" => {
            let method = it.get(&this, "__method")?;
            let method = it.string(&method);
            let url = it.get(&this, "__url")?;
            let url = it.string(&url);
            let url = crate::web::url::resolve(&it.url, &url);
            let body = match a0 {
                Value::Undefined | Value::Null => String::new(),
                v => it.string(&v),
            };
            let (status, text) = match (it.fetch)(&url, &method, &[], &body) {
                Ok((s, t, _)) => (s, t),
                Err(_) => (0, String::new()),
            };
            it.put(&this, "status", Value::Num(status as f64))?;
            it.put(&this, "responseText", Value::str(&text))?;
            it.put(&this, "response", Value::str(&text))?;
            it.put(&this, "readyState", Value::Num(4.0))?;
            for handler in ["onreadystatechange", "onload", "onloadend"] {
                let f = it.get(&this, handler)?;
                if it.is_callable(&f) {
                    it.call(&f, this.clone(), Vec::new())?;
                }
            }
            Value::Undefined
        }
        "matchMedia" => {
            let q = it.string(&a0);
            let o = it.new_object();
            let w = it.viewport.0;
            let matches = q.contains("max-width") && extract_px(&q).is_some_and(|px| w <= px)
                || q.contains("min-width") && extract_px(&q).is_some_and(|px| w >= px);
            it.set_prop(o, "matches", Value::Bool(matches));
            it.set_prop(o, "media", Value::str(&q));
            let noop = it.native("dom.noop");
            it.set_prop(o, "addListener", noop.clone());
            it.set_prop(o, "removeListener", noop.clone());
            it.set_prop(o, "addEventListener", noop.clone());
            it.set_prop(o, "removeEventListener", noop);
            Value::Obj(o)
        }
        "Event" | "CustomEvent" | "KeyboardEvent" | "MouseEvent" => {
            let p = it.protos.event;
            let e = it.alloc(Some(p), Kind::Plain);
            let kind = it.to_str(&a0)?;
            it.set_prop(e, "type", Value::Str(kind));
            let opts = arg(&args, 1);
            if let Value::Obj(_) = opts {
                for k in ["detail", "bubbles", "cancelable", "key", "code"] {
                    let v = it.get(&opts, k)?;
                    it.set_prop(e, k, v);
                }
            }
            Value::Obj(e)
        }
        "Event.prototype.preventDefault" => {
            it.put(&this, "defaultPrevented", Value::Bool(true))?;
            Value::Undefined
        }
        "Event.prototype.stopPropagation" | "Event.prototype.stopImmediatePropagation" => {
            it.put(&this, "__stopped", Value::Bool(true))?;
            Value::Undefined
        }
        n if n.starts_with("Boolean.prototype") => Value::Undefined,
        n => match super::dom::call_global(it, n, this, args) {
            Some(r) => r?,
            None => throw!(it, TypeError, "{} desteklenmiyor", n),
        },
    })
}

fn extract_px(q: &str) -> Option<f32> {
    let i = q.find(':')?;
    let rest = q[i + 1..].trim();
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    num.parse().ok()
}

fn request_options(it: &mut Interp, opts: &Value) -> R<(String, String, Vec<(String, String)>)> {
    let mut method = String::from("GET");
    let mut body = String::new();
    let mut headers = Vec::new();
    if let Value::Obj(_) = opts {
        let m = it.get(opts, "method")?;
        if m != Value::Undefined {
            method = it.string(&m).to_ascii_uppercase();
        }
        let b = it.get(opts, "body")?;
        if b != Value::Undefined && b != Value::Null {
            body = it.string(&b);
        }
        let h = it.get(opts, "headers")?;
        if let Value::Obj(ho) = h {
            for k in it.own_keys(ho) {
                let v = it.get(&h, &k)?;
                headers.push((String::from(&*k), it.string(&v)));
            }
        }
    }
    Ok((method, body, headers))
}

/// Açılıştan beri geçen süreyi gerçek zamana çevirmek için açılış anı (ms, Unix).
fn boot_epoch_ms() -> f64 {
    (crate::time::unix_time() as f64) * 1000.0 - crate::time::uptime_ms() as f64
}

fn define_property(it: &mut Interp, o: ObjId, key: &str, desc: &Value) -> R<()> {
    let get = it.get(desc, "get")?;
    let set = it.get(desc, "set")?;
    let value = it.get(desc, "value")?;
    let enumerable = it.get(desc, "enumerable")?;
    let enumerable = it.truthy(&enumerable);
    let obj = it.obj_mut(o);
    let slot = match obj.props.iter().position(|p| &*p.key == key) {
        Some(i) => &mut obj.props[i],
        None => {
            obj.props.push(super::PropSlot {
                key: Rc::from(key),
                value: Value::Undefined,
                getter: None,
                setter: None,
                enumerable,
            });
            obj.props.last_mut().unwrap()
        }
    };
    if get != Value::Undefined {
        slot.getter = Some(get);
    }
    if set != Value::Undefined {
        slot.setter = Some(set);
    }
    if slot.getter.is_none() && slot.setter.is_none() {
        slot.value = value;
    }
    slot.enumerable = enumerable;
    Ok(())
}

fn define_properties(it: &mut Interp, o: ObjId, props: ObjId) -> R<()> {
    let pv = Value::Obj(props);
    for k in it.own_keys(props) {
        let d = it.get(&pv, &k)?;
        define_property(it, o, &k, &d)?;
    }
    Ok(())
}

fn string_method(it: &mut Interp, method: &str, s: &str, args: &[Value]) -> R<Value> {
    let a0 = arg(args, 0);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    Ok(match method {
        "charAt" | "at" => {
            let i = num(it, args, 0).unwrap_or(0.0);
            let i = if method == "at" && i < 0.0 { len as f64 + i } else { i };
            match chars.get(i as usize).filter(|_| i >= 0.0) {
                Some(c) => Value::str(c.encode_utf8(&mut [0; 4])),
                None if method == "at" => Value::Undefined,
                None => Value::str(""),
            }
        }
        "charCodeAt" | "codePointAt" => {
            let i = num(it, args, 0).unwrap_or(0.0);
            match chars.get(i as usize) {
                Some(c) => Value::Num(*c as u32 as f64),
                None => Value::Num(f64::NAN),
            }
        }
        "indexOf" | "lastIndexOf" | "includes" | "startsWith" | "endsWith" => {
            if regex_of(it, &a0).is_some() && method != "indexOf" {
                throw!(it, TypeError, "{} düzenli ifade almaz", method);
            }
            let needle: Vec<char> = it.to_str(&a0)?.chars().collect();
            let find = |from: usize| -> Option<usize> {
                (from..=len.saturating_sub(needle.len())).find(|&i| i + needle.len() <= len && chars[i..i + needle.len()] == needle[..])
            };
            match method {
                "indexOf" => {
                    let from = num(it, args, 1).unwrap_or(0.0).max(0.0) as usize;
                    Value::Num(find(from).map_or(-1.0, |i| i as f64))
                }
                "lastIndexOf" => Value::Num(
                    (0..=len.saturating_sub(needle.len()))
                        .rev()
                        .find(|&i| i + needle.len() <= len && chars[i..i + needle.len()] == needle[..])
                        .map_or(-1.0, |i| i as f64),
                ),
                "includes" => Value::Bool(find(0).is_some() || needle.is_empty()),
                "startsWith" => {
                    let at = num(it, args, 1).unwrap_or(0.0).max(0.0) as usize;
                    Value::Bool(chars.get(at..).is_some_and(|rest| rest.starts_with(&needle)))
                }
                _ => {
                    let end = match arg(args, 1) {
                        Value::Undefined => len,
                        v => (it.to_number(&v)? as usize).min(len),
                    };
                    Value::Bool(chars[..end].ends_with(&needle))
                }
            }
        }
        "slice" => {
            let s0 = rel(num(it, args, 0).unwrap_or(0.0), len);
            let e = match arg(args, 1) {
                Value::Undefined => len,
                v => rel(it.to_number(&v)?, len),
            };
            Value::str(&if s0 < e { chars[s0..e].iter().collect::<String>() } else { String::new() })
        }
        "substring" => {
            let a = num(it, args, 0).unwrap_or(0.0);
            let a = if a.is_nan() { 0 } else { (a.max(0.0) as usize).min(len) };
            let b = match arg(args, 1) {
                Value::Undefined => len,
                v => {
                    let n = it.to_number(&v)?;
                    if n.is_nan() { 0 } else { (n.max(0.0) as usize).min(len) }
                }
            };
            let (x, y) = if a < b { (a, b) } else { (b, a) };
            Value::str(&chars[x..y].iter().collect::<String>())
        }
        "substr" => {
            let start = rel(num(it, args, 0).unwrap_or(0.0), len);
            let count = match arg(args, 1) {
                Value::Undefined => len - start,
                v => (it.to_number(&v)?.max(0.0) as usize).min(len - start),
            };
            Value::str(&chars[start..start + count].iter().collect::<String>())
        }
        "toUpperCase" | "toLocaleUpperCase" => Value::str(&s.chars().map(|c| match c {
            'i' if method.contains("Locale") => String::from("İ"),
            c => c.to_uppercase().collect(),
        }).collect::<String>()),
        "toLowerCase" | "toLocaleLowerCase" => Value::str(&s.chars().map(|c| match c {
            'I' if method.contains("Locale") => String::from("ı"),
            c => c.to_lowercase().collect(),
        }).collect::<String>()),
        "trim" => Value::str(s.trim()),
        "trimStart" => Value::str(s.trim_start()),
        "trimEnd" => Value::str(s.trim_end()),
        "repeat" => {
            let n = num(it, args, 0)?;
            if n < 0.0 || n.is_infinite() || n * len as f64 > 50_000_000.0 {
                throw!(it, RangeError, "geçersiz tekrar sayısı");
            }
            Value::str(&s.repeat(n as usize))
        }
        "padStart" | "padEnd" => {
            let target = num(it, args, 0)? as usize;
            let fill = match arg(args, 1) {
                Value::Undefined => String::from(" "),
                v => String::from(&*it.to_str(&v)?),
            };
            if target <= len || fill.is_empty() {
                Value::str(s)
            } else {
                let pad: String = fill.chars().cycle().take(target - len).collect();
                Value::str(&if method == "padStart" { alloc::format!("{}{}", pad, s) } else { alloc::format!("{}{}", s, pad) })
            }
        }
        "concat" => {
            let mut out = String::from(s);
            for a in args {
                out.push_str(&it.to_str(a)?);
            }
            Value::str(&out)
        }
        "split" => {
            let limit = match arg(args, 1) {
                Value::Undefined => usize::MAX,
                v => it.to_number(&v)? as usize,
            };
            let parts: Vec<Value> = if a0 == Value::Undefined {
                alloc::vec![Value::str(s)]
            } else if let Some((re, _)) = regex_of(it, &a0) {
                let mut out = Vec::new();
                let mut last = 0;
                let mut pos = 0;
                while pos < len {
                    let Some(caps) = re.exec_at(&chars, pos) else { break };
                    let (ms, me) = caps[0].unwrap();
                    if me == ms {
                        if ms >= len {
                            break;
                        }
                        if ms == last {
                            pos = ms + 1;
                            if ms == 0 {
                                continue;
                            }
                        }
                    }
                    if me == ms && ms == 0 {
                        pos += 1;
                        continue;
                    }
                    out.push(Value::str(&char_index_to_string(&chars, last, ms)));
                    for c in caps.iter().skip(1) {
                        out.push(c.map_or(Value::Undefined, |(a, b)| Value::str(&char_index_to_string(&chars, a, b))));
                    }
                    last = me;
                    pos = if me == ms { me + 1 } else { me };
                }
                out.push(Value::str(&char_index_to_string(&chars, last, len)));
                out
            } else {
                let sep = it.to_str(&a0)?;
                if sep.is_empty() {
                    chars.iter().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect()
                } else {
                    s.split(&*sep).map(Value::str).collect()
                }
            };
            let parts = parts.into_iter().take(limit).collect();
            it.new_array(parts)
        }
        "replace" | "replaceAll" => string_replace(it, s, &a0, &arg(args, 1), method == "replaceAll")?,
        "match" | "matchAll" | "search" => {
            let re = match regex_of(it, &a0) {
                Some((re, _)) => re,
                None => {
                    let p = it.string(&a0);
                    match regex::Regex::new(&p, if method == "matchAll" { "g" } else { "" }) {
                        Ok(r) => r,
                        Err(e) => throw!(it, SyntaxError, "{}", e),
                    }
                }
            };
            if method == "search" {
                return Ok(Value::Num(re.exec_at(&chars, 0).map_or(-1.0, |c| c[0].unwrap().0 as f64)));
            }
            if re.global {
                let mut out = Vec::new();
                let mut pos = 0;
                while pos <= len {
                    let Some(caps) = re.exec_at(&chars, pos) else { break };
                    let (ms, me) = caps[0].unwrap();
                    out.push(if method == "matchAll" {
                        match_array(it, &chars, &caps, &re)
                    } else {
                        Value::str(&char_index_to_string(&chars, ms, me))
                    });
                    pos = if me == ms { me + 1 } else { me };
                }
                if out.is_empty() && method == "match" {
                    Value::Null
                } else {
                    it.new_array(out)
                }
            } else {
                match re.exec_at(&chars, 0) {
                    Some(caps) => match_array(it, &chars, &caps, &re),
                    None => Value::Null,
                }
            }
        }
        "localeCompare" => {
            let other = it.to_str(&a0)?;
            let key = |t: &str| -> String { t.chars().flat_map(|c| c.to_lowercase()).collect() };
            Value::Num(match key(s).cmp(&key(&other)) {
                core::cmp::Ordering::Less => -1.0,
                core::cmp::Ordering::Equal => 0.0,
                core::cmp::Ordering::Greater => 1.0,
            })
        }
        "normalize" | "toString" | "valueOf" => Value::str(s),
        _ => Value::Undefined,
    })
}

#[allow(dead_code)]
fn promise_state(it: &Interp, p: ObjId) -> u8 {
    match &it.obj(p).kind {
        Kind::Promise(PromiseState::Pending(_)) => 0,
        Kind::Promise(PromiseState::Fulfilled(_)) => 1,
        _ => 2,
    }
}
