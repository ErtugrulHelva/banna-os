//! JavaScript yorumlayıcısı (ağaç üzerinde yürüyen). Nesneler ve kapsamlar dizilerde
//! tutulur (sayıyla başvurulur); olay döngüsü görevleri arasında işaretle-süpür çöp
//! toplayıcı çalışır. Promise ve async/await desteklenir: `await` bekleyen bir söz için
//! zamanlayıcıları ve mikro görevleri iç içe çalıştırarak sonucu bekler.

pub mod builtins;
pub mod dom;
pub mod lexer;
pub mod parser;
pub mod regex;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use parser::{Class, Expr, Function, Name, Pattern, Prop as PropExpr, PropKey, Stmt, CONST, LET, VAR};

use super::dom::{Document, NodeId};
use super::fmath;

pub type ObjId = u32;
pub type EnvId = u32;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Obj(ObjId),
}

impl Value {
    pub fn str(s: &str) -> Self {
        Value::Str(Rc::from(s))
    }
}

pub struct Closure {
    pub f: Rc<Function>,
    pub env: EnvId,
    /// Ok fonksiyonu: tanımlandığı yerin `this`'i.
    pub this: Option<Value>,
    /// `super` için yöntemin ait olduğu nesne.
    pub home: Option<ObjId>,
    /// Sınıf kurucusu: örnek alanları ve üst sınıf.
    pub class: Option<(Rc<Class>, EnvId)>,
    pub parent: Option<Value>,
    pub is_class: bool,
}

pub enum PromiseState {
    Pending(Vec<(Value, Value, ObjId)>),
    Fulfilled(Value),
    Rejected(Value),
}

pub enum Kind {
    Plain,
    Array(Vec<Value>),
    Function(Closure),
    Native(&'static str, Value),
    Bound(Value, Value, Vec<Value>),
    Error,
    Date(f64),
    RegExp(alloc::boxed::Box<regex::Regex>, usize),
    Promise(PromiseState),
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
    /// DOM düğümü ve yardımcı nesneleri.
    Node(NodeId),
    Style(NodeId),
    ClassList(NodeId),
    Dataset(NodeId),
    Storage(bool),
    /// `arguments` ve benzeri dizimsi değerler için kutulanmış ilkel.
    Primitive(Value),
}

pub struct PropSlot {
    pub key: Rc<str>,
    pub value: Value,
    pub getter: Option<Value>,
    pub setter: Option<Value>,
    pub enumerable: bool,
}

pub struct Obj {
    pub proto: Option<ObjId>,
    pub props: Vec<PropSlot>,
    pub kind: Kind,
    pub frozen: bool,
    marked: bool,
}

pub struct Env {
    vars: Vec<(Rc<str>, Value, bool)>,
    parent: Option<EnvId>,
    this: Option<Value>,
    /// Çağrılan fonksiyon (new.target, super için).
    func: Option<ObjId>,
    new_target: Value,
    captured: bool,
    marked: bool,
    alive: bool,
}

pub enum Flow {
    Normal,
    Return(Value),
    Break(Option<Name>),
    Continue(Option<Name>),
}

/// Fırlatılan değer.
pub type Throw = Value;
pub type R<T> = Result<T, Throw>;

#[derive(Default)]
pub struct Protos {
    pub object: ObjId,
    pub function: ObjId,
    pub array: ObjId,
    pub string: ObjId,
    pub number: ObjId,
    pub boolean: ObjId,
    pub error: ObjId,
    pub promise: ObjId,
    pub regexp: ObjId,
    pub date: ObjId,
    pub map: ObjId,
    pub set: ObjId,
    pub node: ObjId,
    pub event: ObjId,
    pub type_error: ObjId,
    pub range_error: ObjId,
    pub syntax_error: ObjId,
    pub reference_error: ObjId,
}

pub struct Timer {
    pub id: u32,
    pub due: u64,
    pub callback: Value,
    pub args: Vec<Value>,
    pub interval: Option<u64>,
}

pub enum Target {
    Node(NodeId),
    Document,
    Window,
}

pub struct Listener {
    pub target: Target,
    pub kind: Rc<str>,
    pub callback: Value,
    pub once: bool,
}

/// Tarayıcıya geri bildirimler (gezinme, uyarılar, konsol).
#[derive(Default)]
pub struct HostRequests {
    pub navigate: Option<String>,
    pub alerts: Vec<String>,
    pub console: Vec<(u8, String)>,
    pub title_changed: bool,
}

pub struct Interp {
    pub objs: Vec<Option<Obj>>,
    free: Vec<ObjId>,
    envs: Vec<Env>,
    free_envs: Vec<EnvId>,
    pub global: ObjId,
    pub global_env: EnvId,
    pub protos: Protos,
    pub microtasks: VecDeque<(Value, Vec<Value>)>,
    pub timers: Vec<Timer>,
    pub next_timer: u32,
    pub listeners: Vec<Listener>,
    pub node_objs: BTreeMap<NodeId, ObjId>,
    pub doc: Document,
    pub url: String,
    pub host: HostRequests,
    /// Düğüm kutuları (getBoundingClientRect, offsetWidth) ve görünüm boyutu.
    pub rects: Vec<(NodeId, super::layout::RectF)>,
    pub viewport: (f32, f32),
    pub storage: Vec<(String, String)>,
    pub storage_dirty: bool,
    steps: u64,
    budget: u64,
    depth: u32,
    gc_threshold: usize,
    pub now_ms: fn() -> u64,
    pub fetch: fn(&str, &str, &[(String, String)], &str) -> Result<(u16, String, Vec<(String, String)>), String>,
}

pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return String::from("NaN");
    }
    if n.is_infinite() {
        return String::from(if n > 0.0 { "Infinity" } else { "-Infinity" });
    }
    if n == 0.0 {
        return String::from("0");
    }
    if n == fmath::trunc(n) && n.abs() < 1e21 {
        return alloc::format!("{}", n as i128);
    }
    let s = alloc::format!("{}", n);
    if n.abs() >= 1e21 || (n.abs() < 1e-6 && n != 0.0) {
        // Bilimsel gösterim.
        let e = alloc::format!("{:e}", n);
        return e.replace("e", "e+").replace("e+-", "e-");
    }
    s
}

fn to_number_str(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).map_or(f64::NAN, |v| v as f64);
    }
    match t {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => {
            if t.ends_with(|c: char| c.is_ascii_alphabetic()) && !t.ends_with("Infinity") {
                return f64::NAN;
            }
            t.parse().unwrap_or(f64::NAN)
        }
    }
}

macro_rules! throw {
    ($self:expr, $kind:ident, $($arg:tt)*) => {
        return Err($self.make_error(stringify!($kind), &alloc::format!($($arg)*)))
    };
}
pub(crate) use throw;

impl Interp {
    pub fn new(doc: Document, url: &str, now_ms: fn() -> u64, fetch: fn(&str, &str, &[(String, String)], &str) -> Result<(u16, String, Vec<(String, String)>), String>) -> Self {
        let mut it = Self {
            objs: Vec::new(),
            free: Vec::new(),
            envs: Vec::new(),
            free_envs: Vec::new(),
            global: 0,
            global_env: 0,
            protos: Protos::default(),
            microtasks: VecDeque::new(),
            timers: Vec::new(),
            next_timer: 1,
            listeners: Vec::new(),
            node_objs: BTreeMap::new(),
            doc,
            url: String::from(url),
            host: HostRequests::default(),
            rects: Vec::new(),
            viewport: (1024.0, 768.0),
            storage: Vec::new(),
            storage_dirty: false,
            steps: 0,
            budget: 40_000_000,
            depth: 0,
            gc_threshold: 50_000,
            now_ms,
            fetch,
        };
        it.global_env = it.new_env(None, Some(Value::Undefined));
        builtins::install(&mut it);
        if let Value::Obj(_) = Value::Obj(it.global) {
            let g = it.global;
            it.envs[it.global_env as usize].this = Some(Value::Obj(g));
        }
        it
    }

    // ---- yığın ----

    pub fn alloc(&mut self, proto: Option<ObjId>, kind: Kind) -> ObjId {
        let obj = Obj {
            proto,
            props: Vec::new(),
            kind,
            frozen: false,
            marked: false,
        };
        if let Some(id) = self.free.pop() {
            self.objs[id as usize] = Some(obj);
            id
        } else {
            self.objs.push(Some(obj));
            (self.objs.len() - 1) as ObjId
        }
    }

    pub fn obj(&self, id: ObjId) -> &Obj {
        self.objs[id as usize].as_ref().expect("freed object")
    }

    pub fn obj_mut(&mut self, id: ObjId) -> &mut Obj {
        self.objs[id as usize].as_mut().expect("freed object")
    }

    pub fn new_object(&mut self) -> ObjId {
        let p = self.protos.object;
        self.alloc(Some(p), Kind::Plain)
    }

    pub fn new_array(&mut self, items: Vec<Value>) -> Value {
        let p = self.protos.array;
        Value::Obj(self.alloc(Some(p), Kind::Array(items)))
    }

    pub fn native(&mut self, name: &'static str) -> Value {
        let p = self.protos.function;
        Value::Obj(self.alloc(Some(p), Kind::Native(name, Value::Undefined)))
    }

    pub fn native_with(&mut self, name: &'static str, data: Value) -> Value {
        let p = self.protos.function;
        Value::Obj(self.alloc(Some(p), Kind::Native(name, data)))
    }

    fn new_env(&mut self, parent: Option<EnvId>, this: Option<Value>) -> EnvId {
        let env = Env {
            vars: Vec::new(),
            parent,
            this,
            func: None,
            new_target: Value::Undefined,
            captured: false,
            marked: false,
            alive: true,
        };
        if let Some(id) = self.free_envs.pop() {
            self.envs[id as usize] = env;
            id
        } else {
            self.envs.push(env);
            (self.envs.len() - 1) as EnvId
        }
    }

    fn release_env(&mut self, id: EnvId) {
        let env = &mut self.envs[id as usize];
        if !env.captured && env.alive && id != self.global_env {
            env.alive = false;
            env.vars.clear();
            self.free_envs.push(id);
        }
    }

    fn capture(&mut self, mut env: EnvId) {
        loop {
            let e = &mut self.envs[env as usize];
            if e.captured {
                return;
            }
            e.captured = true;
            match e.parent {
                Some(p) => env = p,
                None => return,
            }
        }
    }

    // ---- özellikler ----

    pub fn set_prop(&mut self, obj: ObjId, key: &str, value: Value) {
        let o = self.obj_mut(obj);
        if let Some(slot) = o.props.iter_mut().find(|p| &*p.key == key) {
            slot.value = value;
            return;
        }
        o.props.push(PropSlot {
            key: Rc::from(key),
            value,
            getter: None,
            setter: None,
            enumerable: true,
        });
    }

    pub fn set_hidden(&mut self, obj: ObjId, key: &str, value: Value) {
        self.set_prop(obj, key, value);
        if let Some(slot) = self.obj_mut(obj).props.iter_mut().find(|p| &*p.key == key) {
            slot.enumerable = false;
        }
    }

    pub fn own(&self, obj: ObjId, key: &str) -> Option<&PropSlot> {
        self.obj(obj).props.iter().find(|p| &*p.key == key)
    }

    pub fn proto_of_value(&self, v: &Value) -> Option<ObjId> {
        match v {
            Value::Str(_) => Some(self.protos.string),
            Value::Num(_) => Some(self.protos.number),
            Value::Bool(_) => Some(self.protos.boolean),
            Value::Obj(o) => self.obj(*o).proto,
            _ => None,
        }
    }

    /// Özellik okuma (alıcılar, dizi/dizge uzunluğu, DOM özellikleri, prototip zinciri).
    pub fn get(&mut self, target: &Value, key: &str) -> R<Value> {
        match target {
            Value::Undefined | Value::Null => {
                throw!(self, TypeError, "'{}' özelliği okunamıyor ({} değerinde)", key, if *target == Value::Null { "null" } else { "undefined" })
            }
            Value::Str(s) => {
                if key == "length" {
                    return Ok(Value::Num(s.chars().count() as f64));
                }
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(s.chars().nth(i).map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).unwrap_or_default());
                }
            }
            Value::Obj(o) => {
                let o = *o;
                if let Some(v) = self.special_get(o, key)? {
                    return Ok(v);
                }
            }
            _ => {}
        }
        let mut cursor = match target {
            Value::Obj(o) => Some(*o),
            other => self.proto_of_value(other),
        };
        while let Some(id) = cursor {
            if let Some(slot) = self.own(id, key) {
                if let Some(getter) = slot.getter.clone() {
                    return self.call(&getter, target.clone(), Vec::new());
                }
                if slot.setter.is_some() {
                    return Ok(Value::Undefined);
                }
                return Ok(slot.value.clone());
            }
            cursor = self.obj(id).proto;
        }
        Ok(Value::Undefined)
    }

    fn special_get(&mut self, o: ObjId, key: &str) -> R<Option<Value>> {
        let obj = self.obj(o);
        match &obj.kind {
            Kind::Array(items) => {
                if key == "length" {
                    return Ok(Some(Value::Num(items.len() as f64)));
                }
                if let Some(i) = index(key) {
                    return Ok(Some(items.get(i).cloned().unwrap_or_default()));
                }
            }
            Kind::Function(c) if key == "length" && self.own(o, "length").is_none() => {
                return Ok(Some(Value::Num(c.f.params.len() as f64)));
            }
            Kind::Function(c) if key == "name" && self.own(o, "name").is_none() => {
                return Ok(Some(Value::Str(c.f.name.clone().unwrap_or_else(|| Rc::from("")))));
            }
            Kind::RegExp(re, last) => match key {
                "source" => return Ok(Some(Value::str(&re.source))),
                "flags" => return Ok(Some(Value::str(&re.flags))),
                "global" => return Ok(Some(Value::Bool(re.global))),
                "lastIndex" => return Ok(Some(Value::Num(*last as f64))),
                _ => {}
            },
            Kind::Map(entries) if key == "size" => return Ok(Some(Value::Num(entries.len() as f64))),
            Kind::Set(items) if key == "size" => return Ok(Some(Value::Num(items.len() as f64))),
            Kind::Node(_) | Kind::Style(_) | Kind::ClassList(_) | Kind::Dataset(_) | Kind::Storage(_) => {
                return dom::get(self, o, key);
            }
            Kind::Primitive(v) => {
                let v = v.clone();
                if key == "length"
                    && let Value::Str(s) = &v {
                        return Ok(Some(Value::Num(s.chars().count() as f64)));
                    }
            }
            _ => {}
        }
        Ok(None)
    }

    pub fn put(&mut self, target: &Value, key: &str, value: Value) -> R<()> {
        let Value::Obj(o) = target else {
            if matches!(target, Value::Undefined | Value::Null) {
                throw!(self, TypeError, "'{}' özelliği atanamıyor", key);
            }
            return Ok(());
        };
        let o = *o;
        if self.obj(o).frozen {
            return Ok(());
        }
        match &mut self.obj_mut(o).kind {
            Kind::Array(items) => {
                if key == "length" {
                    let n = match value {
                        Value::Num(n) => n as usize,
                        _ => items.len(),
                    };
                    items.resize(n, Value::Undefined);
                    return Ok(());
                }
                if let Some(i) = index(key) {
                    if i >= items.len() {
                        if i > items.len() + 10_000_000 {
                            throw!(self, RangeError, "dizi çok büyük");
                        }
                        items.resize(i + 1, Value::Undefined);
                    }
                    items[i] = value;
                    return Ok(());
                }
            }
            Kind::RegExp(_, last) if key == "lastIndex" => {
                if let Value::Num(n) = value {
                    *last = n as usize;
                }
                return Ok(());
            }
            Kind::Node(_) | Kind::Style(_) | Kind::ClassList(_) | Kind::Dataset(_) | Kind::Storage(_) => {
                if dom::set(self, o, key, value.clone())? {
                    return Ok(());
                }
            }
            _ => {}
        }
        // Prototip zincirindeki ayarlayıcı.
        let mut cursor = Some(o);
        while let Some(id) = cursor {
            if let Some(slot) = self.own(id, key) {
                if let Some(setter) = slot.setter.clone() {
                    self.call(&setter, target.clone(), alloc::vec![value])?;
                    return Ok(());
                }
                if slot.getter.is_some() {
                    return Ok(());
                }
                break;
            }
            cursor = self.obj(id).proto;
        }
        self.set_prop(o, key, value);
        Ok(())
    }

    pub fn own_keys(&self, o: ObjId) -> Vec<Rc<str>> {
        let obj = self.obj(o);
        let mut keys: Vec<Rc<str>> = match &obj.kind {
            Kind::Array(items) => (0..items.len()).map(|i| Rc::from(alloc::format!("{}", i).as_str())).collect(),
            _ => Vec::new(),
        };
        keys.extend(obj.props.iter().filter(|p| p.enumerable).map(|p| p.key.clone()));
        keys
    }

    // ---- dönüşümler ----

    pub fn truthy(&self, v: &Value) -> bool {
        match v {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Obj(_) => true,
        }
    }

    pub fn to_number(&mut self, v: &Value) -> R<f64> {
        Ok(match v {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::Num(n) => *n,
            Value::Str(s) => to_number_str(s),
            Value::Obj(o) => {
                if let Kind::Date(t) = self.obj(*o).kind {
                    return Ok(t);
                }
                let p = self.to_primitive(v)?;
                if let Value::Obj(_) = p {
                    f64::NAN
                } else {
                    self.to_number(&p)?
                }
            }
        })
    }

    pub fn to_primitive(&mut self, v: &Value) -> R<Value> {
        let Value::Obj(o) = v else { return Ok(v.clone()) };
        if let Kind::Primitive(p) = &self.obj(*o).kind {
            return Ok(p.clone());
        }
        for name in ["valueOf", "toString"] {
            let f = self.get(v, name)?;
            if self.is_callable(&f) {
                let r = self.call(&f, v.clone(), Vec::new())?;
                if !matches!(r, Value::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        Ok(Value::str("[object Object]"))
    }

    pub fn to_str(&mut self, v: &Value) -> R<Rc<str>> {
        Ok(match v {
            Value::Undefined => Rc::from("undefined"),
            Value::Null => Rc::from("null"),
            Value::Bool(b) => Rc::from(if *b { "true" } else { "false" }),
            Value::Num(n) => Rc::from(number_to_string(*n).as_str()),
            Value::Str(s) => s.clone(),
            Value::Obj(o) => {
                if let Kind::Array(items) = &self.obj(*o).kind {
                    let items = items.clone();
                    let mut parts = Vec::new();
                    for item in &items {
                        parts.push(match item {
                            Value::Undefined | Value::Null => String::new(),
                            other => String::from(&*self.to_str(other)?),
                        });
                    }
                    return Ok(Rc::from(parts.join(",").as_str()));
                }
                let p = self.to_primitive(v)?;
                if let Value::Obj(_) = p {
                    Rc::from("[object Object]")
                } else {
                    self.to_str(&p)?
                }
            }
        })
    }

    pub fn string(&mut self, v: &Value) -> String {
        self.to_str(v).map(|s| String::from(&*s)).unwrap_or_default()
    }

    pub fn key_of(&mut self, v: &Value) -> R<Rc<str>> {
        self.to_str(v)
    }

    pub fn type_of(&self, v: &Value) -> &'static str {
        match v {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Obj(o) => {
                if self.is_callable(v) {
                    "function"
                } else if matches!(self.obj(*o).kind, Kind::Primitive(Value::Str(_))) && false {
                    "string"
                } else {
                    "object"
                }
            }
        }
    }

    pub fn is_callable(&self, v: &Value) -> bool {
        matches!(v, Value::Obj(o) if matches!(self.obj(*o).kind, Kind::Function(_) | Kind::Native(..) | Kind::Bound(..)))
    }

    pub fn strict_equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Num(x), Value::Num(y)) => x == y,
            _ => a == b,
        }
    }

    pub fn loose_equal(&mut self, a: &Value, b: &Value) -> R<bool> {
        Ok(match (a, b) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::Obj(x), Value::Obj(y)) => x == y,
            (Value::Obj(_), _) => {
                let p = self.to_primitive(a)?;
                self.loose_equal(&p, b)?
            }
            (_, Value::Obj(_)) => {
                let p = self.to_primitive(b)?;
                self.loose_equal(a, &p)?
            }
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            _ => {
                let x = self.to_number(a)?;
                let y = self.to_number(b)?;
                x == y
            }
        })
    }

    pub fn make_error(&mut self, kind: &str, message: &str) -> Value {
        let proto = match kind {
            "TypeError" => self.protos.type_error,
            "RangeError" => self.protos.range_error,
            "SyntaxError" => self.protos.syntax_error,
            "ReferenceError" => self.protos.reference_error,
            _ => self.protos.error,
        };
        let e = self.alloc(Some(proto), Kind::Error);
        self.set_hidden(e, "message", Value::str(message));
        self.set_hidden(e, "stack", Value::str(&alloc::format!("{}: {}", kind, message)));
        Value::Obj(e)
    }

    pub fn error_text(&mut self, v: &Value) -> String {
        if let Value::Obj(o) = v
            && matches!(self.obj(*o).kind, Kind::Error)
        {
            let name = self.get(v, "name").unwrap_or_default();
            let msg = self.get(v, "message").unwrap_or_default();
            let name = self.string(&name);
            let msg = self.string(&msg);
            return alloc::format!("{}: {}", name, msg);
        }
        self.string(v)
    }

    // ---- kapsamlar ----

    fn declare(&mut self, env: EnvId, name: &str, value: Value, constant: bool) {
        let e = &mut self.envs[env as usize];
        if let Some(slot) = e.vars.iter_mut().find(|(n, ..)| &**n == name) {
            slot.1 = value;
            slot.2 = constant;
        } else {
            e.vars.push((Rc::from(name), value, constant));
        }
    }

    fn lookup(&mut self, env: EnvId, name: &str) -> R<Value> {
        let mut cur = Some(env);
        while let Some(id) = cur {
            let e = &self.envs[id as usize];
            if let Some((_, v, _)) = e.vars.iter().find(|(n, ..)| &**n == name) {
                return Ok(v.clone());
            }
            cur = e.parent;
        }
        let g = Value::Obj(self.global);
        let mut cursor = Some(self.global);
        while let Some(id) = cursor {
            if self.own(id, name).is_some() {
                return self.get(&g, name);
            }
            cursor = self.obj(id).proto;
        }
        throw!(self, ReferenceError, "{} tanımlı değil", name)
    }

    fn assign_var(&mut self, env: EnvId, name: &str, value: Value) -> R<()> {
        let mut cur = Some(env);
        while let Some(id) = cur {
            let e = &mut self.envs[id as usize];
            if let Some(slot) = e.vars.iter_mut().find(|(n, ..)| &**n == name) {
                if slot.2 {
                    throw!(self, TypeError, "sabit '{}' değiştirilemez", name);
                }
                slot.1 = value;
                return Ok(());
            }
            cur = e.parent;
        }
        let g = Value::Obj(self.global);
        self.put(&g, name, value)
    }

    fn this_of(&self, env: EnvId) -> Value {
        let mut cur = Some(env);
        while let Some(id) = cur {
            let e = &self.envs[id as usize];
            if let Some(t) = &e.this {
                return t.clone();
            }
            cur = e.parent;
        }
        Value::Undefined
    }

    fn func_env(&self, env: EnvId) -> Option<EnvId> {
        let mut cur = Some(env);
        while let Some(id) = cur {
            let e = &self.envs[id as usize];
            if e.func.is_some() {
                return Some(id);
            }
            cur = e.parent;
        }
        None
    }

    // ---- çalıştırma ----

    fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.budget {
            self.steps = 0;
            throw!(self, Error, "betik çok uzun sürdü ve durduruldu");
        }
        Ok(())
    }

    /// Bir betiği genel kapsamda çalıştırır; hatayı konsola yazar.
    pub fn run_script(&mut self, src: &str) {
        self.steps = 0;
        let program = match parser::parse(src) {
            Ok(p) => p,
            Err(e) => {
                self.host.console.push((2, alloc::format!("SyntaxError: {}", e)));
                return;
            }
        };
        let env = self.global_env;
        self.hoist(&program, env, true);
        let mut result = Ok(Flow::Normal);
        for stmt in &program {
            result = self.exec(stmt, env);
            if result.is_err() {
                break;
            }
        }
        if let Err(e) = result {
            let text = self.error_text(&e);
            self.host.console.push((2, alloc::format!("Yakalanmamış {}", text)));
        }
        self.run_microtasks();
    }

    /// `var` ve fonksiyon bildirimlerini kapsamın başına taşır.
    fn hoist(&mut self, body: &[Stmt], env: EnvId, functions: bool) {
        for stmt in body {
            self.hoist_vars(stmt, env);
        }
        if functions {
            for stmt in body {
                if let Stmt::Function(f) = stmt {
                    let closure = self.make_closure(f.clone(), env, None);
                    self.declare(env, f.name.as_deref().unwrap_or(""), closure, false);
                }
            }
        }
    }

    fn hoist_vars(&mut self, stmt: &Stmt, env: EnvId) {
        match stmt {
            Stmt::Var(VAR, decls) => {
                for (p, _) in decls {
                    let mut names = Vec::new();
                    pattern_names(p, &mut names);
                    for n in names {
                        if !self.envs[env as usize].vars.iter().any(|(x, ..)| *x == n) {
                            self.declare(env, &n, Value::Undefined, false);
                        }
                    }
                }
            }
            Stmt::If(_, a, b) => {
                self.hoist_vars(a, env);
                if let Some(b) = b {
                    self.hoist_vars(b, env);
                }
            }
            Stmt::Block(body) => {
                for s in body {
                    self.hoist_vars(s, env);
                }
            }
            Stmt::For(init, _, _, body) => {
                if let Some(i) = init {
                    self.hoist_vars(i, env);
                }
                self.hoist_vars(body, env);
            }
            Stmt::ForIn(kind, p, _, body, _) => {
                if *kind == VAR {
                    let mut names = Vec::new();
                    pattern_names(p, &mut names);
                    for n in names {
                        self.declare(env, &n, Value::Undefined, false);
                    }
                }
                self.hoist_vars(body, env);
            }
            Stmt::While(_, b) | Stmt::DoWhile(b, _) | Stmt::Labeled(_, b) => self.hoist_vars(b, env),
            Stmt::Try(a, c, f) => {
                for s in a {
                    self.hoist_vars(s, env);
                }
                if let Some((_, c)) = c {
                    for s in c {
                        self.hoist_vars(s, env);
                    }
                }
                if let Some(f) = f {
                    for s in f {
                        self.hoist_vars(s, env);
                    }
                }
            }
            Stmt::Switch(_, cases) => {
                for (_, body) in cases {
                    for s in body {
                        self.hoist_vars(s, env);
                    }
                }
            }
            _ => {}
        }
    }

    fn block_needs_scope(body: &[Stmt]) -> bool {
        body.iter().any(|s| matches!(s, Stmt::Var(LET | CONST, _) | Stmt::Class(_) | Stmt::Function(_)))
    }

    fn exec_block(&mut self, body: &[Stmt], env: EnvId) -> R<Flow> {
        let scope = if Self::block_needs_scope(body) {
            let e = self.new_env(Some(env), None);
            for s in body {
                if let Stmt::Function(f) = s {
                    let c = self.make_closure(f.clone(), e, None);
                    self.declare(e, f.name.as_deref().unwrap_or(""), c, false);
                }
            }
            e
        } else {
            env
        };
        let mut result = Ok(Flow::Normal);
        for s in body {
            result = self.exec(s, scope);
            match &result {
                Ok(Flow::Normal) => {}
                _ => break,
            }
        }
        if scope != env {
            self.release_env(scope);
        }
        result
    }

    pub fn exec(&mut self, stmt: &Stmt, env: EnvId) -> R<Flow> {
        self.tick()?;
        match stmt {
            Stmt::Expr(e) => {
                self.eval(e, env)?;
            }
            Stmt::Var(kind, decls) => {
                for (pattern, init) in decls {
                    let value = match init {
                        Some(e) => {
                            let v = self.eval(e, env)?;
                            self.name_function(&v, pattern);
                            v
                        }
                        None if *kind == VAR => {
                            if let Pattern::Ident(_) = pattern {
                                continue;
                            }
                            Value::Undefined
                        }
                        None => Value::Undefined,
                    };
                    self.bind(pattern, value, env, Some(*kind))?;
                }
            }
            Stmt::Function(_) => {}
            Stmt::Class(c) => {
                let v = self.make_class(c, env)?;
                self.declare(env, c.name.as_deref().unwrap_or(""), v, false);
            }
            Stmt::Block(body) => return self.exec_block(body, env),
            Stmt::If(cond, then, otherwise) => {
                let c = self.eval(cond, env)?;
                if self.truthy(&c) {
                    return self.exec(then, env);
                } else if let Some(o) = otherwise {
                    return self.exec(o, env);
                }
            }
            Stmt::While(cond, body) => loop {
                let c = self.eval(cond, env)?;
                if !self.truthy(&c) {
                    break;
                }
                match self.exec(body, env)? {
                    Flow::Break(None) => break,
                    Flow::Continue(None) | Flow::Normal => {}
                    other => return Ok(other),
                }
            },
            Stmt::DoWhile(body, cond) => loop {
                match self.exec(body, env)? {
                    Flow::Break(None) => break,
                    Flow::Continue(None) | Flow::Normal => {}
                    other => return Ok(other),
                }
                let c = self.eval(cond, env)?;
                if !self.truthy(&c) {
                    break;
                }
            },
            Stmt::For(init, test, update, body) => return self.exec_for(init, test, update, body, env, None),
            Stmt::ForIn(kind, pattern, iter, body, of) => {
                return self.exec_for_in(*kind, pattern, iter, body, *of, env, None);
            }
            Stmt::Labeled(label, body) => {
                let r = match &**body {
                    Stmt::For(i, t, u, b) => self.exec_for(i, t, u, b, env, Some(label))?,
                    Stmt::ForIn(k, p, it, b, of) => self.exec_for_in(*k, p, it, b, *of, env, Some(label))?,
                    other => self.exec(other, env)?,
                };
                return Ok(match r {
                    Flow::Break(Some(l)) if l == *label => Flow::Normal,
                    other => other,
                });
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                return Ok(Flow::Return(v));
            }
            Stmt::Break(l) => return Ok(Flow::Break(l.clone())),
            Stmt::Continue(l) => return Ok(Flow::Continue(l.clone())),
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                return Err(v);
            }
            Stmt::Try(block, catch, finally) => {
                let mut result = self.exec_block(block, env);
                if let (Err(e), Some((param, body))) = (&result, catch) {
                    let e = e.clone();
                    let scope = self.new_env(Some(env), None);
                    if let Some(p) = param {
                        self.bind(p, e, scope, Some(LET))?;
                    }
                    result = self.exec_block(body, scope);
                    self.release_env(scope);
                }
                if let Some(f) = finally {
                    match self.exec_block(f, env)? {
                        Flow::Normal => {}
                        other => return Ok(other),
                    }
                }
                return result;
            }
            Stmt::Switch(disc, cases) => {
                let d = self.eval(disc, env)?;
                let scope = self.new_env(Some(env), None);
                let mut matched = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(t) = test {
                        let v = self.eval(t, scope)?;
                        if self.strict_equal(&d, &v) {
                            matched = Some(i);
                            break;
                        }
                    }
                }
                if matched.is_none() {
                    matched = cases.iter().position(|(t, _)| t.is_none());
                }
                if let Some(start) = matched {
                    for (_, body) in &cases[start..] {
                        for s in body {
                            match self.exec(s, scope)? {
                                Flow::Normal => {}
                                Flow::Break(None) => {
                                    self.release_env(scope);
                                    return Ok(Flow::Normal);
                                }
                                other => {
                                    self.release_env(scope);
                                    return Ok(other);
                                }
                            }
                        }
                    }
                }
                self.release_env(scope);
            }
            Stmt::Empty => {}
        }
        Ok(Flow::Normal)
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_for(
        &mut self,
        init: &Option<alloc::boxed::Box<Stmt>>,
        test: &Option<Expr>,
        update: &Option<Expr>,
        body: &Stmt,
        env: EnvId,
        label: Option<&Name>,
    ) -> R<Flow> {
        let scope = self.new_env(Some(env), None);
        if let Some(i) = init {
            self.exec(i, scope)?;
        }
        // Her döngü adımı için yeni kapsam yalnızca gövde kapanış oluşturuyorsa gerekir.
        let per_iteration = matches!(init.as_deref(), Some(Stmt::Var(LET, _))) && contains_closure(body);
        let mut iter_env = scope;
        loop {
            if let Some(t) = test {
                let c = self.eval(t, iter_env)?;
                if !self.truthy(&c) {
                    break;
                }
            }
            match self.exec(body, iter_env)? {
                Flow::Break(None) => break,
                Flow::Break(Some(l)) if Some(&l) == label => break,
                Flow::Continue(None) | Flow::Normal => {}
                Flow::Continue(Some(l)) if Some(&l) == label => {}
                other => return Ok(other),
            }
            // Kapanışlar her adımın kendi değişkenini görsün: artırmadan önce kopyala.
            if per_iteration {
                let e = self.new_env(Some(env), None);
                let vars = self.envs[iter_env as usize].vars.clone();
                self.envs[e as usize].vars = vars;
                iter_env = e;
            }
            if let Some(u) = update {
                self.eval(u, iter_env)?;
            }
        }
        if !per_iteration {
            self.release_env(scope);
        }
        Ok(Flow::Normal)
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_for_in(
        &mut self,
        kind: u8,
        pattern: &Pattern,
        iter: &Expr,
        body: &Stmt,
        of: bool,
        env: EnvId,
        label: Option<&Name>,
    ) -> R<Flow> {
        let source = self.eval(iter, env)?;
        let items: Vec<Value> = if of {
            self.iterate(&source)?
        } else {
            match &source {
                Value::Obj(o) => {
                    let mut keys = Vec::new();
                    let mut cursor = Some(*o);
                    while let Some(id) = cursor {
                        for k in self.own_keys(id) {
                            if !keys.contains(&k) {
                                keys.push(k);
                            }
                        }
                        cursor = self.obj(id).proto;
                        if cursor == Some(self.protos.object) || cursor == Some(self.protos.array) {
                            break;
                        }
                    }
                    keys.into_iter().map(Value::Str).collect()
                }
                Value::Str(s) => (0..s.chars().count()).map(|i| Value::str(&alloc::format!("{}", i))).collect(),
                _ => Vec::new(),
            }
        };
        let closures = contains_closure(body);
        for item in items {
            let scope = self.new_env(Some(env), None);
            if kind == 255 {
                self.bind(pattern, item, scope, None)?;
            } else {
                self.bind(pattern, item, scope, Some(kind))?;
            }
            let r = self.exec(body, scope)?;
            if !closures {
                self.release_env(scope);
            }
            match r {
                Flow::Break(None) => break,
                Flow::Break(Some(l)) if Some(&l) == label => break,
                Flow::Continue(None) | Flow::Normal => {}
                Flow::Continue(Some(l)) if Some(&l) == label => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Yinelenebilir değeri diziye çevirir (dizi, dizge, Map, Set, NodeList, arguments).
    pub fn iterate(&mut self, v: &Value) -> R<Vec<Value>> {
        match v {
            Value::Str(s) => Ok(s.chars().map(|c| Value::str(c.encode_utf8(&mut [0; 4]))).collect()),
            Value::Obj(o) => {
                let o = *o;
                match &self.obj(o).kind {
                    Kind::Array(items) => Ok(items.clone()),
                    Kind::Set(items) => Ok(items.clone()),
                    Kind::Map(entries) => {
                        let entries = entries.clone();
                        Ok(entries.into_iter().map(|(k, v)| self.new_array(alloc::vec![k, v])).collect())
                    }
                    _ => {
                        // Yineleyici protokolü: [Symbol.iterator]() → next().
                        let it_fn = self.get(v, "@@iterator")?;
                        if self.is_callable(&it_fn) {
                            let iterator = self.call(&it_fn, v.clone(), Vec::new())?;
                            let mut out = Vec::new();
                            for _ in 0..1_000_000 {
                                let next = self.get(&iterator, "next")?;
                                let step = self.call(&next, iterator.clone(), Vec::new())?;
                                let done = self.get(&step, "done")?;
                                if self.truthy(&done) {
                                    break;
                                }
                                out.push(self.get(&step, "value")?);
                            }
                            return Ok(out);
                        }
                        let len = self.get(v, "length")?;
                        if let Value::Num(n) = len {
                            let mut out = Vec::new();
                            for i in 0..n as usize {
                                out.push(self.get(v, &alloc::format!("{}", i))?);
                            }
                            return Ok(out);
                        }
                        throw!(self, TypeError, "değer yinelenebilir değil")
                    }
                }
            }
            _ => throw!(self, TypeError, "değer yinelenebilir değil"),
        }
    }

    fn name_function(&mut self, v: &Value, pattern: &Pattern) {
        if let (Value::Obj(o), Pattern::Ident(name)) = (v, pattern)
            && let Kind::Function(c) = &self.obj(*o).kind
            && c.f.name.is_none()
            && self.own(*o, "name").is_none()
        {
            let name = name.clone();
            self.set_hidden(*o, "name", Value::Str(name));
        }
    }

    /// Kalıba değer bağlar: `kind` varsa yeni bildirim, yoksa atama.
    pub fn bind(&mut self, pattern: &Pattern, value: Value, env: EnvId, kind: Option<u8>) -> R<()> {
        match pattern {
            Pattern::Ident(name) => match kind {
                Some(k) => self.declare(env, name, value, k == CONST),
                None => self.assign_var(env, name, value)?,
            },
            Pattern::Expr(target) => self.assign_to(target, value, env)?,
            Pattern::Array(items, rest) => {
                let values = if matches!(value, Value::Undefined | Value::Null) {
                    throw!(self, TypeError, "undefined yapı bozulamaz")
                } else {
                    self.iterate(&value)?
                };
                for (i, item) in items.iter().enumerate() {
                    if let Some((p, default)) = item {
                        let mut v = values.get(i).cloned().unwrap_or_default();
                        if v == Value::Undefined
                            && let Some(d) = default
                        {
                            v = self.eval(d, env)?;
                        }
                        self.bind(p, v, env, kind)?;
                    }
                }
                if let Some(r) = rest {
                    let tail = values.get(items.len()..).map(|s| s.to_vec()).unwrap_or_default();
                    let arr = self.new_array(tail);
                    self.bind(r, arr, env, kind)?;
                }
            }
            Pattern::Object(props, rest) => {
                if matches!(value, Value::Undefined | Value::Null) {
                    throw!(self, TypeError, "undefined ya da null yapı bozulamaz");
                }
                let mut used = Vec::new();
                for (key, p, default) in props {
                    let k = match key {
                        PropKey::Name(n) => n.clone(),
                        PropKey::Computed(e) => {
                            let v = self.eval(e, env)?;
                            self.key_of(&v)?
                        }
                    };
                    let mut v = self.get(&value, &k)?;
                    if v == Value::Undefined
                        && let Some(d) = default
                    {
                        v = self.eval(d, env)?;
                    }
                    used.push(k);
                    self.bind(p, v, env, kind)?;
                }
                if let Some(r) = rest {
                    let out = self.new_object();
                    if let Value::Obj(o) = value {
                        for k in self.own_keys(o) {
                            if !used.contains(&k) {
                                let v = self.get(&value, &k)?;
                                self.set_prop(out, &k, v);
                            }
                        }
                    }
                    self.bind(&Pattern::Ident(r.clone()), Value::Obj(out), env, kind)?;
                }
            }
        }
        Ok(())
    }

    fn assign_to(&mut self, target: &Expr, value: Value, env: EnvId) -> R<()> {
        match target {
            Expr::Ident(n) => self.assign_var(env, n, value),
            Expr::Field(obj, name, _) => {
                let o = self.eval(obj, env)?;
                self.put(&o, name, value)
            }
            Expr::Member(obj, key, _) => {
                let o = self.eval(obj, env)?;
                let k = self.eval(key, env)?;
                let k = self.key_of(&k)?;
                self.put(&o, &k, value)
            }
            _ => throw!(self, SyntaxError, "geçersiz atama hedefi"),
        }
    }

    pub fn make_closure(&mut self, f: Rc<Function>, env: EnvId, home: Option<ObjId>) -> Value {
        self.capture(env);
        let this = if f.arrow { Some(self.this_of(env)) } else { None };
        let fp = self.protos.function;
        let arrow = f.arrow;
        let id = self.alloc(
            Some(fp),
            Kind::Function(Closure {
                f,
                env,
                this,
                home,
                class: None,
                parent: None,
                is_class: false,
            }),
        );
        if !arrow {
            let proto = self.new_object();
            self.set_hidden(proto, "constructor", Value::Obj(id));
            self.set_hidden(id, "prototype", Value::Obj(proto));
        }
        Value::Obj(id)
    }

    fn make_class(&mut self, c: &Rc<Class>, env: EnvId) -> R<Value> {
        let parent = match &c.extends {
            Some(e) => Some(self.eval(e, env)?),
            None => None,
        };
        let scope = self.new_env(Some(env), None);
        self.capture(scope);
        let proto_parent = match &parent {
            Some(Value::Null) => None,
            Some(p) => match self.get(p, "prototype")? {
                Value::Obj(o) => Some(o),
                _ => Some(self.protos.object),
            },
            None => Some(self.protos.object),
        };
        let proto = self.alloc(proto_parent, Kind::Plain);
        let ctor_fn = c.constructor.clone().unwrap_or_else(|| {
            Rc::new(Function {
                name: c.name.clone(),
                params: Vec::new(),
                rest: None,
                body: Vec::new(),
                arrow: false,
                is_async: false,
                expression: false,
            })
        });
        let fp = match &parent {
            Some(Value::Obj(p)) => *p,
            _ => self.protos.function,
        };
        let ctor = self.alloc(
            Some(fp),
            Kind::Function(Closure {
                f: ctor_fn,
                env: scope,
                this: None,
                home: Some(proto),
                class: Some((c.clone(), scope)),
                parent: parent.clone().filter(|p| *p != Value::Null),
                is_class: true,
            }),
        );
        self.set_hidden(ctor, "prototype", Value::Obj(proto));
        self.set_hidden(proto, "constructor", Value::Obj(ctor));
        if let Some(name) = &c.name {
            self.declare(scope, name, Value::Obj(ctor), true);
            self.set_hidden(ctor, "name", Value::Str(name.clone()));
        }
        for (key, f, kind, is_static) in &c.methods {
            let target = if *is_static { ctor } else { proto };
            let k = match key {
                PropKey::Name(n) => n.clone(),
                PropKey::Computed(e) => {
                    let v = self.eval(e, scope)?;
                    self.key_of(&v)?
                }
            };
            let m = self.make_closure(f.clone(), scope, Some(target));
            self.define_method(target, &k, m, *kind);
        }
        let ctor_v = Value::Obj(ctor);
        for (key, init, is_static) in &c.fields {
            if !*is_static {
                continue;
            }
            let k = match key {
                PropKey::Name(n) => n.clone(),
                PropKey::Computed(e) => {
                    let v = self.eval(e, scope)?;
                    self.key_of(&v)?
                }
            };
            let fenv = self.new_env(Some(scope), Some(ctor_v.clone()));
            let v = match init {
                Some(e) => self.eval(e, fenv)?,
                None => Value::Undefined,
            };
            if &*k != "__static_init" {
                self.set_prop(ctor, &k, v);
            }
        }
        Ok(ctor_v)
    }

    fn define_method(&mut self, target: ObjId, key: &str, f: Value, kind: u8) {
        let o = self.obj_mut(target);
        let slot = match o.props.iter_mut().position(|p| &*p.key == key) {
            Some(i) => &mut o.props[i],
            None => {
                o.props.push(PropSlot {
                    key: Rc::from(key),
                    value: Value::Undefined,
                    getter: None,
                    setter: None,
                    enumerable: false,
                });
                o.props.last_mut().unwrap()
            }
        };
        match kind {
            1 => slot.getter = Some(f),
            2 => slot.setter = Some(f),
            _ => slot.value = f,
        }
    }

    /// Fonksiyon çağrısı (kapanış, yerleşik ya da bağlanmış).
    pub fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> R<Value> {
        let Value::Obj(id) = f else {
            throw!(self, TypeError, "{} bir fonksiyon değil", self.type_of(f));
        };
        let id = *id;
        self.depth += 1;
        if self.depth > 400 {
            self.depth -= 1;
            throw!(self, RangeError, "en büyük çağrı yığını boyutu aşıldı");
        }
        let result = self.call_inner(id, this, args, Value::Undefined);
        self.depth -= 1;
        result
    }

    fn call_inner(&mut self, id: ObjId, this: Value, args: Vec<Value>, new_target: Value) -> R<Value> {
        let (f, env, captured_this, is_class, home) = match &self.obj(id).kind {
            Kind::Native(name, data) => {
                let (name, data) = (*name, data.clone());
                return builtins::call_native(self, name, data, this, args, new_target);
            }
            Kind::Bound(target, bound_this, bound_args) => {
                let (target, bound_this) = (target.clone(), bound_this.clone());
                let mut all = bound_args.clone();
                all.extend(args);
                return self.call(&target, bound_this, all);
            }
            Kind::Function(c) => (c.f.clone(), c.env, c.this.clone(), c.is_class, c.home),
            _ => throw!(self, TypeError, "değer bir fonksiyon değil"),
        };
        if is_class && new_target == Value::Undefined {
            throw!(self, TypeError, "sınıf 'new' olmadan çağrılamaz");
        }
        let this = captured_this.unwrap_or(this);
        let scope = self.new_env(Some(env), if f.arrow { None } else { Some(this) });
        if !f.arrow {
            self.envs[scope as usize].func = Some(id);
            self.envs[scope as usize].new_target = new_target;
            let args_obj = self.new_array(args.clone());
            self.declare(scope, "arguments", args_obj, false);
        }
        let _ = home;
        for (i, (p, default)) in f.params.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or_default();
            if v == Value::Undefined
                && let Some(d) = default
            {
                v = self.eval(d, scope)?;
            }
            self.bind(p, v, scope, Some(LET))?;
        }
        if let Some(rest) = &f.rest {
            let tail = args.get(f.params.len()..).map(|s| s.to_vec()).unwrap_or_default();
            let arr = self.new_array(tail);
            self.bind(rest, arr, scope, Some(LET))?;
        }
        self.hoist(&f.body, scope, true);
        if f.is_async {
            let result = self.run_body(&f, scope);
            let promise = self.new_promise();
            match result {
                Ok(v) => self.resolve_promise(promise, v)?,
                Err(e) => self.reject_promise(promise, e),
            }
            return Ok(Value::Obj(promise));
        }
        let r = self.run_body(&f, scope);
        self.release_env(scope);
        r
    }

    fn run_body(&mut self, f: &Function, scope: EnvId) -> R<Value> {
        for s in &f.body {
            match self.exec(s, scope)? {
                Flow::Return(v) => return Ok(v),
                Flow::Normal => {}
                _ => break,
            }
        }
        Ok(Value::Undefined)
    }

    /// `new F(...)`.
    pub fn construct(&mut self, f: &Value, args: Vec<Value>) -> R<Value> {
        let Value::Obj(id) = f else {
            throw!(self, TypeError, "değer bir kurucu değil");
        };
        let id = *id;
        if let Kind::Native(name, data) = &self.obj(id).kind {
            let (name, data) = (*name, data.clone());
            return builtins::call_native(self, name, data, Value::Undefined, args, f.clone());
        }
        if let Kind::Bound(target, _, bound) = &self.obj(id).kind {
            let target = target.clone();
            let mut all = bound.clone();
            all.extend(args);
            return self.construct(&target, all);
        }
        let proto = match self.get(f, "prototype")? {
            Value::Obj(p) => p,
            _ => self.protos.object,
        };
        let (class, parent) = match &self.obj(id).kind {
            Kind::Function(c) => (c.class.clone(), c.parent.clone()),
            _ => (None, None),
        };
        // Türetilmiş sınıf: nesne üst sınıfın kurucusunda oluşur (super()).
        let this = if parent.is_some() && class.is_some() {
            Value::Undefined
        } else {
            let o = self.alloc(Some(proto), Kind::Plain);
            if let Some((c, env)) = &class {
                self.init_fields(c, *env, &Value::Obj(o))?;
            }
            Value::Obj(o)
        };
        self.depth += 1;
        if self.depth > 400 {
            self.depth -= 1;
            throw!(self, RangeError, "en büyük çağrı yığını boyutu aşıldı");
        }
        let result = self.construct_call(id, this, args, f.clone(), proto);
        self.depth -= 1;
        result
    }

    fn construct_call(&mut self, id: ObjId, this: Value, args: Vec<Value>, new_target: Value, proto: ObjId) -> R<Value> {
        // Kurucu gövdesi; `this` super() ile ayarlanabilir, bu yüzden kapsamdan okunur.
        let (f, env) = match &self.obj(id).kind {
            Kind::Function(c) => (c.f.clone(), c.env),
            _ => throw!(self, TypeError, "değer bir kurucu değil"),
        };
        let scope = self.new_env(Some(env), Some(this));
        self.envs[scope as usize].func = Some(id);
        self.envs[scope as usize].new_target = new_target.clone();
        self.capture(scope);
        let args_obj = self.new_array(args.clone());
        self.declare(scope, "arguments", args_obj, false);
        for (i, (p, default)) in f.params.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or_default();
            if v == Value::Undefined
                && let Some(d) = default
            {
                v = self.eval(d, scope)?;
            }
            self.bind(p, v, scope, Some(LET))?;
        }
        if let Some(rest) = &f.rest {
            let tail = args.get(f.params.len()..).map(|s| s.to_vec()).unwrap_or_default();
            let arr = self.new_array(tail);
            self.bind(rest, arr, scope, Some(LET))?;
        }
        // Açık kurucusu olmayan türetilmiş sınıf: argümanları üst sınıfa geçir.
        let implicit_super = f.body.is_empty()
            && matches!(&self.obj(id).kind, Kind::Function(c) if c.parent.is_some());
        if implicit_super {
            self.super_call(scope, args)?;
        }
        self.hoist(&f.body, scope, true);
        let r = self.run_body(&f, scope)?;
        let this = self.envs[scope as usize].this.clone().unwrap_or_default();
        let _ = proto;
        if let Value::Obj(_) = r {
            return Ok(r);
        }
        Ok(this)
    }

    fn init_fields(&mut self, c: &Rc<Class>, env: EnvId, this: &Value) -> R<()> {
        for (key, init, is_static) in &c.fields {
            if *is_static {
                continue;
            }
            let fenv = self.new_env(Some(env), Some(this.clone()));
            let k = match key {
                PropKey::Name(n) => n.clone(),
                PropKey::Computed(e) => {
                    let v = self.eval(e, fenv)?;
                    self.key_of(&v)?
                }
            };
            let v = match init {
                Some(e) => self.eval(e, fenv)?,
                None => Value::Undefined,
            };
            self.put(this, &k, v)?;
        }
        Ok(())
    }

    fn super_call(&mut self, env: EnvId, args: Vec<Value>) -> R<Value> {
        let Some(fenv) = self.func_env(env) else {
            throw!(self, SyntaxError, "super() yalnızca kurucuda kullanılabilir");
        };
        let func = self.envs[fenv as usize].func.unwrap();
        let new_target = self.envs[fenv as usize].new_target.clone();
        let (parent, class) = match &self.obj(func).kind {
            Kind::Function(c) => (c.parent.clone(), c.class.clone()),
            _ => (None, None),
        };
        let Some(parent) = parent else {
            throw!(self, SyntaxError, "üst sınıf yok");
        };
        let proto = match self.get(&new_target, "prototype")? {
            Value::Obj(p) => p,
            _ => self.protos.object,
        };
        let this = match &parent {
            Value::Obj(p) if matches!(self.obj(*p).kind, Kind::Function(_)) => {
                let (pclass, pparent) = match &self.obj(*p).kind {
                    Kind::Function(c) => (c.class.clone(), c.parent.clone()),
                    _ => (None, None),
                };
                let initial = if pparent.is_some() && pclass.is_some() {
                    Value::Undefined
                } else {
                    let o = self.alloc(Some(proto), Kind::Plain);
                    if let Some((c, e)) = &pclass {
                        self.init_fields(c, *e, &Value::Obj(o))?;
                    }
                    Value::Obj(o)
                };
                self.construct_call(*p, initial, args, new_target.clone(), proto)?
            }
            _ => {
                // Yerleşik üst sınıf (Error, Array, HTMLElement...).
                let v = self.construct(&parent, args)?;
                if let Value::Obj(o) = v {
                    self.obj_mut(o).proto = Some(proto);
                }
                v
            }
        };
        self.envs[fenv as usize].this = Some(this.clone());
        if let Some((c, cenv)) = class {
            self.init_fields(&c, cenv, &this)?;
        }
        Ok(this)
    }

    fn home_object(&self, env: EnvId) -> Option<ObjId> {
        let mut cur = Some(env);
        while let Some(id) = cur {
            let e = &self.envs[id as usize];
            if let Some(f) = e.func
                && let Kind::Function(c) = &self.obj(f).kind
            {
                return c.home;
            }
            cur = e.parent;
        }
        None
    }

    fn call_args(&mut self, args: &[Expr], env: EnvId) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            if let Expr::Spread(inner) = a {
                let v = self.eval(inner, env)?;
                out.extend(self.iterate(&v)?);
            } else {
                out.push(self.eval(a, env)?);
            }
        }
        Ok(out)
    }

    /// İfade değerlendirme.
    pub fn eval(&mut self, e: &Expr, env: EnvId) -> R<Value> {
        self.tick()?;
        Ok(match e {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Template(parts, exprs) => {
                let mut out = String::new();
                for (i, part) in parts.iter().enumerate() {
                    out.push_str(part);
                    if let Some(ex) = exprs.get(i) {
                        let v = self.eval(ex, env)?;
                        out.push_str(&self.to_str(&v)?);
                    }
                }
                Value::str(&out)
            }
            Expr::Tagged(tag, parts, exprs) => {
                let f = self.eval(tag, env)?;
                let strings: Vec<Value> = parts.iter().map(|p| Value::str(p)).collect();
                let raw = self.new_array(strings.clone());
                let arr = self.new_array(strings);
                if let Value::Obj(a) = &arr {
                    self.set_prop(*a, "raw", raw);
                }
                let mut args = alloc::vec![arr];
                for ex in exprs {
                    args.push(self.eval(ex, env)?);
                }
                self.call(&f, Value::Undefined, args)?
            }
            Expr::Regex(p, f) => match regex::Regex::new(p, f) {
                Ok(re) => {
                    let proto = self.protos.regexp;
                    Value::Obj(self.alloc(Some(proto), Kind::RegExp(alloc::boxed::Box::new(re), 0)))
                }
                Err(msg) => throw!(self, SyntaxError, "{}", msg),
            },
            Expr::Ident(n) => match &**n {
                "undefined" => Value::Undefined,
                "null" => Value::Null,
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => self.lookup(env, n)?,
            },
            Expr::This => {
                let t = self.this_of(env);
                if t == Value::Undefined && self.func_env(env).is_none() {
                    Value::Obj(self.global)
                } else {
                    t
                }
            }
            Expr::NewTarget => self
                .func_env(env)
                .map(|f| self.envs[f as usize].new_target.clone())
                .unwrap_or_default(),
            Expr::Super => {
                // `super.x`: ev nesnesinin prototipi.
                match self.home_object(env).and_then(|h| self.obj(h).proto) {
                    Some(p) => Value::Obj(p),
                    None => Value::Undefined,
                }
            }
            Expr::Array(items) => {
                let mut out = Vec::new();
                for item in items {
                    match item {
                        None => out.push(Value::Undefined),
                        Some(Expr::Spread(inner)) => {
                            let v = self.eval(inner, env)?;
                            out.extend(self.iterate(&v)?);
                        }
                        Some(e) => out.push(self.eval(e, env)?),
                    }
                }
                self.new_array(out)
            }
            Expr::Object(props) => {
                let o = self.new_object();
                for p in props {
                    match p {
                        PropExpr::Value(key, value) => {
                            let k = self.prop_key(key, env)?;
                            let v = self.eval(value, env)?;
                            if let (Value::Obj(f), Expr::Function(_)) = (&v, value)
                                && let Kind::Function(c) = &mut self.obj_mut(*f).kind
                            {
                                c.home = Some(o);
                            }
                            if &*k == "__proto__" {
                                if let Value::Obj(p) = v {
                                    self.obj_mut(o).proto = Some(p);
                                } else if v == Value::Null {
                                    self.obj_mut(o).proto = None;
                                }
                                continue;
                            }
                            self.set_prop(o, &k, v);
                        }
                        PropExpr::Getter(key, f) => {
                            let k = self.prop_key(key, env)?;
                            let g = self.make_closure(f.clone(), env, Some(o));
                            self.define_method(o, &k, g, 1);
                            self.make_enumerable(o, &k);
                        }
                        PropExpr::Setter(key, f) => {
                            let k = self.prop_key(key, env)?;
                            let s = self.make_closure(f.clone(), env, Some(o));
                            self.define_method(o, &k, s, 2);
                            self.make_enumerable(o, &k);
                        }
                        PropExpr::Spread(e) => {
                            let v = self.eval(e, env)?;
                            self.copy_props(o, &v)?;
                        }
                    }
                }
                Value::Obj(o)
            }
            Expr::Function(f) => self.make_closure(f.clone(), env, None),
            Expr::Class(c) => self.make_class(c, env)?,
            Expr::Unary(op, inner) => {
                if *op == "typeof" {
                    if let Expr::Ident(n) = &**inner
                        && !matches!(&**n, "null" | "true" | "false" | "undefined")
                    {
                        let v = self.lookup(env, n).unwrap_or(Value::Undefined);
                        return Ok(Value::str(self.type_of(&v)));
                    }
                    let v = self.eval(inner, env)?;
                    return Ok(Value::str(self.type_of(&v)));
                }
                if *op == "delete" {
                    match &**inner {
                        Expr::Field(o, name, _) => {
                            let ov = self.eval(o, env)?;
                            self.delete_prop(&ov, name);
                        }
                        Expr::Member(o, k, _) => {
                            let ov = self.eval(o, env)?;
                            let kv = self.eval(k, env)?;
                            let k = self.key_of(&kv)?;
                            self.delete_prop(&ov, &k);
                        }
                        _ => {}
                    }
                    return Ok(Value::Bool(true));
                }
                let v = self.eval(inner, env)?;
                match *op {
                    "!" => Value::Bool(!self.truthy(&v)),
                    "-" => Value::Num(-self.to_number(&v)?),
                    "+" => Value::Num(self.to_number(&v)?),
                    "~" => Value::Num(!(to_i32(self.to_number(&v)?)) as f64),
                    "void" => Value::Undefined,
                    _ => Value::Undefined,
                }
            }
            Expr::Update(op, prefix, target) => {
                let old = self.eval(target, env)?;
                let n = self.to_number(&old)?;
                let new = if *op == "++" { n + 1.0 } else { n - 1.0 };
                self.assign_to(target, Value::Num(new), env)?;
                Value::Num(if *prefix { new } else { n })
            }
            Expr::Binary(op, a, b) => {
                let a = self.eval(a, env)?;
                let b = self.eval(b, env)?;
                self.binary(op, &a, &b)?
            }
            Expr::Logical(op, a, b) => {
                let a = self.eval(a, env)?;
                let take_a = match *op {
                    "&&" => !self.truthy(&a),
                    "||" => self.truthy(&a),
                    _ => !matches!(a, Value::Undefined | Value::Null),
                };
                if take_a { a } else { self.eval(b, env)? }
            }
            Expr::Assign(op, target, value) => {
                if *op == "=" {
                    let v = self.eval(value, env)?;
                    self.name_function(&v, target);
                    self.bind(target, v.clone(), env, None)?;
                    return Ok(v);
                }
                let Pattern::Expr(target) = &**target else {
                    throw!(self, SyntaxError, "geçersiz atama");
                };
                let old = self.eval(target, env)?;
                let v = match *op {
                    "&&=" => {
                        if !self.truthy(&old) {
                            return Ok(old);
                        }
                        self.eval(value, env)?
                    }
                    "||=" => {
                        if self.truthy(&old) {
                            return Ok(old);
                        }
                        self.eval(value, env)?
                    }
                    "??=" => {
                        if !matches!(old, Value::Undefined | Value::Null) {
                            return Ok(old);
                        }
                        self.eval(value, env)?
                    }
                    _ => {
                        let r = self.eval(value, env)?;
                        self.binary(&op[..op.len() - 1], &old, &r)?
                    }
                };
                self.assign_to(target, v.clone(), env)?;
                v
            }
            Expr::Conditional(c, a, b) => {
                let c = self.eval(c, env)?;
                if self.truthy(&c) {
                    self.eval(a, env)?
                } else {
                    self.eval(b, env)?
                }
            }
            Expr::Sequence(list) => {
                let mut last = Value::Undefined;
                for e in list {
                    last = self.eval(e, env)?;
                }
                last
            }
            Expr::Field(o, name, optional) => {
                let ov = if let Expr::Super = **o {
                    let p = self.eval(o, env)?;
                    let this = self.this_of(env);
                    return self.get_with_receiver(&p, name, this);
                } else {
                    self.eval(o, env)?
                };
                if *optional && matches!(ov, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                self.get(&ov, name)?
            }
            Expr::Member(o, k, optional) => {
                let ov = self.eval(o, env)?;
                if *optional && matches!(ov, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let kv = self.eval(k, env)?;
                if let (Value::Obj(id), Value::Num(n)) = (&ov, &kv)
                    && let Kind::Array(items) = &self.obj(*id).kind
                    && *n >= 0.0
                {
                    return Ok(items.get(*n as usize).cloned().unwrap_or_default());
                }
                let key = self.key_of(&kv)?;
                self.get(&ov, &key)?
            }
            Expr::Call(callee, args, optional) => return self.eval_call(callee, args, *optional, env),
            Expr::New(callee, args) => {
                let f = self.eval(callee, env)?;
                let a = self.call_args(args, env)?;
                if !self.is_callable(&f) {
                    throw!(self, TypeError, "değer bir kurucu değil");
                }
                self.construct(&f, a)?
            }
            Expr::Spread(_) => throw!(self, SyntaxError, "beklenmeyen ..."),
            Expr::Await(inner) => {
                let v = self.eval(inner, env)?;
                self.await_value(v)?
            }
            Expr::Yield(_) => throw!(self, SyntaxError, "yield desteklenmiyor"),
        })
    }

    fn make_enumerable(&mut self, o: ObjId, key: &str) {
        if let Some(slot) = self.obj_mut(o).props.iter_mut().find(|p| &*p.key == key) {
            slot.enumerable = true;
        }
    }

    fn get_with_receiver(&mut self, holder: &Value, key: &str, receiver: Value) -> R<Value> {
        let mut cursor = match holder {
            Value::Obj(o) => Some(*o),
            _ => None,
        };
        while let Some(id) = cursor {
            if let Some(slot) = self.own(id, key) {
                if let Some(g) = slot.getter.clone() {
                    return self.call(&g, receiver, Vec::new());
                }
                return Ok(slot.value.clone());
            }
            cursor = self.obj(id).proto;
        }
        Ok(Value::Undefined)
    }

    fn delete_prop(&mut self, target: &Value, key: &str) {
        if let Value::Obj(o) = target {
            let o = *o;
            if let Kind::Array(items) = &mut self.obj_mut(o).kind
                && let Some(i) = index(key)
            {
                if i < items.len() {
                    items[i] = Value::Undefined;
                }
                return;
            }
            if let Kind::Storage(_) | Kind::Dataset(_) = self.obj(o).kind {
                let _ = dom::delete(self, o, key);
                return;
            }
            self.obj_mut(o).props.retain(|p| &*p.key != key);
        }
    }

    pub fn copy_props(&mut self, to: ObjId, from: &Value) -> R<()> {
        match from {
            Value::Obj(src) => {
                let src = *src;
                for k in self.own_keys(src) {
                    let v = self.get(from, &k)?;
                    self.set_prop(to, &k, v);
                }
            }
            Value::Str(s) => {
                for (i, c) in s.chars().enumerate() {
                    self.set_prop(to, &alloc::format!("{}", i), Value::str(c.encode_utf8(&mut [0; 4])));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn prop_key(&mut self, key: &PropKey, env: EnvId) -> R<Rc<str>> {
        Ok(match key {
            PropKey::Name(n) => n.clone(),
            PropKey::Computed(e) => {
                let v = self.eval(e, env)?;
                self.key_of(&v)?
            }
        })
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Expr], optional: bool, env: EnvId) -> R<Value> {
        // super(...) çağrısı.
        if let Expr::Super = callee {
            let a = self.call_args(args, env)?;
            return self.super_call(env, a);
        }
        let (this, f) = match callee {
            Expr::Field(o, name, opt) => {
                if let Expr::Super = **o {
                    let holder = self.eval(o, env)?;
                    let this = self.this_of(env);
                    let f = self.get_with_receiver(&holder, name, this.clone())?;
                    (this, f)
                } else {
                    let ov = self.eval(o, env)?;
                    if *opt && matches!(ov, Value::Undefined | Value::Null) {
                        return Ok(Value::Undefined);
                    }
                    let f = self.get(&ov, name)?;
                    if !self.is_callable(&f) && !optional {
                        throw!(self, TypeError, "{}.{} bir fonksiyon değil", describe(o), name);
                    }
                    (ov, f)
                }
            }
            Expr::Member(o, k, opt) => {
                let ov = self.eval(o, env)?;
                if *opt && matches!(ov, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let kv = self.eval(k, env)?;
                let key = self.key_of(&kv)?;
                let f = self.get(&ov, &key)?;
                (ov, f)
            }
            _ => {
                let f = self.eval(callee, env)?;
                if !self.is_callable(&f) && !optional {
                    throw!(self, TypeError, "{} bir fonksiyon değil", describe(callee));
                }
                (Value::Undefined, f)
            }
        };
        if optional && matches!(f, Value::Undefined | Value::Null) {
            return Ok(Value::Undefined);
        }
        let a = self.call_args(args, env)?;
        self.call(&f, this, a)
    }

    pub fn binary(&mut self, op: &str, a: &Value, b: &Value) -> R<Value> {
        Ok(match op {
            "+" => {
                let pa = self.to_primitive(a)?;
                let pb = self.to_primitive(b)?;
                if matches!(pa, Value::Str(_)) || matches!(pb, Value::Str(_)) {
                    let mut s = String::from(&*self.to_str(&pa)?);
                    s.push_str(&self.to_str(&pb)?);
                    Value::str(&s)
                } else {
                    Value::Num(self.to_number(&pa)? + self.to_number(&pb)?)
                }
            }
            "-" => Value::Num(self.to_number(a)? - self.to_number(b)?),
            "*" => Value::Num(self.to_number(a)? * self.to_number(b)?),
            "/" => Value::Num(self.to_number(a)? / self.to_number(b)?),
            "%" => {
                let x = self.to_number(a)?;
                let y = self.to_number(b)?;
                Value::Num(if y == 0.0 || x.is_infinite() { f64::NAN } else { x - y * fmath::trunc(x / y) })
            }
            "**" => {
                let x = self.to_number(a)?;
                let y = self.to_number(b)?;
                Value::Num(fmath::pow(x, y))
            }
            "==" => Value::Bool(self.loose_equal(a, b)?),
            "!=" => Value::Bool(!self.loose_equal(a, b)?),
            "===" => Value::Bool(self.strict_equal(a, b)),
            "!==" => Value::Bool(!self.strict_equal(a, b)),
            "<" | ">" | "<=" | ">=" => {
                let pa = self.to_primitive(a)?;
                let pb = self.to_primitive(b)?;
                let ord = if let (Value::Str(x), Value::Str(y)) = (&pa, &pb) {
                    Some(x.cmp(y))
                } else {
                    let x = self.to_number(&pa)?;
                    let y = self.to_number(&pb)?;
                    x.partial_cmp(&y)
                };
                let Some(ord) = ord else { return Ok(Value::Bool(false)) };
                Value::Bool(match op {
                    "<" => ord.is_lt(),
                    ">" => ord.is_gt(),
                    "<=" => ord.is_le(),
                    _ => ord.is_ge(),
                })
            }
            "&" => Value::Num((to_i32(self.to_number(a)?) & to_i32(self.to_number(b)?)) as f64),
            "|" => Value::Num((to_i32(self.to_number(a)?) | to_i32(self.to_number(b)?)) as f64),
            "^" => Value::Num((to_i32(self.to_number(a)?) ^ to_i32(self.to_number(b)?)) as f64),
            "<<" => Value::Num(to_i32(self.to_number(a)?).wrapping_shl(to_i32(self.to_number(b)?) as u32 & 31) as f64),
            ">>" => Value::Num((to_i32(self.to_number(a)?) >> (to_i32(self.to_number(b)?) as u32 & 31)) as f64),
            ">>>" => Value::Num(((to_i32(self.to_number(a)?) as u32) >> (to_i32(self.to_number(b)?) as u32 & 31)) as f64),
            "instanceof" => {
                if !self.is_callable(b) {
                    throw!(self, TypeError, "instanceof'un sağı bir fonksiyon değil");
                }
                let Value::Obj(o) = a else { return Ok(Value::Bool(false)) };
                let proto = self.get(b, "prototype")?;
                let mut cursor = self.obj(*o).proto;
                while let Some(p) = cursor {
                    if Value::Obj(p) == proto {
                        return Ok(Value::Bool(true));
                    }
                    cursor = self.obj(p).proto;
                }
                Value::Bool(false)
            }
            "in" => {
                let key = self.key_of(a)?;
                let Value::Obj(o) = b else {
                    throw!(self, TypeError, "'in' nesne gerektirir");
                };
                let mut cursor = Some(*o);
                let mut found = false;
                while let Some(id) = cursor {
                    if self.own(id, &key).is_some() {
                        found = true;
                        break;
                    }
                    if let Kind::Array(items) = &self.obj(id).kind
                        && (index(&key).is_some_and(|i| i < items.len()) || &*key == "length")
                    {
                        found = true;
                        break;
                    }
                    cursor = self.obj(id).proto;
                }
                if !found
                    && let Some(v) = self.special_get(*o, &key)?
                {
                    found = v != Value::Undefined;
                }
                Value::Bool(found)
            }
            _ => throw!(self, SyntaxError, "bilinmeyen işleç {}", op),
        })
    }

    // ---- sözler (Promise) ----

    pub fn new_promise(&mut self) -> ObjId {
        let p = self.protos.promise;
        self.alloc(Some(p), Kind::Promise(PromiseState::Pending(Vec::new())))
    }

    pub fn resolve_promise(&mut self, p: ObjId, v: Value) -> R<()> {
        // Thenable ile çözülürse onu bekle.
        if let Value::Obj(o) = &v {
            if *o == p {
                let e = self.make_error("TypeError", "söz kendisiyle çözülemez");
                self.reject_promise(p, e);
                return Ok(());
            }
            if let Kind::Promise(_) = self.obj(*o).kind {
                let resolve = self.native_with("__resolve", Value::Obj(p));
                let reject = self.native_with("__reject", Value::Obj(p));
                self.promise_then(*o, resolve, reject);
                return Ok(());
            }
            let then = self.get(&v, "then")?;
            if self.is_callable(&then) {
                let resolve = self.native_with("__resolve", Value::Obj(p));
                let reject = self.native_with("__reject", Value::Obj(p));
                if let Err(e) = self.call(&then, v.clone(), alloc::vec![resolve, reject]) {
                    self.reject_promise(p, e);
                }
                return Ok(());
            }
        }
        self.settle(p, Ok(v));
        Ok(())
    }

    pub fn reject_promise(&mut self, p: ObjId, e: Value) {
        self.settle(p, Err(e));
    }

    fn settle(&mut self, p: ObjId, result: Result<Value, Value>) {
        let reactions = match &mut self.obj_mut(p).kind {
            Kind::Promise(state @ PromiseState::Pending(_)) => {
                let new = match &result {
                    Ok(v) => PromiseState::Fulfilled(v.clone()),
                    Err(e) => PromiseState::Rejected(e.clone()),
                };
                match core::mem::replace(state, new) {
                    PromiseState::Pending(r) => r,
                    _ => Vec::new(),
                }
            }
            _ => return,
        };
        for (on_ok, on_err, derived) in reactions {
            self.schedule_reaction(&result, on_ok, on_err, derived);
        }
    }

    fn schedule_reaction(&mut self, result: &Result<Value, Value>, on_ok: Value, on_err: Value, derived: ObjId) {
        let job = self.native_with("__reaction", Value::Obj(derived));
        let (handler, value, ok) = match result {
            Ok(v) => (on_ok, v.clone(), true),
            Err(e) => (on_err, e.clone(), false),
        };
        self.microtasks.push_back((job, alloc::vec![handler, value, Value::Bool(ok)]));
    }

    pub fn promise_then(&mut self, p: ObjId, on_ok: Value, on_err: Value) -> ObjId {
        let derived = self.new_promise();
        let state = match &mut self.obj_mut(p).kind {
            Kind::Promise(PromiseState::Pending(list)) => {
                list.push((on_ok, on_err, derived));
                return derived;
            }
            Kind::Promise(PromiseState::Fulfilled(v)) => Ok(v.clone()),
            Kind::Promise(PromiseState::Rejected(e)) => Err(e.clone()),
            _ => return derived,
        };
        self.schedule_reaction(&state, on_ok, on_err, derived);
        derived
    }

    pub fn run_microtasks(&mut self) {
        let mut guard = 0;
        while let Some((job, args)) = self.microtasks.pop_front() {
            guard += 1;
            if guard > 100_000 {
                self.microtasks.clear();
                break;
            }
            if let Err(e) = self.call(&job, Value::Undefined, args) {
                let text = self.error_text(&e);
                self.host.console.push((2, alloc::format!("Yakalanmamış (söz) {}", text)));
            }
        }
    }

    /// `await`: söz bekliyorsa zamanlayıcı ve mikro görevleri iç içe çalıştırır.
    fn await_value(&mut self, v: Value) -> R<Value> {
        let Value::Obj(o) = &v else { return Ok(v) };
        let o = *o;
        let promise = match &self.obj(o).kind {
            Kind::Promise(_) => o,
            _ => {
                let then = self.get(&v, "then")?;
                if !self.is_callable(&then) {
                    return Ok(v);
                }
                let p = self.new_promise();
                self.resolve_promise(p, v)?;
                p
            }
        };
        let start = (self.now_ms)();
        loop {
            match &self.obj(promise).kind {
                Kind::Promise(PromiseState::Fulfilled(v)) => return Ok(v.clone()),
                Kind::Promise(PromiseState::Rejected(e)) => return Err(e.clone()),
                _ => {}
            }
            if !self.microtasks.is_empty() {
                self.run_microtasks();
                continue;
            }
            let now = (self.now_ms)();
            if now - start > 15_000 || self.timers.is_empty() {
                throw!(self, Error, "await edilen söz sonuçlanmadı");
            }
            self.run_due_timers(now, true);
        }
    }

    /// Süresi gelen zamanlayıcıları çalıştırır; `wait` ise en yakınına kadar bekler.
    pub fn run_due_timers(&mut self, now: u64, wait: bool) {
        if wait
            && let Some(next) = self.timers.iter().map(|t| t.due).min()
            && next > now
        {
            let ms = (next - now).min(100);
            crate::time::sleep_ms(ms);
        }
        let now = (self.now_ms)();
        let due: Vec<u32> = self.timers.iter().filter(|t| t.due <= now).map(|t| t.id).collect();
        for id in due {
            let Some(i) = self.timers.iter().position(|t| t.id == id) else { continue };
            let (callback, args) = (self.timers[i].callback.clone(), self.timers[i].args.clone());
            match self.timers[i].interval {
                Some(every) => self.timers[i].due = now + every.max(4),
                None => {
                    self.timers.remove(i);
                }
            }
            self.steps = 0;
            if let Err(e) = self.call(&callback, Value::Undefined, args) {
                let text = self.error_text(&e);
                self.host.console.push((2, alloc::format!("Yakalanmamış {}", text)));
            }
            self.run_microtasks();
        }
    }

    // ---- çöp toplama ----

    /// Görevler arasında çağrılır: erişilemeyen nesneleri ve kapsamları serbest bırakır.
    pub fn maybe_gc(&mut self) {
        let live = self.objs.len() - self.free.len();
        if live < self.gc_threshold {
            return;
        }
        let mut stack: Vec<Value> = Vec::new();
        let mut env_stack: Vec<EnvId> = alloc::vec![self.global_env];
        stack.push(Value::Obj(self.global));
        let protos = [
            self.protos.object, self.protos.function, self.protos.array, self.protos.string,
            self.protos.number, self.protos.boolean, self.protos.error, self.protos.promise,
            self.protos.regexp, self.protos.date, self.protos.map, self.protos.set,
            self.protos.node, self.protos.event, self.protos.type_error, self.protos.range_error,
            self.protos.syntax_error, self.protos.reference_error,
        ];
        stack.extend(protos.iter().map(|&p| Value::Obj(p)));
        stack.extend(self.node_objs.values().map(|&o| Value::Obj(o)));
        for t in &self.timers {
            stack.push(t.callback.clone());
            stack.extend(t.args.iter().cloned());
        }
        for l in &self.listeners {
            stack.push(l.callback.clone());
        }
        for (job, args) in &self.microtasks {
            stack.push(job.clone());
            stack.extend(args.iter().cloned());
        }
        for o in self.objs.iter_mut().flatten() {
            o.marked = false;
        }
        for e in self.envs.iter_mut() {
            e.marked = false;
        }
        loop {
            while let Some(v) = stack.pop() {
                let Value::Obj(id) = v else { continue };
                let Some(o) = self.objs.get_mut(id as usize).and_then(|o| o.as_mut()) else {
                    continue;
                };
                if o.marked {
                    continue;
                }
                o.marked = true;
                if let Some(p) = o.proto {
                    stack.push(Value::Obj(p));
                }
                for slot in &o.props {
                    stack.push(slot.value.clone());
                    if let Some(g) = &slot.getter {
                        stack.push(g.clone());
                    }
                    if let Some(s) = &slot.setter {
                        stack.push(s.clone());
                    }
                }
                match &o.kind {
                    Kind::Array(items) | Kind::Set(items) => stack.extend(items.iter().cloned()),
                    Kind::Map(entries) => {
                        for (k, v) in entries {
                            stack.push(k.clone());
                            stack.push(v.clone());
                        }
                    }
                    Kind::Function(c) => {
                        env_stack.push(c.env);
                        if let Some(t) = &c.this {
                            stack.push(t.clone());
                        }
                        if let Some(h) = c.home {
                            stack.push(Value::Obj(h));
                        }
                        if let Some(p) = &c.parent {
                            stack.push(p.clone());
                        }
                        if let Some((_, e)) = &c.class {
                            env_stack.push(*e);
                        }
                    }
                    Kind::Native(_, d) | Kind::Primitive(d) => stack.push(d.clone()),
                    Kind::Bound(a, b, args) => {
                        stack.push(a.clone());
                        stack.push(b.clone());
                        stack.extend(args.iter().cloned());
                    }
                    Kind::Promise(PromiseState::Pending(r)) => {
                        for (a, b, d) in r {
                            stack.push(a.clone());
                            stack.push(b.clone());
                            stack.push(Value::Obj(*d));
                        }
                    }
                    Kind::Promise(PromiseState::Fulfilled(v) | PromiseState::Rejected(v)) => stack.push(v.clone()),
                    _ => {}
                }
            }
            let Some(env) = env_stack.pop() else { break };
            let e = &mut self.envs[env as usize];
            if e.marked {
                continue;
            }
            e.marked = true;
            for (_, v, _) in &e.vars {
                stack.push(v.clone());
            }
            if let Some(t) = &e.this {
                stack.push(t.clone());
            }
            stack.push(e.new_target.clone());
            if let Some(f) = e.func {
                stack.push(Value::Obj(f));
            }
            if let Some(p) = e.parent {
                env_stack.push(p);
            }
        }
        for (i, slot) in self.objs.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|o| !o.marked) {
                *slot = None;
                self.free.push(i as ObjId);
            }
        }
        for (i, e) in self.envs.iter_mut().enumerate() {
            if e.alive && !e.marked && i as EnvId != self.global_env {
                e.alive = false;
                e.vars.clear();
                self.free_envs.push(i as EnvId);
            }
        }
        let live = self.objs.len() - self.free.len();
        self.gc_threshold = (live * 2).max(50_000);
    }
}

pub fn index(key: &str) -> Option<usize> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse().ok()
}

pub fn to_i32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let t = fmath::trunc(n);
    let m = t - 4294967296.0 * fmath::floor(t / 4294967296.0);
    m as u32 as i32
}

fn describe(e: &Expr) -> String {
    match e {
        Expr::Ident(n) => String::from(&**n),
        Expr::Field(o, n, _) => alloc::format!("{}.{}", describe(o), n),
        Expr::Call(c, ..) => alloc::format!("{}(...)", describe(c)),
        _ => String::from("ifade"),
    }
}

fn pattern_names(p: &Pattern, out: &mut Vec<Name>) {
    match p {
        Pattern::Ident(n) => out.push(n.clone()),
        Pattern::Object(props, rest) => {
            for (_, p, _) in props {
                pattern_names(p, out);
            }
            if let Some(r) = rest {
                out.push(r.clone());
            }
        }
        Pattern::Array(items, rest) => {
            for (p, _) in items.iter().flatten() {
                pattern_names(p, out);
            }
            if let Some(r) = rest {
                pattern_names(r, out);
            }
        }
        Pattern::Expr(_) => {}
    }
}

/// Gövdede fonksiyon ifadesi var mı (döngü başına kapsam gerekip gerekmediği).
fn contains_closure(s: &Stmt) -> bool {
    fn expr(e: &Expr) -> bool {
        match e {
            Expr::Function(_) | Expr::Class(_) => true,
            Expr::Template(_, xs) | Expr::Sequence(xs) => xs.iter().any(expr),
            Expr::Tagged(a, _, xs) => expr(a) || xs.iter().any(expr),
            Expr::Array(xs) => xs.iter().flatten().any(expr),
            Expr::Object(ps) => ps.iter().any(|p| match p {
                PropExpr::Value(_, v) | PropExpr::Spread(v) => expr(v),
                _ => true,
            }),
            Expr::Unary(_, a) | Expr::Update(_, _, a) | Expr::Spread(a) | Expr::Await(a) => expr(a),
            Expr::Binary(_, a, b) | Expr::Logical(_, a, b) | Expr::Member(a, b, _) => expr(a) || expr(b),
            Expr::Assign(_, _, v) => expr(v),
            Expr::Conditional(a, b, c) => expr(a) || expr(b) || expr(c),
            Expr::Call(c, args, _) | Expr::New(c, args) => expr(c) || args.iter().any(expr),
            Expr::Field(o, ..) => expr(o),
            _ => false,
        }
    }
    match s {
        Stmt::Expr(e) | Stmt::Throw(e) => expr(e),
        Stmt::Return(e) => e.as_ref().is_some_and(expr),
        Stmt::Var(_, ds) => ds.iter().any(|(_, i)| i.as_ref().is_some_and(expr)),
        Stmt::Block(b) => b.iter().any(contains_closure),
        Stmt::If(c, a, b) => expr(c) || contains_closure(a) || b.as_deref().is_some_and(contains_closure),
        Stmt::For(i, t, u, b) => {
            i.as_deref().is_some_and(contains_closure)
                || t.as_ref().is_some_and(expr)
                || u.as_ref().is_some_and(expr)
                || contains_closure(b)
        }
        Stmt::ForIn(_, _, e, b, _) => expr(e) || contains_closure(b),
        Stmt::While(c, b) | Stmt::DoWhile(b, c) => expr(c) || contains_closure(b),
        Stmt::Try(a, c, f) => {
            a.iter().any(contains_closure)
                || c.as_ref().is_some_and(|(_, b)| b.iter().any(contains_closure))
                || f.as_ref().is_some_and(|b| b.iter().any(contains_closure))
        }
        Stmt::Switch(e, cases) => expr(e) || cases.iter().any(|(_, b)| b.iter().any(contains_closure)),
        Stmt::Labeled(_, b) => contains_closure(b),
        Stmt::Function(_) | Stmt::Class(_) => true,
        _ => false,
    }
}
