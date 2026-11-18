//! `.banna` sanal makinesi: yığın tabanlı bayt kodu yorumlayıcısı ve standart kitaplık
//! (String, Vec, HashMap/HashSet, Option/Result, sayılar, karakterler, yineleyiciler,
//! dosya ve konsol). G/Ç `Host` üzerinden yapılır (Banna OS ya da ana makine testleri).

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::cmp::Ordering;

use super::code::{BIN_OPS, Const, Op, Program, TypeKind};
use super::compiler::{OPTION, ORDERING, RESULT};

pub trait Host {
    fn write(&mut self, text: &str, stderr: bool);
    fn read_line(&mut self) -> Option<String>;
    fn args(&self) -> Vec<String>;
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String>;
    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), String>;
    fn sleep_ms(&mut self, ms: u64);
    fn now_ms(&self) -> u64;
    fn random(&mut self) -> u64;
}

#[derive(Clone, Debug)]
pub enum V {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(Rc<RefCell<String>>),
    List(Rc<RefCell<Vec<V>>>),
    Tuple(Rc<RefCell<Vec<V>>>),
    Struct(u32, Rc<RefCell<Vec<V>>>),
    Enum(u32, u32, Rc<RefCell<Vec<V>>>),
    /// Eşleme; `set` doğruysa HashSet.
    Map(Rc<RefCell<Vec<(V, V)>>>, bool),
    Ref(Rc<RefCell<V>>),
    Range(i64, i64),
    Func(u32, Rc<Vec<V>>),
    Iter(Rc<RefCell<(Vec<V>, usize)>>),
    Stdin,
    /// `map.entry(anahtar)`.
    Entry(Rc<RefCell<Vec<(V, V)>>>, Rc<V>),
}

pub enum Trap {
    Panic(String),
    Exit(i32),
}

type R<T> = Result<T, Trap>;

fn s(text: &str) -> V {
    V::Str(Rc::new(RefCell::new(String::from(text))))
}

fn list(items: Vec<V>) -> V {
    V::List(Rc::new(RefCell::new(items)))
}

fn tuple(items: Vec<V>) -> V {
    V::Tuple(Rc::new(RefCell::new(items)))
}

pub fn some(v: V) -> V {
    V::Enum(OPTION, 1, Rc::new(RefCell::new(alloc::vec![v])))
}

pub fn none() -> V {
    V::Enum(OPTION, 0, Rc::new(RefCell::new(Vec::new())))
}

fn ok(v: V) -> V {
    V::Enum(RESULT, 0, Rc::new(RefCell::new(alloc::vec![v])))
}

fn err(v: V) -> V {
    V::Enum(RESULT, 1, Rc::new(RefCell::new(alloc::vec![v])))
}

fn ordering(o: Ordering) -> V {
    let i = match o {
        Ordering::Less => 0,
        Ordering::Equal => 1,
        Ordering::Greater => 2,
    };
    V::Enum(ORDERING, i, Rc::new(RefCell::new(Vec::new())))
}

fn opt(o: Option<V>) -> V {
    o.map_or_else(none, some)
}

/// Başvuruları çözer.
fn deref(v: &V) -> V {
    match v {
        V::Ref(r) => deref(&r.borrow()),
        other => other.clone(),
    }
}

fn panic<T>(msg: &str) -> R<T> {
    Err(Trap::Panic(String::from(msg)))
}

pub struct Vm<'h> {
    pub prog: Program,
    pub host: &'h mut dyn Host,
    depth: u32,
    line: u32,
    func: u32,
}

impl<'h> Vm<'h> {
    pub fn new(prog: Program, host: &'h mut dyn Host) -> Self {
        Self {
            prog,
            host,
            depth: 0,
            line: 0,
            func: 0,
        }
    }

    /// `main`'i çalıştırır; çıkış kodu ya da panik iletisi.
    pub fn run(&mut self) -> Result<i32, String> {
        let main = self.prog.main;
        match self.call(main, Vec::new()) {
            Ok(_) => Ok(0),
            Err(Trap::Exit(code)) => Ok(code),
            Err(Trap::Panic(msg)) => {
                let name = self.prog.funcs.get(self.func as usize).map(|f| f.name.clone()).unwrap_or_default();
                Err(alloc::format!("'{}' içinde panik ({}. satır): {}", name, self.line, msg))
            }
        }
    }

    fn type_name(&self, t: u32) -> &str {
        &self.prog.types[t as usize].name
    }

    fn is_copy(&self, t: u32) -> bool {
        match &self.prog.types[t as usize].kind {
            TypeKind::Struct { copy, .. } | TypeKind::Enum { copy, .. } => *copy,
        }
    }

    fn field_names(&self, v: &V) -> Vec<String> {
        match v {
            V::Struct(t, _) => match &self.prog.types[*t as usize].kind {
                TypeKind::Struct { fields, .. } => fields.clone(),
                _ => Vec::new(),
            },
            V::Enum(t, var, _) => match &self.prog.types[*t as usize].kind {
                TypeKind::Enum { variants, .. } => variants[*var as usize].1.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Derin kopya (`.clone()`).
    pub fn clone_deep(&self, v: &V) -> V {
        match v {
            V::Str(x) => s(&x.borrow()),
            V::List(x) => list(x.borrow().iter().map(|i| self.clone_deep(i)).collect()),
            V::Tuple(x) => tuple(x.borrow().iter().map(|i| self.clone_deep(i)).collect()),
            V::Struct(t, x) => V::Struct(*t, Rc::new(RefCell::new(x.borrow().iter().map(|i| self.clone_deep(i)).collect()))),
            V::Enum(t, var, x) => V::Enum(*t, *var, Rc::new(RefCell::new(x.borrow().iter().map(|i| self.clone_deep(i)).collect()))),
            V::Map(x, set) => V::Map(Rc::new(RefCell::new(x.borrow().iter().map(|(k, v)| (self.clone_deep(k), self.clone_deep(v))).collect())), *set),
            V::Ref(r) => self.clone_deep(&r.borrow()),
            other => other.clone(),
        }
    }

    /// `Copy` yapıları atamada kopyalanır.
    fn copy_if_copy(&self, v: V) -> V {
        match &v {
            V::Struct(t, _) | V::Enum(t, _, _) if *t > ORDERING && self.is_copy(*t) => self.clone_deep(&v),
            _ => v,
        }
    }

    // ---- karşılaştırma ve biçim ----

    pub fn eq(&self, a: &V, b: &V) -> bool {
        match (a, b) {
            (V::Ref(x), _) => self.eq(&x.borrow(), b),
            (_, V::Ref(y)) => self.eq(a, &y.borrow()),
            (V::Unit, V::Unit) => true,
            (V::Int(x), V::Int(y)) => x == y,
            (V::Float(x), V::Float(y)) => x == y,
            (V::Int(x), V::Float(y)) | (V::Float(y), V::Int(x)) => *x as f64 == *y,
            (V::Bool(x), V::Bool(y)) => x == y,
            (V::Char(x), V::Char(y)) => x == y,
            (V::Str(x), V::Str(y)) => *x.borrow() == *y.borrow(),
            (V::List(x), V::List(y)) | (V::Tuple(x), V::Tuple(y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| self.eq(a, b))
            }
            (V::Struct(t1, x), V::Struct(t2, y)) => t1 == t2 && {
                let (x, y) = (x.borrow(), y.borrow());
                x.iter().zip(y.iter()).all(|(a, b)| self.eq(a, b))
            },
            (V::Enum(t1, v1, x), V::Enum(t2, v2, y)) => t1 == t2 && v1 == v2 && {
                let (x, y) = (x.borrow(), y.borrow());
                x.iter().zip(y.iter()).all(|(a, b)| self.eq(a, b))
            },
            (V::Range(a1, b1), V::Range(a2, b2)) => a1 == a2 && b1 == b2,
            (V::Map(x, _), V::Map(y, _)) => {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| self.eq(k, k2) && self.eq(v, v2)))
            }
            _ => false,
        }
    }

    pub fn cmp(&self, a: &V, b: &V) -> Ordering {
        match (a, b) {
            (V::Ref(x), _) => self.cmp(&x.borrow(), b),
            (_, V::Ref(y)) => self.cmp(a, &y.borrow()),
            (V::Int(x), V::Int(y)) => x.cmp(y),
            (V::Float(x), V::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
            (V::Int(x), V::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
            (V::Float(x), V::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
            (V::Bool(x), V::Bool(y)) => x.cmp(y),
            (V::Char(x), V::Char(y)) => x.cmp(y),
            (V::Str(x), V::Str(y)) => x.borrow().as_str().cmp(y.borrow().as_str()),
            (V::List(x), V::List(y)) | (V::Tuple(x), V::Tuple(y)) | (V::Struct(_, x), V::Struct(_, y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                for (a, b) in x.iter().zip(y.iter()) {
                    let o = self.cmp(a, b);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                x.len().cmp(&y.len())
            }
            (V::Enum(_, v1, x), V::Enum(_, v2, y)) => v1.cmp(v2).then_with(|| {
                let (x, y) = (x.borrow(), y.borrow());
                for (a, b) in x.iter().zip(y.iter()) {
                    let o = self.cmp(a, b);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            }),
            _ => Ordering::Equal,
        }
    }

    pub fn show(&self, v: &V, debug: bool, out: &mut String) {
        match v {
            V::Unit => out.push_str("()"),
            V::Int(i) => out.push_str(&alloc::format!("{}", i)),
            V::Float(f) => {
                if debug && *f == libm_trunc(*f) && f.abs() < 1e16 {
                    out.push_str(&alloc::format!("{:.1}", f));
                } else {
                    out.push_str(&alloc::format!("{}", f));
                }
            }
            V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            V::Char(c) => {
                if debug {
                    out.push('\'');
                    out.push(*c);
                    out.push('\'');
                } else {
                    out.push(*c);
                }
            }
            V::Str(x) => {
                if debug {
                    out.push('"');
                    for c in x.borrow().chars() {
                        match c {
                            '"' => out.push_str("\\\""),
                            '\n' => out.push_str("\\n"),
                            '\\' => out.push_str("\\\\"),
                            c => out.push(c),
                        }
                    }
                    out.push('"');
                } else {
                    out.push_str(&x.borrow());
                }
            }
            V::List(x) => {
                out.push('[');
                for (i, item) in x.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.show(item, true, out);
                }
                out.push(']');
            }
            V::Tuple(x) => {
                out.push('(');
                let items = x.borrow();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.show(item, true, out);
                }
                if items.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            V::Struct(t, x) | V::Enum(t, _, x) => {
                let names = self.field_names(v);
                let label = match v {
                    V::Enum(t, var, _) => match &self.prog.types[*t as usize].kind {
                        TypeKind::Enum { variants, .. } => variants[*var as usize].0.clone(),
                        _ => String::new(),
                    },
                    _ => String::from(self.type_name(*t)),
                };
                out.push_str(&label);
                let items = x.borrow();
                if items.is_empty() {
                    return;
                }
                let named = names.first().is_some_and(|n| !n.starts_with(|c: char| c.is_ascii_digit()));
                if named {
                    out.push_str(" { ");
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&names[i]);
                        out.push_str(": ");
                        self.show(item, true, out);
                    }
                    out.push_str(" }");
                } else {
                    out.push('(');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        self.show(item, true, out);
                    }
                    out.push(')');
                }
            }
            V::Map(x, set) => {
                out.push('{');
                for (i, (k, val)) in x.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.show(k, true, out);
                    if !set {
                        out.push_str(": ");
                        self.show(val, true, out);
                    }
                }
                out.push('}');
            }
            V::Ref(r) => self.show(&r.borrow(), debug, out),
            V::Range(a, b) => out.push_str(&alloc::format!("{}..{}", a, b)),
            V::Func(..) => out.push_str("<fn>"),
            V::Iter(_) => out.push_str("<yineleyici>"),
            V::Entry(..) => out.push_str("<entry>"),
            V::Stdin => out.push_str("Stdin"),
        }
    }

    fn display(&self, v: &V) -> String {
        let mut out = String::new();
        self.show(v, false, &mut out);
        out
    }

    fn format(&self, fmt: &str, args: &[V]) -> R<String> {
        let mut out = String::new();
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0;
        let mut next = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '{' && chars.get(i + 1) == Some(&'{') {
                out.push('{');
                i += 2;
                continue;
            }
            if c == '}' && chars.get(i + 1) == Some(&'}') {
                out.push('}');
                i += 2;
                continue;
            }
            if c != '{' {
                out.push(c);
                i += 1;
                continue;
            }
            let end = chars[i..].iter().position(|&x| x == '}').map_or(chars.len(), |p| i + p);
            let spec: String = chars[i + 1..end].iter().collect();
            i = end + 1;
            let spec = spec.split_once(':').map(|(_, s)| s).unwrap_or("");
            let Some(arg) = args.get(next) else {
                return panic("biçim için yeterli argüman yok");
            };
            next += 1;
            let arg = deref(arg);
            // Biçim: [[dolgu]hiza][+][#][0][genişlik][.kesinlik][tür]
            let sc: Vec<char> = spec.chars().collect();
            let mut k = 0;
            let mut fill = ' ';
            let mut align = None;
            if sc.len() >= 2 && matches!(sc[1], '<' | '>' | '^') {
                fill = sc[0];
                align = Some(sc[1]);
                k = 2;
            } else if sc.first().is_some_and(|c| matches!(c, '<' | '>' | '^')) {
                align = Some(sc[0]);
                k = 1;
            }
            let plus = sc.get(k) == Some(&'+');
            if plus {
                k += 1;
            }
            if sc.get(k) == Some(&'#') {
                k += 1;
            }
            let zero = sc.get(k) == Some(&'0');
            if zero {
                k += 1;
            }
            let mut width = 0usize;
            while k < sc.len() && sc[k].is_ascii_digit() {
                width = width * 10 + sc[k].to_digit(10).unwrap() as usize;
                k += 1;
            }
            let mut precision = None;
            if sc.get(k) == Some(&'.') {
                k += 1;
                let mut p = 0;
                while k < sc.len() && sc[k].is_ascii_digit() {
                    p = p * 10 + sc[k].to_digit(10).unwrap() as usize;
                    k += 1;
                }
                precision = Some(p);
            }
            let kind: String = sc[k..].iter().collect();
            let mut text = match (kind.as_str(), &arg) {
                ("x", V::Int(n)) => alloc::format!("{:x}", n),
                ("X", V::Int(n)) => alloc::format!("{:X}", n),
                ("b", V::Int(n)) => alloc::format!("{:b}", n),
                ("o", V::Int(n)) => alloc::format!("{:o}", n),
                (_, V::Float(f)) if precision.is_some() => alloc::format!("{:.*}", precision.unwrap(), f),
                (_, V::Int(n)) if precision.is_some() => alloc::format!("{:.*}", precision.unwrap(), *n as f64),
                (k, _) if k.contains('?') => {
                    let mut t = String::new();
                    self.show(&arg, true, &mut t);
                    t
                }
                _ => {
                    let t = self.display(&arg);
                    match (precision, &arg) {
                        (Some(p), V::Str(_)) => t.chars().take(p).collect(),
                        _ => t,
                    }
                }
            };
            if plus && matches!(arg, V::Int(n) if n >= 0) || plus && matches!(arg, V::Float(f) if f >= 0.0) {
                text.insert(0, '+');
            }
            let len = text.chars().count();
            if len < width {
                let pad = width - len;
                if zero && align.is_none() {
                    let (sign, digits) = if text.starts_with(['-', '+']) { text.split_at(1) } else { ("", text.as_str()) };
                    text = alloc::format!("{}{}{}", sign, "0".repeat(pad), digits);
                } else {
                    let numeric = matches!(arg, V::Int(_) | V::Float(_));
                    let fill_s: String = core::iter::repeat_n(fill, pad).collect();
                    text = match align.unwrap_or(if numeric { '>' } else { '<' }) {
                        '>' => alloc::format!("{}{}", fill_s, text),
                        '^' => {
                            let left: String = core::iter::repeat_n(fill, pad / 2).collect();
                            let right: String = core::iter::repeat_n(fill, pad - pad / 2).collect();
                            alloc::format!("{}{}{}", left, text, right)
                        }
                        _ => alloc::format!("{}{}", text, fill_s),
                    };
                }
            }
            out.push_str(&text);
        }
        Ok(out)
    }

    // ---- yürütme ----

    pub fn call(&mut self, f: u32, args: Vec<V>) -> R<V> {
        self.depth += 1;
        if self.depth > 300 {
            self.depth -= 1;
            return panic("yığın taştı (çok derin özyineleme)");
        }
        let saved = (self.func, self.line);
        self.func = f;
        let r = self.exec(f, args);
        if r.is_ok() {
            (self.func, self.line) = saved;
        }
        self.depth -= 1;
        r
    }

    fn call_value(&mut self, callee: &V, mut args: Vec<V>) -> R<V> {
        match deref(callee) {
            V::Func(f, captures) => {
                let mut all: Vec<V> = captures.iter().cloned().collect();
                all.append(&mut args);
                self.call(f, all)
            }
            _ => panic("çağrılabilir bir değer değil"),
        }
    }

    fn exec(&mut self, fi: u32, args: Vec<V>) -> R<V> {
        let func = &self.prog.funcs[fi as usize];
        let nlocals = func.locals as usize;
        let mut locals: Vec<V> = Vec::with_capacity(nlocals);
        locals.extend(args);
        locals.resize(nlocals.max(locals.len()), V::Unit);
        let mut stack: Vec<V> = Vec::with_capacity(16);
        let mut pc = 0usize;
        loop {
            let (ins, line) = {
                let f = &self.prog.funcs[fi as usize];
                match f.code.get(pc) {
                    Some(ins) => (*ins, f.lines[pc]),
                    None => return Ok(V::Unit),
                }
            };
            self.line = line;
            pc += 1;
            macro_rules! pop {
                () => {
                    stack.pop().unwrap_or(V::Unit)
                };
            }
            match ins.op {
                Op::Const => stack.push(match &self.prog.consts[ins.a as usize] {
                    Const::Int(v) => V::Int(*v),
                    Const::Float(v) => V::Float(*v),
                    Const::Str(t) => s(t),
                }),
                Op::Unit => stack.push(V::Unit),
                Op::True => stack.push(V::Bool(true)),
                Op::False => stack.push(V::Bool(false)),
                Op::Load => {
                    let v = locals[ins.a as usize].clone();
                    stack.push(if ins.b == 1 { v } else { self.copy_if_copy(v) });
                }
                Op::Store => {
                    let v = pop!();
                    locals[ins.a as usize] = v;
                }
                Op::LoadBox => {
                    let v = match &locals[ins.a as usize] {
                        V::Ref(r) => r.borrow().clone(),
                        other => other.clone(),
                    };
                    stack.push(v);
                }
                Op::StoreBox => {
                    let v = pop!();
                    match &locals[ins.a as usize] {
                        V::Ref(r) => *r.borrow_mut() = v,
                        _ => locals[ins.a as usize] = v,
                    }
                }
                Op::BoxLocal => {
                    let v = core::mem::replace(&mut locals[ins.a as usize], V::Unit);
                    locals[ins.a as usize] = match v {
                        V::Ref(r) => V::Ref(r),
                        other => V::Ref(Rc::new(RefCell::new(other))),
                    };
                }
                Op::RefLocal => stack.push(locals[ins.a as usize].clone()),
                Op::Deref => {
                    let v = pop!();
                    stack.push(deref(&v));
                }
                Op::StoreDeref => {
                    let v = pop!();
                    let target = pop!();
                    match target {
                        V::Ref(r) => *r.borrow_mut() = v,
                        _ => return panic("*x = ... için değiştirilebilir başvuru gerekir (&mut)"),
                    }
                }
                Op::Pop => {
                    stack.pop();
                }
                Op::Dup => {
                    let v = stack.last().cloned().unwrap_or(V::Unit);
                    stack.push(v);
                }
                Op::Swap => {
                    let n = stack.len();
                    if n >= 2 {
                        stack.swap(n - 1, n - 2);
                    }
                }
                Op::Bin => {
                    let b = pop!();
                    let a = pop!();
                    stack.push(self.binary(BIN_OPS[ins.a as usize], a, b)?);
                }
                Op::Neg => {
                    let v = deref(&pop!());
                    stack.push(match v {
                        V::Int(i) => V::Int(i.checked_neg().ok_or_else(|| Trap::Panic(String::from("taşma")))?),
                        V::Float(f) => V::Float(-f),
                        _ => return panic("sayı bekleniyordu"),
                    });
                }
                Op::Not => {
                    let v = deref(&pop!());
                    stack.push(match v {
                        V::Bool(b) => V::Bool(!b),
                        V::Int(i) => V::Int(!i),
                        _ => return panic("bool bekleniyordu"),
                    });
                }
                Op::Jmp => pc = ins.a as usize,
                Op::Jz | Op::Jnz => {
                    let v = deref(&pop!());
                    let t = matches!(v, V::Bool(true));
                    if t == (ins.op == Op::Jnz) {
                        pc = ins.a as usize;
                    }
                }
                Op::Call => {
                    let n = ins.b as usize;
                    let args = take_n(&mut stack, n)?;
                    let r = self.call(ins.a, args)?;
                    stack.push(r);
                }
                Op::CallValue => {
                    let n = ins.a as usize;
                    let args = take_n(&mut stack, n)?;
                    let callee = pop!();
                    let r = self.call_value(&callee, args)?;
                    stack.push(r);
                }
                Op::Ret => return Ok(pop!()),
                Op::Builtin => {
                    let n = ins.b as usize;
                    let args = take_n(&mut stack, n)?;
                    let name = match &self.prog.consts[ins.a as usize] {
                        Const::Str(t) => t.clone(),
                        _ => String::new(),
                    };
                    let r = self.builtin(&name, args)?;
                    stack.push(r);
                }
                Op::Method => {
                    let n = ins.b as usize;
                    let args = take_n(&mut stack, n)?;
                    let recv = pop!();
                    let name = match &self.prog.consts[ins.a as usize] {
                        Const::Str(t) => t.clone(),
                        _ => String::new(),
                    };
                    let r = self.method(recv, &name, args)?;
                    stack.push(r);
                }
                Op::Field => {
                    let obj = deref(&pop!());
                    let name = self.const_str(ins.a);
                    let v = self.field(&obj, &name)?;
                    stack.push(if ins.b == 1 { v } else { self.copy_if_copy(v) });
                }
                Op::SetField => {
                    let v = pop!();
                    let obj = deref(&pop!());
                    let name = self.const_str(ins.a);
                    self.set_field(&obj, &name, v)?;
                }
                Op::Index => {
                    let idx = deref(&pop!());
                    let obj = deref(&pop!());
                    let v = self.index(&obj, &idx)?;
                    stack.push(if ins.a == 1 { v } else { self.copy_if_copy(v) });
                }
                Op::SetIndex => {
                    let v = pop!();
                    let idx = deref(&pop!());
                    let obj = deref(&pop!());
                    match (&obj, &idx) {
                        (V::List(l), V::Int(i)) => {
                            let mut l = l.borrow_mut();
                            let len = l.len();
                            match l.get_mut(*i as usize).filter(|_| *i >= 0) {
                                Some(slot) => *slot = v,
                                None => return panic(&alloc::format!("dizin sınır dışı: uzunluk {} ama dizin {}", len, i)),
                            }
                        }
                        (V::Map(m, _), _) => {
                            let mut m = m.borrow_mut();
                            match m.iter_mut().find(|(k, _)| self.eq(k, &idx)) {
                                Some(slot) => slot.1 = v,
                                None => return panic("eşlemede anahtar yok"),
                            }
                        }
                        _ => return panic("dizinle atama desteklenmiyor"),
                    }
                }
                Op::Struct => {
                    let n = ins.b as usize;
                    let fields = take_n(&mut stack, n)?;
                    stack.push(V::Struct(ins.a, Rc::new(RefCell::new(fields))));
                }
                Op::Variant => {
                    let n = (ins.b & 0xff) as usize;
                    let fields = take_n(&mut stack, n)?;
                    stack.push(V::Enum(ins.a, ins.b >> 8, Rc::new(RefCell::new(fields))));
                }
                Op::MakeVec => {
                    let n = ins.a as usize;
                    let items = take_n(&mut stack, n)?;
                    stack.push(list(items));
                }
                Op::Tuple => {
                    let n = ins.a as usize;
                    let items = take_n(&mut stack, n)?;
                    stack.push(tuple(items));
                }
                Op::Range => {
                    let hi = if ins.a & 4 != 0 { Some(deref(&pop!())) } else { None };
                    let lo = if ins.a & 2 != 0 { Some(deref(&pop!())) } else { None };
                    let lo = match lo {
                        Some(V::Int(v)) => v,
                        Some(V::Char(c)) => c as i64,
                        None => 0,
                        _ => return panic("aralık tamsayı ister"),
                    };
                    let hi = match hi {
                        Some(V::Int(v)) => v + (ins.a & 1) as i64,
                        Some(V::Char(c)) => c as i64 + (ins.a & 1) as i64,
                        None => i64::MAX,
                        _ => return panic("aralık tamsayı ister"),
                    };
                    stack.push(V::Range(lo, hi));
                }
                Op::Repeat => {
                    let n = match deref(&pop!()) {
                        V::Int(n) if n >= 0 => n as usize,
                        _ => return panic("tekrar sayısı negatif olamaz"),
                    };
                    let v = pop!();
                    let items = (0..n).map(|_| self.clone_deep(&v)).collect();
                    stack.push(list(items));
                }
                Op::Format => {
                    let n = ins.b as usize;
                    let mut args = take_n(&mut stack, n)?;
                    let fmt = self.const_str(ins.a);
                    // `{}` ile biçimlenen kullanıcı türleri: kendi `fmt` yöntemleri (Display).
                    let specs = arg_specs(&fmt);
                    for (i, a) in args.iter_mut().enumerate() {
                        if specs.get(i).is_some_and(|d| !d) {
                            *a = self.user_display(a)?;
                        }
                    }
                    let text = self.format(&fmt, &args)?;
                    stack.push(s(&text));
                }
                Op::Print => {
                    let v = pop!();
                    let mut text = self.display(&v);
                    if ins.a & 1 != 0 {
                        text.push('\n');
                    }
                    self.host.write(&text, ins.a & 2 != 0);
                }
                Op::Iter => {
                    let v = pop!();
                    let it = self.iterator(&v)?;
                    stack.push(it);
                }
                Op::Next => {
                    let it = pop!();
                    let item = match &it {
                        V::Iter(state) => {
                            let mut st = state.borrow_mut();
                            let (items, pos) = &mut *st;
                            let item = items.get(*pos).cloned();
                            *pos += 1;
                            item
                        }
                        V::Range(..) => None,
                        _ => None,
                    };
                    match item {
                        Some(v) => stack.push(v),
                        None => pc = ins.a as usize,
                    }
                }
                Op::TestVariant => {
                    let v = deref(&pop!());
                    stack.push(V::Bool(matches!(v, V::Enum(t, var, _) if t == ins.a && var == ins.b)));
                }
                Op::Unpack => {
                    let v = deref(&pop!());
                    let n = ins.a as usize;
                    let items: Vec<V> = match &v {
                        V::Enum(_, _, f) | V::Tuple(f) | V::Struct(_, f) | V::List(f) => f.borrow().clone(),
                        _ => Vec::new(),
                    };
                    for i in 0..n {
                        stack.push(items.get(i).cloned().unwrap_or(V::Unit));
                    }
                }
                Op::Cast => {
                    let v = deref(&pop!());
                    let ty = self.const_str(ins.a);
                    stack.push(cast(v, &ty)?);
                }
                Op::Closure => {
                    let n = ins.b as usize;
                    let captures = take_n(&mut stack, n)?;
                    stack.push(V::Func(ins.a, Rc::new(captures)));
                }
                Op::Try => {
                    let v = deref(&pop!());
                    match &v {
                        V::Enum(OPTION, 0, _) | V::Enum(RESULT, 1, _) => return Ok(v),
                        V::Enum(OPTION, 1, f) | V::Enum(RESULT, 0, f) => stack.push(f.borrow()[0].clone()),
                        _ => return panic("? yalnızca Option ve Result ile kullanılır"),
                    }
                }
                Op::Panic => {
                    let msg = self.const_str(ins.a);
                    return panic(&msg);
                }
                Op::Line | Op::StructBase => {}
            }
        }
    }

    /// `impl Display` yazılmış yapı/enum ise `fmt` çağrılıp dizgeye çevrilir.
    fn user_display(&mut self, v: &V) -> R<V> {
        let target = deref(v);
        let Some(f) = self.user_method(&target, "fmt") else { return Ok(v.clone()) };
        let buffer = s("");
        self.call(f, alloc::vec![target, buffer.clone()])?;
        Ok(buffer)
    }

    fn const_str(&self, i: u32) -> String {
        match &self.prog.consts[i as usize] {
            Const::Str(t) => t.clone(),
            _ => String::new(),
        }
    }

    fn field(&self, obj: &V, name: &str) -> R<V> {
        match obj {
            V::Tuple(items) => {
                let i: usize = name.parse().map_err(|_| Trap::Panic(String::from("demet alanı sayı olmalı")))?;
                items.borrow().get(i).cloned().ok_or_else(|| Trap::Panic(String::from("demet alanı yok")))
            }
            V::Struct(..) | V::Enum(..) => {
                let names = self.field_names(obj);
                let i = names.iter().position(|n| n == name).ok_or_else(|| Trap::Panic(alloc::format!("'{}' alanı yok", name)))?;
                match obj {
                    V::Struct(_, f) | V::Enum(_, _, f) => Ok(f.borrow()[i].clone()),
                    _ => unreachable!(),
                }
            }
            _ => panic(&alloc::format!("'{}' alanı okunamaz", name)),
        }
    }

    fn set_field(&self, obj: &V, name: &str, v: V) -> R<()> {
        match obj {
            V::Tuple(items) => {
                let i: usize = name.parse().unwrap_or(0);
                if let Some(slot) = items.borrow_mut().get_mut(i) {
                    *slot = v;
                }
                Ok(())
            }
            V::Struct(_, f) | V::Enum(_, _, f) => {
                let names = self.field_names(obj);
                let i = names.iter().position(|n| n == name).ok_or_else(|| Trap::Panic(alloc::format!("'{}' alanı yok", name)))?;
                f.borrow_mut()[i] = v;
                Ok(())
            }
            _ => panic("alan atanamaz"),
        }
    }

    fn index(&self, obj: &V, idx: &V) -> R<V> {
        match (obj, idx) {
            (V::List(l), V::Int(i)) => {
                let l = l.borrow();
                l.get(*i as usize)
                    .filter(|_| *i >= 0)
                    .cloned()
                    .ok_or_else(|| Trap::Panic(alloc::format!("dizin sınır dışı: uzunluk {} ama dizin {}", l.len(), i)))
            }
            (V::List(l), V::Range(a, b)) => {
                let l = l.borrow();
                let end = (*b).min(l.len() as i64);
                if *a < 0 || *a > end {
                    return panic("dilim sınır dışı");
                }
                Ok(list(l[*a as usize..end as usize].to_vec()))
            }
            (V::Str(t), V::Range(a, b)) => {
                let t = t.borrow();
                let end = (*b).min(t.len() as i64) as usize;
                let a = *a as usize;
                if a > end || !t.is_char_boundary(a) || !t.is_char_boundary(end) {
                    return panic("dizge dilimi karakter sınırında değil");
                }
                Ok(s(&t[a..end]))
            }
            (V::Map(m, _), key) => m
                .borrow()
                .iter()
                .find(|(k, _)| self.eq(k, key))
                .map(|(_, v)| v.clone())
                .ok_or_else(|| Trap::Panic(String::from("eşlemede anahtar yok"))),
            _ => panic("bu değer dizinlenemez"),
        }
    }

    fn iterator(&self, v: &V) -> R<V> {
        let items = self.items(v)?;
        Ok(V::Iter(Rc::new(RefCell::new((items, 0)))))
    }

    /// Yinelenebilir değerin öğeleri.
    fn items(&self, v: &V) -> R<Vec<V>> {
        Ok(match deref(v) {
            V::List(l) => l.borrow().clone(),
            V::Range(a, b) => {
                if b == i64::MAX || b - a > 50_000_000 {
                    return panic("sınırsız aralık yinelenemez");
                }
                (a..b).map(V::Int).collect()
            }
            V::Iter(st) => {
                let st = st.borrow();
                st.0[st.1.min(st.0.len())..].to_vec()
            }
            V::Map(m, set) => m
                .borrow()
                .iter()
                .map(|(k, v)| if set { k.clone() } else { tuple(alloc::vec![k.clone(), v.clone()]) })
                .collect(),
            V::Str(t) => t.borrow().chars().map(V::Char).collect(),
            V::Enum(OPTION, 1, f) => f.borrow().clone(),
            V::Enum(OPTION, 0, _) => Vec::new(),
            V::Tuple(t) => t.borrow().clone(),
            _ => return panic("değer yinelenebilir değil"),
        })
    }

    fn binary(&self, op: &str, a: V, b: V) -> R<V> {
        let (a, b) = (deref(&a), deref(&b));
        let overflow = || Trap::Panic(String::from("aritmetik taşma"));
        Ok(match op {
            "==" => V::Bool(self.eq(&a, &b)),
            "!=" => V::Bool(!self.eq(&a, &b)),
            "<" => V::Bool(self.cmp(&a, &b) == Ordering::Less),
            ">" => V::Bool(self.cmp(&a, &b) == Ordering::Greater),
            "<=" => V::Bool(self.cmp(&a, &b) != Ordering::Greater),
            ">=" => V::Bool(self.cmp(&a, &b) != Ordering::Less),
            "&&" => V::Bool(matches!(a, V::Bool(true)) && matches!(b, V::Bool(true))),
            "||" => V::Bool(matches!(a, V::Bool(true)) || matches!(b, V::Bool(true))),
            _ => match (a, b) {
                (V::Int(x), V::Int(y)) => V::Int(match op {
                    "+" => x.checked_add(y).ok_or_else(overflow)?,
                    "-" => x.checked_sub(y).ok_or_else(overflow)?,
                    "*" => x.checked_mul(y).ok_or_else(overflow)?,
                    "/" => {
                        if y == 0 {
                            return panic("sıfıra bölme");
                        }
                        x / y
                    }
                    "%" => {
                        if y == 0 {
                            return panic("sıfıra göre kalan");
                        }
                        x % y
                    }
                    "&" => x & y,
                    "|" => x | y,
                    "^" => x ^ y,
                    "<<" => x.checked_shl(y as u32).ok_or_else(overflow)?,
                    ">>" => x >> (y & 63),
                    _ => return panic("bilinmeyen işleç"),
                }),
                (V::Bool(x), V::Bool(y)) => V::Bool(match op {
                    "&" => x & y,
                    "|" => x | y,
                    "^" => x ^ y,
                    _ => return panic("bool ile bu işlem yapılamaz"),
                }),
                (x @ (V::Float(_) | V::Int(_)), y @ (V::Float(_) | V::Int(_))) => {
                    let f = |v: &V| match v {
                        V::Float(f) => *f,
                        V::Int(i) => *i as f64,
                        _ => 0.0,
                    };
                    let (x, y) = (f(&x), f(&y));
                    V::Float(match op {
                        "+" => x + y,
                        "-" => x - y,
                        "*" => x * y,
                        "/" => x / y,
                        "%" => x - y * libm_trunc(x / y),
                        _ => return panic("ondalık sayıyla bu işlem yapılamaz"),
                    })
                }
                (V::Str(x), y) if op == "+" => {
                    let mut t = x.borrow().clone();
                    t.push_str(&self.display(&y));
                    s(&t)
                }
                _ => return panic(&alloc::format!("'{}' işleci bu türlerle kullanılamaz", op)),
            },
        })
    }

    fn int_arg(&self, args: &[V], i: usize) -> R<i64> {
        match args.get(i).map(deref) {
            Some(V::Int(v)) => Ok(v),
            Some(V::Char(c)) => Ok(c as i64),
            _ => panic("tamsayı argüman bekleniyordu"),
        }
    }

    fn str_arg(&self, args: &[V], i: usize) -> String {
        args.get(i).map(|v| self.display(&deref(v))).unwrap_or_default()
    }

    fn builtin(&mut self, name: &str, args: Vec<V>) -> R<V> {
        let short = name.rsplit("::").next().unwrap_or(name);
        Ok(match name {
            "String::new" | "String::default" => s(""),
            "String::from" | "String::from_str" => s(&self.str_arg(&args, 0)),
            "String::with_capacity" => s(""),
            "Vec::new" | "Vec::with_capacity" | "VecDeque::new" | "Vec::default" => list(Vec::new()),
            "HashMap::new" | "BTreeMap::new" | "HashMap::with_capacity" => V::Map(Rc::new(RefCell::new(Vec::new())), false),
            "HashSet::new" | "BTreeSet::new" => V::Map(Rc::new(RefCell::new(Vec::new())), true),
            "Box::new" | "Rc::new" | "RefCell::new" | "Cell::new" | "Some::from" => args.into_iter().next().unwrap_or(V::Unit),
            "process::exit" | "exit" => return Err(Trap::Exit(self.int_arg(&args, 0)? as i32)),
            "panic" => return panic(&self.str_arg(&args, 0)),
            "env::args" | "args" => list(self.host.args().iter().map(|a| s(a)).collect()),
            "io::stdin" | "stdin" => V::Stdin,
            "io::stdout" | "stdout" => V::Unit,
            "fs::read_to_string" => match self.host.read_file(&self.str_arg(&args, 0)) {
                Ok(d) => ok(s(&String::from_utf8_lossy(&d))),
                Err(e) => err(s(&e)),
            },
            "fs::write" => {
                let path = self.str_arg(&args, 0);
                let data = self.str_arg(&args, 1);
                match self.host.write_file(&path, data.as_bytes()) {
                    Ok(()) => ok(V::Unit),
                    Err(e) => err(s(&e)),
                }
            }
            "thread::sleep" | "sleep" | "banna::sleep_ms" => {
                let ms = self.int_arg(&args, 0)?;
                self.host.sleep_ms(ms.max(0) as u64);
                V::Unit
            }
            "Duration::from_millis" => V::Int(self.int_arg(&args, 0)?),
            "Duration::from_secs" => V::Int(self.int_arg(&args, 0)? * 1000),
            "Instant::now" | "banna::uptime_ms" => V::Int(self.host.now_ms() as i64),
            "banna::random" => {
                let n = self.int_arg(&args, 0)?.max(1);
                V::Int((self.host.random() % n as u64) as i64)
            }
            "banna::clear" => {
                self.host.write("\x1b[2J\x1b[H", false);
                V::Unit
            }
            "char::from_u32" => opt(char::from_u32(self.int_arg(&args, 0)? as u32).map(V::Char)),
            "char::from_digit" => {
                let d = self.int_arg(&args, 0)?;
                let radix = self.int_arg(&args, 1).unwrap_or(10);
                opt(char::from_digit(d as u32, radix as u32).map(V::Char))
            }
            "char::from" => cast(deref(&args[0]), "char")?,
            "i64::from_str_radix" | "u64::from_str_radix" | "i32::from_str_radix" | "u32::from_str_radix" => {
                let t = self.str_arg(&args, 0);
                let radix = self.int_arg(&args, 1)? as u32;
                match i64::from_str_radix(&t, radix) {
                    Ok(v) => ok(V::Int(v)),
                    Err(_) => err(s("geçersiz sayı")),
                }
            }
            "cmp::max" | "max" => {
                let (a, b) = (deref(&args[0]), deref(&args[1]));
                if self.cmp(&a, &b) == Ordering::Less { b } else { a }
            }
            "cmp::min" | "min" => {
                let (a, b) = (deref(&args[0]), deref(&args[1]));
                if self.cmp(&a, &b) == Ordering::Greater { b } else { a }
            }
            "mem::swap" | "swap" => {
                if let (V::Ref(a), V::Ref(b)) = (&args[0], &args[1]) {
                    let t = a.borrow().clone();
                    *a.borrow_mut() = b.borrow().clone();
                    *b.borrow_mut() = t;
                } else if let (V::List(a), V::List(b)) = (&args[0], &args[1]) {
                    let t = a.borrow().clone();
                    *a.borrow_mut() = b.borrow().clone();
                    *b.borrow_mut() = t;
                }
                V::Unit
            }
            "mem::take" | "take" => {
                if let V::Ref(r) = &args[0] {
                    let v = r.borrow().clone();
                    let empty = match &v {
                        V::Str(_) => s(""),
                        V::List(_) => list(Vec::new()),
                        V::Int(_) => V::Int(0),
                        _ => V::Unit,
                    };
                    *r.borrow_mut() = empty;
                    v
                } else {
                    args[0].clone()
                }
            }
            "drop" => V::Unit,
            "f64::sqrt" | "sqrt" => V::Float(sqrt(as_f(&args[0]))),
            "Default::default" | "default" => V::Int(0),
            _ => {
                return panic(&alloc::format!("'{}' bilinmiyor ya da desteklenmiyor ({})", name, short));
            }
        })
    }

    fn user_method(&self, v: &V, name: &str) -> Option<u32> {
        let t = match v {
            V::Struct(t, _) | V::Enum(t, _, _) => *t,
            _ => return None,
        };
        let ty = self.type_name(t);
        self.prog
            .methods
            .iter()
            .find(|(tn, m, _)| tn == ty && m == name)
            .map(|(_, _, f)| *f)
    }

    fn method(&mut self, recv: V, full: &str, args: Vec<V>) -> R<V> {
        let (name, hint) = full.split_once(':').unwrap_or((full, ""));
        let target = deref(&recv);
        if let Some(f) = self.user_method(&target, name) {
            let mut all = alloc::vec![target];
            all.extend(args);
            return self.call(f, all);
        }
        // Başvuru üzerinden ilkel değerlerin değiştirilmesi (ör. `*x += 1` yerine yöntem).
        match (&recv, name) {
            (V::Ref(r), "push_str" | "push" | "clear" | "insert" | "pop" | "truncate") if matches!(*r.borrow(), V::Int(_)) => {
                return panic("tamsayıda bu yöntem yok");
            }
            _ => {}
        }
        if name == "clone" || name == "to_owned" || name == "cloned" && !matches!(target, V::List(_) | V::Iter(_)) {
            return Ok(self.clone_deep(&target));
        }
        if matches!(name, "as_str" | "as_ref" | "borrow" | "borrow_mut" | "as_mut" | "iter" | "iter_mut" | "into_iter" | "by_ref" | "as_slice" | "to_vec" | "copied" | "cloned" | "as_bytes_mut" | "deref" | "get_mut_ref" | "lock" | "into") {
            return Ok(match (name, &target) {
                ("to_vec", _) => self.clone_deep(&target),
                ("iter" | "into_iter" | "iter_mut" | "copied" | "cloned", V::Map(..)) => list(self.items(&target)?),
                ("into" | "as_str" | "as_ref", V::Str(_)) if hint == "String" || name != "into" => target,
                _ => target,
            });
        }
        match &target {
            V::Int(_) | V::Float(_) => self.number_method(&target, name, hint, &args),
            V::Char(c) => self.char_method(*c, name, &args),
            V::Bool(b) => Ok(match name {
                "to_string" => s(if *b { "true" } else { "false" }),
                "then_some" => if *b { some(args[0].clone()) } else { none() },
                "then" => {
                    if *b {
                        some(self.call_value(&args[0], Vec::new())?)
                    } else {
                        none()
                    }
                }
                _ => return panic(&alloc::format!("bool üzerinde '{}' yöntemi yok", name)),
            }),
            V::Str(t) => self.string_method(t.clone(), name, hint, &args),
            V::List(l) => self.list_method(l.clone(), name, hint, args),
            V::Iter(_) | V::Range(..) | V::Tuple(_) => {
                if let V::Range(a, b) = target {
                    match name {
                        "contains" => {
                            let x = self.int_arg(&args, 0)?;
                            return Ok(V::Bool(x >= a && x < b));
                        }
                        "len" => return Ok(V::Int((b - a).max(0))),
                        "start" => return Ok(V::Int(a)),
                        "end" => return Ok(V::Int(b)),
                        "rev" => {
                            let items: Vec<V> = (a..b).rev().map(V::Int).collect();
                            return Ok(list(items));
                        }
                        "step_by" => {
                            let step = self.int_arg(&args, 0)?.max(1) as usize;
                            return Ok(list((a..b).step_by(step).map(V::Int).collect()));
                        }
                        _ => {}
                    }
                }
                if let V::Iter(st) = &target
                    && name == "next"
                {
                    let mut st = st.borrow_mut();
                    let (items, pos) = &mut *st;
                    let item = items.get(*pos).cloned();
                    *pos += 1;
                    return Ok(opt(item));
                }
                let items = self.items(&target)?;
                let l = Rc::new(RefCell::new(items));
                self.list_method(l, name, hint, args)
            }
            V::Map(m, set) => self.map_method(m.clone(), *set, name, args),
            V::Entry(m, key) => {
                // Değer paylaşılan hücreye çevrilir; `*e += 1` haritadaki değeri değiştirir.
                let present = m.borrow().iter().position(|(k, _)| self.eq(k, key));
                match name {
                    "and_modify" => {
                        if let Some(i) = present {
                            let cell = self.cell_at(m, i);
                            self.call_value(&args[0], alloc::vec![cell])?;
                        }
                        Ok(target.clone())
                    }
                    "or_insert" | "or_insert_with" | "or_default" | "or_insert_with_key" => {
                        let i = match present {
                            Some(i) => i,
                            None => {
                                let value = match name {
                                    "or_insert" => args[0].clone(),
                                    "or_default" => V::Int(0),
                                    _ => self.call_value(&args[0], Vec::new())?,
                                };
                                m.borrow_mut().push(((**key).clone(), value));
                                m.borrow().len() - 1
                            }
                        };
                        Ok(self.cell_at(m, i))
                    }
                    "key" => Ok((**key).clone()),
                    _ => panic(&alloc::format!("entry üzerinde '{}' yok", name)),
                }
            }
            V::Enum(OPTION, ..) | V::Enum(RESULT, ..) => self.option_method(target.clone(), name, args),
            V::Enum(ORDERING, var, _) => Ok(match name {
                "reverse" => ordering(match var {
                    0 => Ordering::Greater,
                    2 => Ordering::Less,
                    _ => Ordering::Equal,
                }),
                "then" => if *var == 1 { args[0].clone() } else { target.clone() },
                "then_with" => if *var == 1 { self.call_value(&args[0], Vec::new())? } else { target.clone() },
                "is_eq" => V::Bool(*var == 1),
                "is_lt" => V::Bool(*var == 0),
                "is_gt" => V::Bool(*var == 2),
                _ => return panic(&alloc::format!("Ordering üzerinde '{}' yok", name)),
            }),
            V::Stdin => match name {
                "read_line" => {
                    let line = self.host.read_line();
                    match (line, args.first()) {
                        (Some(l), Some(V::Ref(r))) => {
                            let n = l.len() + 1;
                            let mut target = r.borrow_mut();
                            match &mut *target {
                                V::Str(t) => {
                                    t.borrow_mut().push_str(&l);
                                    t.borrow_mut().push('\n');
                                }
                                other => *other = s(&alloc::format!("{}\n", l)),
                            }
                            Ok(ok(V::Int(n as i64)))
                        }
                        (Some(l), Some(V::Str(t))) => {
                            t.borrow_mut().push_str(&l);
                            t.borrow_mut().push('\n');
                            Ok(ok(V::Int(l.len() as i64 + 1)))
                        }
                        (None, _) => Ok(ok(V::Int(0))),
                        _ => panic("read_line bir &mut String ister"),
                    }
                }
                "lines" => {
                    let mut out = Vec::new();
                    while let Some(l) = self.host.read_line() {
                        out.push(ok(s(&l)));
                    }
                    Ok(list(out))
                }
                "lock" => Ok(V::Stdin),
                _ => panic(&alloc::format!("Stdin üzerinde '{}' yok", name)),
            },
            V::Unit if name == "flush" => Ok(ok(V::Unit)),
            V::Struct(..) | V::Enum(..) if name == "to_string" => {
                let v = self.user_display(&target)?;
                Ok(s(&self.display(&v)))
            }
            _ => {
                let mut desc = String::new();
                self.show(&target, true, &mut desc);
                panic(&alloc::format!("'{}' yöntemi bulunamadı ({})", name, desc.chars().take(40).collect::<String>()))
            }
        }
    }

    /// Eşlemedeki değeri hücreye çevirip başvurusunu döndürür.
    fn cell_at(&self, m: &Rc<RefCell<Vec<(V, V)>>>, i: usize) -> V {
        let mut mm = m.borrow_mut();
        let slot = &mut mm[i].1;
        if let V::Ref(r) = slot {
            return V::Ref(r.clone());
        }
        let v = core::mem::replace(slot, V::Unit);
        let cell = Rc::new(RefCell::new(v));
        *slot = V::Ref(cell.clone());
        V::Ref(cell)
    }

    fn number_method(&mut self, v: &V, name: &str, hint: &str, args: &[V]) -> R<V> {
        if let V::Int(n) = *v {
            return Ok(match name {
                "abs" => V::Int(n.abs()),
                "pow" => V::Int(n.checked_pow(self.int_arg(args, 0)? as u32).ok_or_else(|| Trap::Panic(String::from("taşma")))?),
                "signum" => V::Int(n.signum()),
                "min" => V::Int(n.min(self.int_arg(args, 0)?)),
                "max" => V::Int(n.max(self.int_arg(args, 0)?)),
                "to_string" => s(&alloc::format!("{}", n)),
                "is_positive" => V::Bool(n > 0),
                "is_negative" => V::Bool(n < 0),
                "count_ones" => V::Int(n.count_ones() as i64),
                "rem_euclid" => V::Int(n.rem_euclid(self.int_arg(args, 0)?.max(1))),
                "div_euclid" => V::Int(n.div_euclid(self.int_arg(args, 0)?.max(1))),
                "checked_add" => opt(n.checked_add(self.int_arg(args, 0)?).map(V::Int)),
                "checked_sub" => opt(n.checked_sub(self.int_arg(args, 0)?).map(V::Int)),
                "checked_mul" => opt(n.checked_mul(self.int_arg(args, 0)?).map(V::Int)),
                "checked_div" => {
                    let d = self.int_arg(args, 0)?;
                    opt(n.checked_div(d).map(V::Int))
                }
                "wrapping_add" => V::Int(n.wrapping_add(self.int_arg(args, 0)?)),
                "wrapping_mul" => V::Int(n.wrapping_mul(self.int_arg(args, 0)?)),
                "saturating_sub" => V::Int(if hint == "u" { (n - self.int_arg(args, 0)?).max(0) } else { n.saturating_sub(self.int_arg(args, 0)?) }),
                "cmp" | "partial_cmp" => {
                    let o = self.cmp(v, &deref(&args[0]));
                    if name == "partial_cmp" { some(ordering(o)) } else { ordering(o) }
                }
                "is_power_of_two" => V::Bool(n > 0 && n & (n - 1) == 0),
                "clamp" => V::Int(n.clamp(self.int_arg(args, 0)?, self.int_arg(args, 1)?)),
                "sqrt" | "powf" | "powi" | "floor" | "ceil" | "round" => return self.number_method(&V::Float(n as f64), name, hint, args),
                _ => return panic(&alloc::format!("tamsayı üzerinde '{}' yöntemi yok", name)),
            });
        }
        let f = as_f(v);
        Ok(match name {
            "abs" => V::Float(f.abs()),
            "sqrt" => V::Float(sqrt(f)),
            "floor" => V::Float(floor(f)),
            "ceil" => V::Float(-floor(-f)),
            "round" => V::Float(if f >= 0.0 { floor(f + 0.5) } else { -floor(-f + 0.5) }),
            "trunc" => V::Float(libm_trunc(f)),
            "fract" => V::Float(f - libm_trunc(f)),
            "powi" => {
                let mut r = 1.0;
                let e = self.int_arg(args, 0)?;
                for _ in 0..e.abs() {
                    r *= f;
                }
                V::Float(if e < 0 { 1.0 / r } else { r })
            }
            "powf" => V::Float(crate::web::fmath::pow(f, as_f(&args[0]))),
            "exp" => V::Float(crate::web::fmath::exp(f)),
            "ln" => V::Float(crate::web::fmath::ln(f)),
            "log10" => V::Float(crate::web::fmath::log10(f)),
            "log2" => V::Float(crate::web::fmath::log2(f)),
            "sin" => V::Float(crate::web::fmath::sin(f)),
            "cos" => V::Float(crate::web::fmath::cos(f)),
            "tan" => V::Float(crate::web::fmath::tan(f)),
            "atan" => V::Float(crate::web::fmath::atan(f)),
            "atan2" => V::Float(crate::web::fmath::atan2(f, as_f(&args[0]))),
            "min" => V::Float(f.min(as_f(&args[0]))),
            "max" => V::Float(f.max(as_f(&args[0]))),
            "is_nan" => V::Bool(f.is_nan()),
            "signum" => V::Float(if f > 0.0 { 1.0 } else if f < 0.0 { -1.0 } else { 0.0 }),
            "to_string" => s(&alloc::format!("{}", f)),
            "partial_cmp" => some(ordering(self.cmp(v, &deref(&args[0])))),
            "total_cmp" => ordering(self.cmp(v, &deref(&args[0]))),
            "clamp" => V::Float(f.clamp(as_f(&args[0]), as_f(&args[1]))),
            "to_degrees" => V::Float(f * 180.0 / core::f64::consts::PI),
            "to_radians" => V::Float(f * core::f64::consts::PI / 180.0),
            "hypot" => V::Float(sqrt(f * f + as_f(&args[0]) * as_f(&args[0]))),
            _ => return panic(&alloc::format!("ondalık sayı üzerinde '{}' yöntemi yok", name)),
        })
    }

    fn char_method(&mut self, c: char, name: &str, args: &[V]) -> R<V> {
        Ok(match name {
            "is_alphabetic" => V::Bool(c.is_alphabetic()),
            "is_numeric" => V::Bool(c.is_numeric()),
            "is_alphanumeric" => V::Bool(c.is_alphanumeric()),
            "is_whitespace" => V::Bool(c.is_whitespace()),
            "is_uppercase" => V::Bool(c.is_uppercase()),
            "is_lowercase" => V::Bool(c.is_lowercase()),
            "is_ascii" => V::Bool(c.is_ascii()),
            "is_ascii_digit" => V::Bool(c.is_ascii_digit()),
            "is_ascii_alphabetic" => V::Bool(c.is_ascii_alphabetic()),
            "is_ascii_punctuation" => V::Bool(c.is_ascii_punctuation()),
            "is_ascii_uppercase" => V::Bool(c.is_ascii_uppercase()),
            "is_ascii_lowercase" => V::Bool(c.is_ascii_lowercase()),
            "is_digit" => V::Bool(c.is_digit(self.int_arg(args, 0).unwrap_or(10) as u32)),
            "to_digit" => opt(c.to_digit(self.int_arg(args, 0).unwrap_or(10) as u32).map(|d| V::Int(d as i64))),
            "to_ascii_uppercase" => V::Char(c.to_ascii_uppercase()),
            "to_ascii_lowercase" => V::Char(c.to_ascii_lowercase()),
            "to_uppercase" => s(&c.to_uppercase().collect::<String>()),
            "to_lowercase" => s(&c.to_lowercase().collect::<String>()),
            "to_string" => s(c.encode_utf8(&mut [0; 4])),
            "len_utf8" => V::Int(c.len_utf8() as i64),
            "eq_ignore_ascii_case" => V::Bool(matches!(deref(&args[0]), V::Char(d) if d.eq_ignore_ascii_case(&c))),
            "cmp" => ordering(self.cmp(&V::Char(c), &deref(&args[0]))),
            _ => return panic(&alloc::format!("char üzerinde '{}' yöntemi yok", name)),
        })
    }

    fn string_method(&mut self, t: Rc<RefCell<String>>, name: &str, hint: &str, args: &[V]) -> R<V> {
        let text = t.borrow().clone();
        Ok(match name {
            "len" => V::Int(text.len() as i64),
            "is_empty" => V::Bool(text.is_empty()),
            "push_str" | "push" => {
                let add = self.str_arg(args, 0);
                t.borrow_mut().push_str(&add);
                V::Unit
            }
            "pop" => opt(t.borrow_mut().pop().map(V::Char)),
            "clear" => {
                t.borrow_mut().clear();
                V::Unit
            }
            "insert" | "insert_str" => {
                let at = self.int_arg(args, 0)? as usize;
                let add = self.str_arg(args, 1);
                if !text.is_char_boundary(at) {
                    return panic("karakter sınırında değil");
                }
                t.borrow_mut().insert_str(at, &add);
                V::Unit
            }
            "remove" => {
                let at = self.int_arg(args, 0)? as usize;
                if at >= text.len() || !text.is_char_boundary(at) {
                    return panic("geçersiz dizin");
                }
                V::Char(t.borrow_mut().remove(at))
            }
            "truncate" => {
                let n = self.int_arg(args, 0)? as usize;
                if n < text.len() && text.is_char_boundary(n) {
                    t.borrow_mut().truncate(n);
                }
                V::Unit
            }
            "to_string" | "to_owned" | "into" => s(&text),
            "trim" => s(text.trim()),
            "trim_start" => s(text.trim_start()),
            "trim_end" => s(text.trim_end()),
            "to_uppercase" | "to_ascii_uppercase" => s(&text.to_uppercase()),
            "to_lowercase" | "to_ascii_lowercase" => s(&text.to_lowercase()),
            "contains" => V::Bool(match deref(&args[0]) {
                V::Char(c) => text.contains(c),
                other => text.contains(self.display(&other).as_str()),
            }),
            "starts_with" => V::Bool(match deref(&args[0]) {
                V::Char(c) => text.starts_with(c),
                other => text.starts_with(self.display(&other).as_str()),
            }),
            "ends_with" => V::Bool(match deref(&args[0]) {
                V::Char(c) => text.ends_with(c),
                other => text.ends_with(self.display(&other).as_str()),
            }),
            "find" | "rfind" => {
                let needle = match deref(&args[0]) {
                    V::Char(c) => String::from(c),
                    V::Func(..) => {
                        let f = args[0].clone();
                        for (i, c) in text.char_indices() {
                            if matches!(self.call_value(&f, alloc::vec![V::Char(c)])?, V::Bool(true)) {
                                return Ok(some(V::Int(i as i64)));
                            }
                        }
                        return Ok(none());
                    }
                    other => self.display(&other),
                };
                let found = if name == "find" { text.find(needle.as_str()) } else { text.rfind(needle.as_str()) };
                opt(found.map(|i| V::Int(i as i64)))
            }
            "replace" | "replacen" => {
                let from = self.str_arg(args, 0);
                let to = self.str_arg(args, 1);
                s(&text.replace(from.as_str(), &to))
            }
            "repeat" => s(&text.repeat(self.int_arg(args, 0)?.max(0) as usize)),
            "chars" => list(text.chars().map(V::Char).collect()),
            "char_indices" => list(text.char_indices().map(|(i, c)| tuple(alloc::vec![V::Int(i as i64), V::Char(c)])).collect()),
            "bytes" | "as_bytes" => list(text.bytes().map(|b| V::Int(b as i64)).collect()),
            "lines" => list(text.lines().map(s).collect()),
            "split_whitespace" => list(text.split_whitespace().map(s).collect()),
            "split" | "split_terminator" => {
                let parts: Vec<V> = match deref(&args[0]) {
                    V::Char(c) => text.split(c).map(s).collect(),
                    other => {
                        let sep = self.display(&other);
                        text.split(sep.as_str()).map(s).collect()
                    }
                };
                list(parts)
            }
            "splitn" => {
                let n = self.int_arg(args, 0)? as usize;
                let sep = self.str_arg(args, 1);
                list(text.splitn(n, sep.as_str()).map(s).collect())
            }
            "split_once" => {
                let sep = match deref(&args[0]) {
                    V::Char(c) => String::from(c),
                    other => self.display(&other),
                };
                opt(text.split_once(sep.as_str()).map(|(a, b)| tuple(alloc::vec![s(a), s(b)])))
            }
            "strip_prefix" => opt(text.strip_prefix(self.str_arg(args, 0).as_str()).map(s)),
            "strip_suffix" => opt(text.strip_suffix(self.str_arg(args, 0).as_str()).map(s)),
            "parse" => {
                let trimmed = text.trim();
                match hint {
                    "f64" | "f32" => match trimmed.parse::<f64>() {
                        Ok(v) => ok(V::Float(v)),
                        Err(_) => err(s("geçersiz ondalık sayı")),
                    },
                    "bool" => match trimmed {
                        "true" => ok(V::Bool(true)),
                        "false" => ok(V::Bool(false)),
                        _ => err(s("geçersiz bool")),
                    },
                    "char" => match trimmed.chars().next() {
                        Some(c) if trimmed.chars().count() == 1 => ok(V::Char(c)),
                        _ => err(s("geçersiz karakter")),
                    },
                    "String" => ok(s(&text)),
                    _ => match trimmed.parse::<i64>() {
                        Ok(v) if hint.starts_with('u') && v < 0 => err(s("negatif sayı")),
                        Ok(v) => ok(V::Int(v)),
                        Err(_) => err(s("geçersiz sayı")),
                    },
                }
            }
            "cmp" => ordering(self.cmp(&s(&text), &deref(&args[0]))),
            "eq_ignore_ascii_case" => V::Bool(text.eq_ignore_ascii_case(&self.str_arg(args, 0))),
            "get" => match deref(&args[0]) {
                V::Range(a, b) => {
                    let end = (b as usize).min(text.len());
                    let a = a as usize;
                    opt((a <= end && text.is_char_boundary(a) && text.is_char_boundary(end)).then(|| s(&text[a..end])))
                }
                _ => none(),
            },
            "char_count" => V::Int(text.chars().count() as i64),
            _ => {
                // Dizgeyi karakter listesi gibi kullanan yineleyici yöntemleri.
                let items = Rc::new(RefCell::new(text.chars().map(V::Char).collect()));
                return self.list_method(items, name, hint, args.to_vec());
            }
        })
    }

    fn list_method(&mut self, l: Rc<RefCell<Vec<V>>>, name: &str, hint: &str, args: Vec<V>) -> R<V> {
        let a0 = args.first().cloned().unwrap_or(V::Unit);
        Ok(match name {
            "len" | "count" => V::Int(l.borrow().len() as i64),
            "is_empty" => V::Bool(l.borrow().is_empty()),
            "push" | "push_back" => {
                l.borrow_mut().push(a0);
                V::Unit
            }
            "push_front" => {
                l.borrow_mut().insert(0, a0);
                V::Unit
            }
            "pop" | "pop_back" => opt(l.borrow_mut().pop()),
            "pop_front" => {
                let mut v = l.borrow_mut();
                opt(if v.is_empty() { None } else { Some(v.remove(0)) })
            }
            "clear" => {
                l.borrow_mut().clear();
                V::Unit
            }
            "insert" => {
                let i = self.int_arg(&args, 0)? as usize;
                let mut v = l.borrow_mut();
                if i > v.len() {
                    return panic("insert dizini sınır dışı");
                }
                v.insert(i, args[1].clone());
                V::Unit
            }
            "remove" | "swap_remove" => {
                let i = self.int_arg(&args, 0)? as usize;
                let mut v = l.borrow_mut();
                if i >= v.len() {
                    return panic("remove dizini sınır dışı");
                }
                if name == "remove" { v.remove(i) } else { v.swap_remove(i) }
            }
            "truncate" => {
                let n = self.int_arg(&args, 0)? as usize;
                l.borrow_mut().truncate(n);
                V::Unit
            }
            "extend" | "extend_from_slice" | "append" => {
                let items = self.items(&a0)?;
                l.borrow_mut().extend(items);
                if name == "append"
                    && let V::Ref(r) = &a0
                    && let V::List(other) = &*r.borrow()
                {
                    other.borrow_mut().clear();
                }
                V::Unit
            }
            "contains" => {
                let target = deref(&a0);
                V::Bool(l.borrow().iter().any(|x| self.eq(x, &target)))
            }
            "first" | "front" => opt(l.borrow().first().cloned()),
            "last" | "back" => opt(l.borrow().last().cloned()),
            "get" => match deref(&a0) {
                V::Int(i) if i >= 0 => opt(l.borrow().get(i as usize).cloned()),
                V::Range(a, b) => {
                    let v = l.borrow();
                    let end = (b as usize).min(v.len());
                    opt((a as usize <= end).then(|| list(v[a as usize..end].to_vec())))
                }
                _ => none(),
            },
            "swap" => {
                let i = self.int_arg(&args, 0)? as usize;
                let j = self.int_arg(&args, 1)? as usize;
                let mut v = l.borrow_mut();
                if i >= v.len() || j >= v.len() {
                    return panic("swap dizini sınır dışı");
                }
                v.swap(i, j);
                V::Unit
            }
            "reverse" => {
                l.borrow_mut().reverse();
                V::Unit
            }
            "rev" => {
                let mut v = l.borrow().clone();
                v.reverse();
                list(v)
            }
            "sort" | "sort_unstable" => {
                let mut v = l.borrow().clone();
                self.sort(&mut v, None)?;
                *l.borrow_mut() = v;
                V::Unit
            }
            "sort_by" | "sort_unstable_by" | "sort_by_key" | "sort_unstable_by_key" => {
                let mut v = l.borrow().clone();
                self.sort(&mut v, Some((a0, name.ends_with("key"))))?;
                *l.borrow_mut() = v;
                V::Unit
            }
            "dedup" => {
                let mut v = l.borrow().clone();
                let mut out: Vec<V> = Vec::new();
                for x in v.drain(..) {
                    if !out.last().is_some_and(|y| self.eq(y, &x)) {
                        out.push(x);
                    }
                }
                *l.borrow_mut() = out;
                V::Unit
            }
            "retain" => {
                let v = l.borrow().clone();
                let mut out = Vec::new();
                for x in v {
                    if truthy(&self.call_value(&a0, alloc::vec![x.clone()])?) {
                        out.push(x);
                    }
                }
                *l.borrow_mut() = out;
                V::Unit
            }
            "join" | "concat" => {
                let sep = if name == "join" { self.str_arg(&args, 0) } else { String::new() };
                let parts: Vec<String> = l.borrow().iter().map(|x| self.display(x)).collect();
                s(&parts.join(&sep))
            }
            "map" | "filter" | "for_each" | "filter_map" | "flat_map" | "inspect" | "take_while" | "skip_while" | "map_while" => {
                let v = l.borrow().clone();
                let mut out = Vec::new();
                for x in v {
                    let r = self.call_value(&a0, alloc::vec![x.clone()])?;
                    match name {
                        "map" => out.push(r),
                        "filter" => {
                            if truthy(&r) {
                                out.push(x);
                            }
                        }
                        "filter_map" | "map_while" => match r {
                            V::Enum(OPTION, 1, f) => out.push(f.borrow()[0].clone()),
                            _ if name == "map_while" => break,
                            _ => {}
                        },
                        "flat_map" => out.extend(self.items(&r)?),
                        "inspect" => out.push(x),
                        "take_while" => {
                            if !truthy(&r) {
                                break;
                            }
                            out.push(x);
                        }
                        "skip_while"
                            if (!truthy(&r) || !out.is_empty()) => {
                                out.push(x);
                            }
                        _ => {}
                    }
                }
                if name == "for_each" { V::Unit } else { list(out) }
            }
            "any" | "all" | "find" | "position" | "rposition" | "find_map" => {
                let v = l.borrow().clone();
                let order: Vec<usize> = if name == "rposition" { (0..v.len()).rev().collect() } else { (0..v.len()).collect() };
                for i in order {
                    let r = self.call_value(&a0, alloc::vec![v[i].clone()])?;
                    if name == "find_map" {
                        if let V::Enum(OPTION, 1, _) = r {
                            return Ok(r);
                        }
                        continue;
                    }
                    let t = truthy(&r);
                    match name {
                        "any" if t => return Ok(V::Bool(true)),
                        "all" if !t => return Ok(V::Bool(false)),
                        "find" if t => return Ok(some(v[i].clone())),
                        "position" | "rposition" if t => return Ok(some(V::Int(i as i64))),
                        _ => {}
                    }
                }
                match name {
                    "any" => V::Bool(false),
                    "all" => V::Bool(true),
                    _ => none(),
                }
            }
            "fold" => {
                let mut acc = a0;
                let v = l.borrow().clone();
                for x in v {
                    acc = self.call_value(&args[1], alloc::vec![acc, x])?;
                }
                acc
            }
            "reduce" => {
                let v = l.borrow().clone();
                let mut it = v.into_iter();
                let Some(mut acc) = it.next() else { return Ok(none()) };
                for x in it {
                    acc = self.call_value(&a0, alloc::vec![acc, x])?;
                }
                some(acc)
            }
            "sum" | "product" => {
                let v = l.borrow().clone();
                let float = hint.starts_with('f') || v.iter().any(|x| matches!(deref(x), V::Float(_)));
                if float {
                    let mut acc = if name == "sum" { 0.0 } else { 1.0 };
                    for x in &v {
                        acc = if name == "sum" { acc + as_f(x) } else { acc * as_f(x) };
                    }
                    V::Float(acc)
                } else {
                    let mut acc: i64 = if name == "sum" { 0 } else { 1 };
                    for x in &v {
                        let n = match deref(x) {
                            V::Int(n) => n,
                            _ => return panic("toplama sayı ister"),
                        };
                        acc = if name == "sum" { acc.checked_add(n) } else { acc.checked_mul(n) }.ok_or_else(|| Trap::Panic(String::from("taşma")))?;
                    }
                    V::Int(acc)
                }
            }
            "max" | "min" => {
                let v = l.borrow().clone();
                let mut best: Option<V> = None;
                for x in v {
                    best = Some(match best {
                        None => x,
                        Some(b) => {
                            let o = self.cmp(&x, &b);
                            if (name == "max" && o != Ordering::Less) || (name == "min" && o == Ordering::Less) { x } else { b }
                        }
                    });
                }
                opt(best)
            }
            "max_by_key" | "min_by_key" | "max_by" | "min_by" => {
                let v = l.borrow().clone();
                let mut best: Option<(V, V)> = None;
                for x in v {
                    let key = if name.ends_with("key") { self.call_value(&a0, alloc::vec![x.clone()])? } else { x.clone() };
                    let better = match &best {
                        None => true,
                        Some((bk, bx)) => {
                            let o = if name.ends_with("key") {
                                self.cmp(&key, bk)
                            } else {
                                match self.call_value(&a0, alloc::vec![x.clone(), bx.clone()])? {
                                    V::Enum(ORDERING, var, _) => [Ordering::Less, Ordering::Equal, Ordering::Greater][var as usize],
                                    _ => Ordering::Equal,
                                }
                            };
                            if name.starts_with("max") { o != Ordering::Less } else { o == Ordering::Less }
                        }
                    };
                    if better {
                        best = Some((key, x));
                    }
                }
                opt(best.map(|(_, x)| x))
            }
            "enumerate" => list(l.borrow().iter().enumerate().map(|(i, x)| tuple(alloc::vec![V::Int(i as i64), x.clone()])).collect()),
            "zip" => {
                let other = self.items(&a0)?;
                list(l.borrow().iter().zip(other).map(|(a, b)| tuple(alloc::vec![a.clone(), b])).collect())
            }
            "chain" => {
                let mut v = l.borrow().clone();
                v.extend(self.items(&a0)?);
                list(v)
            }
            "take" => list(l.borrow().iter().take(self.int_arg(&args, 0)?.max(0) as usize).cloned().collect()),
            "skip" => list(l.borrow().iter().skip(self.int_arg(&args, 0)?.max(0) as usize).cloned().collect()),
            "step_by" => list(l.borrow().iter().step_by(self.int_arg(&args, 0)?.max(1) as usize).cloned().collect()),
            "last_n" => list(Vec::new()),
            "nth" => opt(l.borrow().get(self.int_arg(&args, 0)? as usize).cloned()),
            "windows" | "chunks" => {
                let n = self.int_arg(&args, 0)?.max(1) as usize;
                let v = l.borrow();
                let parts: Vec<V> = if name == "windows" {
                    v.windows(n).map(|w| list(w.to_vec())).collect()
                } else {
                    v.chunks(n).map(|w| list(w.to_vec())).collect()
                };
                list(parts)
            }
            "split_at" => {
                let i = self.int_arg(&args, 0)? as usize;
                let v = l.borrow();
                if i > v.len() {
                    return panic("split_at sınır dışı");
                }
                tuple(alloc::vec![list(v[..i].to_vec()), list(v[i..].to_vec())])
            }
            "iter" | "into_iter" | "copied" | "cloned" | "peekable" | "fuse" => list(l.borrow().clone()),
            "collect" => {
                if hint == "String" {
                    let parts: String = l.borrow().iter().map(|x| self.display(x)).collect();
                    s(&parts)
                } else if hint == "HashMap" || hint == "BTreeMap" {
                    let mut m: Vec<(V, V)> = Vec::new();
                    for item in l.borrow().iter() {
                        if let V::Tuple(p) = deref(item) {
                            let p = p.borrow();
                            let (k, v) = (p[0].clone(), p[1].clone());
                            match m.iter_mut().find(|(x, _)| self.eq(x, &k)) {
                                Some(slot) => slot.1 = v,
                                None => m.push((k, v)),
                            }
                        }
                    }
                    V::Map(Rc::new(RefCell::new(m)), false)
                } else if hint == "HashSet" || hint == "BTreeSet" {
                    let mut m: Vec<(V, V)> = Vec::new();
                    for item in l.borrow().iter() {
                        if !m.iter().any(|(x, _)| self.eq(x, item)) {
                            m.push((item.clone(), V::Unit));
                        }
                    }
                    V::Map(Rc::new(RefCell::new(m)), true)
                } else if hint == "Result" || hint == "Option" {
                    // Vec<Result<T>> → Result<Vec<T>>
                    let mut out = Vec::new();
                    for item in l.borrow().iter() {
                        match deref(item) {
                            V::Enum(RESULT, 1, _) | V::Enum(OPTION, 0, _) => return Ok(deref(item)),
                            V::Enum(_, _, f) => out.push(f.borrow()[0].clone()),
                            other => out.push(other),
                        }
                    }
                    if hint == "Result" { ok(list(out)) } else { some(list(out)) }
                } else {
                    list(l.borrow().clone())
                }
            }
            "binary_search" => {
                let target = deref(&a0);
                let v = l.borrow();
                match v.binary_search_by(|x| self.cmp(x, &target)) {
                    Ok(i) => ok(V::Int(i as i64)),
                    Err(i) => err(V::Int(i as i64)),
                }
            }
            "fill" => {
                let n = l.borrow().len();
                *l.borrow_mut() = (0..n).map(|_| self.clone_deep(&a0)).collect();
                V::Unit
            }
            "resize" => {
                let n = self.int_arg(&args, 0)? as usize;
                let fill = args[1].clone();
                let mut v = l.borrow_mut();
                while v.len() < n {
                    v.push(fill.clone());
                }
                v.truncate(n);
                V::Unit
            }
            "to_string" => {
                let parts: String = l.borrow().iter().map(|x| self.display(x)).collect();
                s(&parts)
            }
            "cmp" => ordering(self.cmp(&V::List(l.clone()), &deref(&a0))),
            "next" => {
                let mut v = l.borrow_mut();
                opt(if v.is_empty() { None } else { Some(v.remove(0)) })
            }
            "last_index" => V::Int(l.borrow().len() as i64 - 1),
            "starts_with" => {
                let other = self.items(&a0)?;
                let v = l.borrow();
                V::Bool(v.len() >= other.len() && v.iter().zip(other.iter()).all(|(a, b)| self.eq(a, b)))
            }
            "concat_strings" => V::Unit,
            "unzip" => {
                let mut a = Vec::new();
                let mut b = Vec::new();
                for item in l.borrow().iter() {
                    if let V::Tuple(p) = deref(item) {
                        let p = p.borrow();
                        a.push(p[0].clone());
                        b.push(p[1].clone());
                    }
                }
                tuple(alloc::vec![list(a), list(b)])
            }
            _ => return panic(&alloc::format!("Vec üzerinde '{}' yöntemi yok", name)),
        })
    }

    fn sort(&mut self, v: &mut [V], by: Option<(V, bool)>) -> R<()> {
        // Kararlı birleştirme sıralaması (karşılaştırıcı VM'de çalışabilir).
        if v.len() <= 1 {
            return Ok(());
        }
        let mid = v.len() / 2;
        self.sort(&mut v[..mid], by.clone())?;
        self.sort(&mut v[mid..], by.clone())?;
        let mut out = Vec::with_capacity(v.len());
        let (mut i, mut j) = (0, mid);
        while i < mid && j < v.len() {
            let o = match &by {
                None => self.cmp(&v[i], &v[j]),
                Some((f, true)) => {
                    let a = self.call_value(f, alloc::vec![v[i].clone()])?;
                    let b = self.call_value(f, alloc::vec![v[j].clone()])?;
                    self.cmp(&a, &b)
                }
                Some((f, false)) => match self.call_value(f, alloc::vec![v[i].clone(), v[j].clone()])? {
                    V::Enum(ORDERING, var, _) => [Ordering::Less, Ordering::Equal, Ordering::Greater][var as usize],
                    _ => Ordering::Equal,
                },
            };
            if o == Ordering::Greater {
                out.push(v[j].clone());
                j += 1;
            } else {
                out.push(v[i].clone());
                i += 1;
            }
        }
        out.extend_from_slice(&v[i..mid]);
        out.extend_from_slice(&v[j..]);
        v.clone_from_slice(&out);
        Ok(())
    }

    fn map_method(&mut self, m: Rc<RefCell<Vec<(V, V)>>>, set: bool, name: &str, args: Vec<V>) -> R<V> {
        let key = args.first().map(deref).unwrap_or(V::Unit);
        let pos = |vm: &Self, m: &Vec<(V, V)>| m.iter().position(|(k, _)| vm.eq(k, &key));
        Ok(match name {
            "insert" => {
                let value = if set { V::Unit } else { args.get(1).cloned().unwrap_or(V::Unit) };
                let mut mm = m.borrow_mut();
                match pos(self, &mm) {
                    Some(i) => {
                        let old = core::mem::replace(&mut mm[i].1, value);
                        if set { V::Bool(false) } else { some(old) }
                    }
                    None => {
                        mm.push((key, value));
                        if set { V::Bool(true) } else { none() }
                    }
                }
            }
            "get" | "get_mut" => {
                let mm = m.borrow();
                opt(pos(self, &mm).map(|i| mm[i].1.clone()))
            }
            "contains_key" | "contains" => V::Bool(pos(self, &m.borrow()).is_some()),
            "remove" => {
                let mut mm = m.borrow_mut();
                match pos(self, &mm) {
                    Some(i) => {
                        let (_, v) = mm.remove(i);
                        if set { V::Bool(true) } else { some(v) }
                    }
                    None => {
                        if set { V::Bool(false) } else { none() }
                    }
                }
            }
            "len" => V::Int(m.borrow().len() as i64),
            "is_empty" => V::Bool(m.borrow().is_empty()),
            "clear" => {
                m.borrow_mut().clear();
                V::Unit
            }
            "keys" => list(m.borrow().iter().map(|(k, _)| k.clone()).collect()),
            "values" | "values_mut" => list(m.borrow().iter().map(|(_, v)| v.clone()).collect()),
            "iter" | "into_iter" | "iter_mut" => list(self.items(&V::Map(m.clone(), set))?),
            "entry" => V::Entry(m.clone(), Rc::new(key)),
            "union" | "intersection" | "difference" => {
                let other = match deref(&args[0]) {
                    V::Map(o, _) => o.borrow().clone(),
                    _ => Vec::new(),
                };
                let mine = m.borrow().clone();
                let out: Vec<V> = match name {
                    "union" => {
                        let mut all: Vec<V> = mine.iter().map(|(k, _)| k.clone()).collect();
                        for (k, _) in &other {
                            if !all.iter().any(|x| self.eq(x, k)) {
                                all.push(k.clone());
                            }
                        }
                        all
                    }
                    "intersection" => mine.iter().filter(|(k, _)| other.iter().any(|(o, _)| self.eq(o, k))).map(|(k, _)| k.clone()).collect(),
                    _ => mine.iter().filter(|(k, _)| !other.iter().any(|(o, _)| self.eq(o, k))).map(|(k, _)| k.clone()).collect(),
                };
                list(out)
            }
            _ => return panic(&alloc::format!("eşleme üzerinde '{}' yöntemi yok", name)),
        })
    }

    fn option_method(&mut self, v: V, name: &str, args: Vec<V>) -> R<V> {
        let V::Enum(t, var, fields) = &v else { unreachable!() };
        let has = (*t == OPTION && *var == 1) || (*t == RESULT && *var == 0);
        let inner = fields.borrow().first().cloned().unwrap_or(V::Unit);
        let a0 = args.first().cloned().unwrap_or(V::Unit);
        Ok(match name {
            "unwrap" => {
                if has {
                    inner
                } else if *t == RESULT {
                    return panic(&alloc::format!("Err değeri üzerinde unwrap: {}", self.display(&inner)));
                } else {
                    return panic("None değeri üzerinde unwrap");
                }
            }
            "expect" => {
                if has {
                    inner
                } else {
                    return panic(&self.str_arg(&args, 0));
                }
            }
            "unwrap_or" => if has { inner } else { a0 },
            "unwrap_or_default" => {
                if has {
                    inner
                } else {
                    V::Int(0)
                }
            }
            "unwrap_or_else" => {
                if has {
                    inner
                } else {
                    let a = if *t == RESULT { alloc::vec![inner] } else { Vec::new() };
                    self.call_value(&a0, a)?
                }
            }
            "is_some" | "is_ok" => V::Bool(has),
            "is_none" | "is_err" => V::Bool(!has),
            "map" => {
                if has {
                    let r = self.call_value(&a0, alloc::vec![inner])?;
                    if *t == OPTION { some(r) } else { ok(r) }
                } else {
                    v.clone()
                }
            }
            "map_err" => {
                if *t == RESULT && !has {
                    err(self.call_value(&a0, alloc::vec![inner])?)
                } else {
                    v.clone()
                }
            }
            "and_then" => if has { self.call_value(&a0, alloc::vec![inner])? } else { v.clone() },
            "or" => if has { v.clone() } else { a0 },
            "or_else" => if has { v.clone() } else { self.call_value(&a0, Vec::new())? },
            "ok" => if has { some(inner) } else { none() },
            "err" => if *t == RESULT && !has { some(inner) } else { none() },
            "ok_or" | "ok_or_else" => {
                if has {
                    ok(inner)
                } else if name == "ok_or" {
                    err(a0)
                } else {
                    err(self.call_value(&a0, Vec::new())?)
                }
            }
            "filter" => {
                if has && truthy(&self.call_value(&a0, alloc::vec![inner])?) {
                    v.clone()
                } else {
                    none()
                }
            }
            "take" => v.clone(),
            "unwrap_or_zero" => if has { inner } else { V::Int(0) },
            "copied" | "cloned" | "as_ref" | "as_mut" | "as_deref" => v.clone(),
            "iter" => list(if has { alloc::vec![inner] } else { Vec::new() }),
            "or_insert" | "or_insert_with" | "or_default" => v.clone(),
            _ => return panic(&alloc::format!("Option/Result üzerinde '{}' yöntemi yok", name)),
        })
    }
}

/// Biçim dizgesindeki her `{}` için Debug (`?`) mu.
fn arg_specs(fmt: &str) -> Vec<bool> {
    let mut out = Vec::new();
    let mut rest = fmt;
    while let Some(i) = rest.find('{') {
        if rest[i + 1..].starts_with('{') {
            rest = &rest[i + 2..];
            continue;
        }
        let end = rest[i..].find('}').map_or(rest.len(), |e| i + e);
        out.push(rest[i..end].contains('?'));
        rest = &rest[(end + 1).min(rest.len())..];
    }
    out
}

/// Yığının üstündeki `n` değer (bayt kodu bozuksa panik, süreç çökmez).
fn take_n(stack: &mut Vec<V>, n: usize) -> R<Vec<V>> {
    if n > stack.len() {
        return panic("bozuk bayt kodu (yığın boşaldı)");
    }
    Ok(stack.split_off(stack.len() - n))
}

fn truthy(v: &V) -> bool {
    matches!(deref(v), V::Bool(true))
}

fn as_f(v: &V) -> f64 {
    match deref(v) {
        V::Float(f) => f,
        V::Int(i) => i as f64,
        _ => 0.0,
    }
}

fn libm_trunc(x: f64) -> f64 {
    crate::web::fmath::trunc(x)
}

fn floor(x: f64) -> f64 {
    crate::web::fmath::floor(x)
}

fn sqrt(x: f64) -> f64 {
    crate::web::fmath::sqrt(x)
}

fn cast(v: V, ty: &str) -> R<V> {
    Ok(match (v, ty) {
        (V::Int(i), "f64" | "f32") => V::Float(i as f64),
        (V::Float(f), "f64" | "f32") => V::Float(f),
        (V::Float(f), _) if ty != "char" => {
            let i = if f.is_nan() { 0 } else { libm_trunc(f).clamp(i64::MIN as f64, i64::MAX as f64) as i64 };
            wrap(i, ty)
        }
        (V::Int(i), "char") => V::Char(char::from_u32(i as u32).unwrap_or('\u{fffd}')),
        (V::Char(c), "char") => V::Char(c),
        (V::Char(c), _) => wrap(c as i64, ty),
        (V::Bool(b), _) => V::Int(b as i64),
        (V::Int(i), _) => wrap(i, ty),
        (other, _) => other,
    })
}

fn wrap(i: i64, ty: &str) -> V {
    V::Int(match ty {
        "u8" => i as u8 as i64,
        "i8" => i as i8 as i64,
        "u16" => i as u16 as i64,
        "i16" => i as i16 as i64,
        "u32" => i as u32 as i64,
        "i32" => i as i32 as i64,
        _ => i,
    })
}
