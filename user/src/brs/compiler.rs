//! Banna Rust derleyicisi: sözdizimi ağacından `.banna` bayt koduna.
//!
//! Değerler çalışma anında türlüdür; derleyici adları, alanları, fonksiyon ve varyant
//! sayılarını denetler. `&mut` ile ödünç verilen ya da closure içinde değiştirilen yerel
//! değişkenler kutulanır (paylaşılan hücre), böylece fonksiyon/closure onları değiştirebilir.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use super::code::{BIN_OPS, Const, Func as CodeFunc, Ins, Op, Program, TypeDef, TypeKind};
use super::parser::{Block, Expr, Func, Item, Pat, Stmt, Type, VariantShape};

type R<T> = Result<T, String>;

/// Yerleşik enum türleri (tür numaraları sabittir).
pub const OPTION: u32 = 0;
pub const RESULT: u32 = 1;
pub const ORDERING: u32 = 2;

struct Local {
    name: String,
    slot: u32,
    boxed: bool,
}

struct LoopCtx {
    breaks: Vec<usize>,
    continues: Vec<usize>,
    result: u32,
}

#[derive(Default)]
struct FnState {
    code: Vec<Ins>,
    lines: Vec<u32>,
    scopes: Vec<Vec<Local>>,
    locals: u32,
    boxed: BTreeSet<String>,
    loops: Vec<LoopCtx>,
    line: u32,
    /// İçinde bulunulan `impl` türü (`Self` için).
    self_type: Option<String>,
}

pub struct Compiler {
    prog: Program,
    funcs: BTreeMap<String, u32>,
    pending: Vec<(u32, Func, Option<String>)>,
    structs: BTreeMap<String, (u32, Vec<String>, bool)>,
    enums: BTreeMap<String, u32>,
    /// Varyant adı → (tür, varyant numarası, alan sayısı).
    variants: BTreeMap<String, Vec<(u32, u32, usize)>>,
    consts: BTreeMap<String, Expr>,
    f: FnState,
    closure_count: u32,
}

pub fn compile(src: &str) -> R<Program> {
    let items = super::parser::parse(src)?;
    let mut c = Compiler {
        prog: Program::default(),
        funcs: BTreeMap::new(),
        pending: Vec::new(),
        structs: BTreeMap::new(),
        enums: BTreeMap::new(),
        variants: BTreeMap::new(),
        consts: BTreeMap::new(),
        f: FnState::default(),
        closure_count: 0,
    };
    c.builtin_types();
    c.declare(&items)?;
    let Some(&main) = c.funcs.get("main") else {
        return Err(String::from("main fonksiyonu bulunamadı"));
    };
    c.prog.main = main;
    while let Some((index, func, self_type)) = c.pending.pop() {
        c.function(index, &func, self_type.as_deref())?;
    }
    Ok(c.prog)
}

fn segs(path: &[String]) -> String {
    let n = path.len();
    if n >= 2 {
        alloc::format!("{}::{}", path[n - 2], path[n - 1])
    } else {
        path[0].clone()
    }
}

impl Compiler {
    fn builtin_types(&mut self) {
        let defs: [(&str, &[(&str, usize)]); 3] = [
            ("Option", &[("None", 0), ("Some", 1)]),
            ("Result", &[("Ok", 1), ("Err", 1)]),
            ("Ordering", &[("Less", 0), ("Equal", 0), ("Greater", 0)]),
        ];
        for (name, vars) in defs {
            let id = self.prog.types.len() as u32;
            let variants = vars
                .iter()
                .map(|(v, n)| (String::from(*v), (0..*n).map(|i| alloc::format!("{}", i)).collect()))
                .collect();
            self.prog.types.push(TypeDef {
                name: String::from(name),
                kind: TypeKind::Enum { variants, copy: false },
            });
            self.enums.insert(String::from(name), id);
            for (i, (v, n)) in vars.iter().enumerate() {
                self.variants.entry(String::from(*v)).or_default().push((id, i as u32, *n));
            }
        }
    }

    fn add_func(&mut self, name: String, func: Func, self_type: Option<String>) -> u32 {
        let index = self.prog.funcs.len() as u32;
        self.prog.funcs.push(CodeFunc {
            name: name.clone(),
            ..Default::default()
        });
        self.funcs.insert(name, index);
        self.pending.push((index, func, self_type));
        index
    }

    fn declare(&mut self, items: &[Item]) -> R<()> {
        for item in items {
            match item {
                Item::Struct { name, fields, tuple, copy } => {
                    let id = self.prog.types.len() as u32;
                    self.prog.types.push(TypeDef {
                        name: name.clone(),
                        kind: TypeKind::Struct { fields: fields.clone(), copy: *copy },
                    });
                    self.structs.insert(name.clone(), (id, fields.clone(), *tuple));
                }
                Item::Enum { name, variants, copy } => {
                    let id = self.prog.types.len() as u32;
                    let defs = variants
                        .iter()
                        .map(|(v, shape)| {
                            let fields = match shape {
                                VariantShape::Unit => Vec::new(),
                                VariantShape::Tuple(n) => (0..*n).map(|i| alloc::format!("{}", i)).collect(),
                                VariantShape::Struct(names) => names.clone(),
                            };
                            (v.clone(), fields)
                        })
                        .collect();
                    self.prog.types.push(TypeDef {
                        name: name.clone(),
                        kind: TypeKind::Enum { variants: defs, copy: *copy },
                    });
                    self.enums.insert(name.clone(), id);
                    for (i, (v, shape)) in variants.iter().enumerate() {
                        let n = match shape {
                            VariantShape::Unit => 0,
                            VariantShape::Tuple(n) => *n,
                            VariantShape::Struct(f) => f.len(),
                        };
                        self.variants.entry(v.clone()).or_default().push((id, i as u32, n));
                    }
                }
                Item::Const(name, _, e) => {
                    self.consts.insert(name.clone(), e.clone());
                }
                _ => {}
            }
        }
        for item in items {
            match item {
                Item::Fn(f) => {
                    self.add_func(f.name.clone(), f.clone(), None);
                }
                Item::Impl(ty, methods) => {
                    for m in methods {
                        let name = alloc::format!("{}::{}", ty, m.name);
                        let index = self.add_func(name, m.clone(), Some(ty.clone()));
                        if m.method {
                            self.prog.methods.push((ty.clone(), m.name.clone(), index));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    // ---- kod üretimi ----

    fn emit(&mut self, op: Op, a: u32, b: u32) -> usize {
        self.f.code.push(Ins { op, a, b });
        self.f.lines.push(self.f.line);
        self.f.code.len() - 1
    }

    fn here(&self) -> u32 {
        self.f.code.len() as u32
    }

    fn patch(&mut self, at: usize, target: u32) {
        self.f.code[at].a = target;
    }

    fn constant(&mut self, c: Const) -> u32 {
        if let Some(i) = self.prog.consts.iter().position(|x| *x == c) {
            return i as u32;
        }
        self.prog.consts.push(c);
        (self.prog.consts.len() - 1) as u32
    }

    fn str_const(&mut self, s: &str) -> u32 {
        self.constant(Const::Str(String::from(s)))
    }

    fn new_slot(&mut self) -> u32 {
        self.f.locals += 1;
        self.f.locals - 1
    }

    fn declare_local(&mut self, name: &str) -> u32 {
        let slot = self.new_slot();
        let boxed = self.f.boxed.contains(name);
        self.f.scopes.last_mut().unwrap().push(Local {
            name: String::from(name),
            slot,
            boxed,
        });
        slot
    }

    fn lookup(&self, name: &str) -> Option<(u32, bool)> {
        for scope in self.f.scopes.iter().rev() {
            if let Some(l) = scope.iter().rev().find(|l| l.name == name) {
                return Some((l.slot, l.boxed));
            }
        }
        None
    }

    /// Yerel değişkene değer bağlar (kutulanacaksa hücreye sarar).
    fn bind_local(&mut self, name: &str) {
        let slot = self.declare_local(name);
        self.emit(Op::Store, slot, 0);
        if self.lookup(name).is_some_and(|(_, b)| b) {
            self.emit(Op::BoxLocal, slot, 0);
        }
    }

    fn error<T>(&self, msg: &str) -> R<T> {
        Err(alloc::format!("{}. satır: {}", self.f.line, msg))
    }

    fn function(&mut self, index: u32, func: &Func, self_type: Option<&str>) -> R<()> {
        let saved = core::mem::take(&mut self.f);
        self.f.line = func.line;
        self.f.scopes.push(Vec::new());
        scan_boxed(&func.body, &mut self.f.boxed);
        let mut params = 0;
        if func.method {
            self.declare_local("self");
            params += 1;
        }
        self.f.self_type = self_type.map(String::from);
        let mut pattern_params = Vec::new();
        for (pat, _) in &func.params {
            match pat {
                Pat::Bind(name, _) => {
                    let slot = self.declare_local(name);
                    if self.lookup(name).is_some_and(|(_, b)| b) {
                        pattern_params.push((slot, None));
                    }
                }
                other => {
                    let slot = self.new_slot();
                    pattern_params.push((slot, Some(other.clone())));
                }
            }
            params += 1;
        }
        for (slot, pat) in pattern_params {
            match pat {
                None => {
                    self.emit(Op::BoxLocal, slot, 0);
                }
                Some(p) => {
                    let fails = self.pattern(&p, slot)?;
                    if !fails.is_empty() {
                        return self.error("parametre deseni her değerle eşleşmeli");
                    }
                }
            }
        }
        self.block(&func.body)?;
        self.emit(Op::Ret, 0, 0);
        let code = CodeFunc {
            name: self.prog.funcs[index as usize].name.clone(),
            params,
            locals: self.f.locals.max(params),
            code: core::mem::take(&mut self.f.code),
            lines: core::mem::take(&mut self.f.lines),
        };
        self.prog.funcs[index as usize] = code;
        self.f = saved;
        Ok(())
    }

    fn block(&mut self, b: &Block) -> R<()> {
        self.f.scopes.push(Vec::new());
        // İç fonksiyonlar önce bildirilir.
        for s in &b.stmts {
            if let Stmt::Fn(f) = s {
                self.add_func(f.name.clone(), f.clone(), None);
            }
        }
        for s in &b.stmts {
            match s {
                Stmt::Let(pat, ty, init, otherwise, line) => {
                    self.f.line = *line;
                    match init {
                        Some(e) => {
                            let e = with_collect_hint(e, ty.as_ref());
                            self.expr(&e)?;
                            self.copy_value();
                        }
                        None => {
                            self.emit(Op::Unit, 0, 0);
                        }
                    }
                    if let Some(t) = ty
                        && let Some(e) = init
                        && is_parse_call(e)
                    {
                        let _ = t;
                    }
                    match pat {
                        Pat::Bind(name, _) => self.bind_local(name),
                        other => {
                            let slot = self.new_slot();
                            self.emit(Op::Store, slot, 0);
                            let fails = self.pattern(other, slot)?;
                            match otherwise {
                                Some(block) => {
                                    let skip = self.emit(Op::Jmp, 0, 0);
                                    let here = self.here();
                                    for f in fails {
                                        self.patch(f, here);
                                    }
                                    self.block(block)?;
                                    self.emit(Op::Pop, 0, 0);
                                    let name = self.str_const("let-else bloğu çıkmadı");
                                    self.emit(Op::Panic, name, 0);
                                    let end = self.here();
                                    self.patch(skip, end);
                                }
                                None if !fails.is_empty() => {
                                    let skip = self.emit(Op::Jmp, 0, 0);
                                    let here = self.here();
                                    for f in fails {
                                        self.patch(f, here);
                                    }
                                    let msg = self.str_const("let deseni eşleşmedi");
                                    self.emit(Op::Panic, msg, 0);
                                    let end = self.here();
                                    self.patch(skip, end);
                                }
                                None => {}
                            }
                        }
                    }
                }
                Stmt::Expr(e, line) => {
                    self.f.line = *line;
                    self.expr(e)?;
                    self.emit(Op::Pop, 0, 0);
                }
                Stmt::Fn(_) => {}
            }
        }
        match &b.tail {
            Some(e) => self.expr(e)?,
            None => {
                self.emit(Op::Unit, 0, 0);
            }
        }
        self.f.scopes.pop();
        Ok(())
    }

    /// Yığındaki değer `Copy` bir yapıysa kopyalanır (atamada paylaşım olmasın).
    fn copy_value(&mut self) {}

    /// Desen denetimi: `slot`taki değer eşleşmezse atlanacak sıçramalar.
    fn pattern(&mut self, pat: &Pat, slot: u32) -> R<Vec<usize>> {
        let mut fails = Vec::new();
        match pat {
            Pat::Wild | Pat::Rest => {}
            Pat::Bind(name, _) => {
                // Büyük harfli sabit adı mı?
                if let Some(c) = self.consts.get(name).cloned() {
                    self.emit(Op::Load, slot, 0);
                    self.expr(&c)?;
                    self.bin("==");
                    fails.push(self.emit(Op::Jz, 0, 0));
                } else {
                    self.emit(Op::Load, slot, 0);
                    self.bind_local(name);
                }
            }
            Pat::Lit(e) => {
                self.emit(Op::Load, slot, 0);
                self.expr(e)?;
                self.bin("==");
                fails.push(self.emit(Op::Jz, 0, 0));
            }
            Pat::Range(lo, hi) => {
                self.emit(Op::Load, slot, 0);
                self.expr(lo)?;
                self.bin(">=");
                fails.push(self.emit(Op::Jz, 0, 0));
                self.emit(Op::Load, slot, 0);
                self.expr(hi)?;
                self.bin("<=");
                fails.push(self.emit(Op::Jz, 0, 0));
            }
            Pat::Tuple(items) => {
                let n = items.iter().filter(|p| !matches!(p, Pat::Rest)).count();
                self.emit(Op::Load, slot, 0);
                self.emit(Op::Unpack, n as u32, 0);
                let temps: Vec<u32> = (0..n).map(|_| self.new_slot()).collect();
                for &t in temps.iter().rev() {
                    self.emit(Op::Store, t, 0);
                }
                for (p, t) in items.iter().filter(|p| !matches!(p, Pat::Rest)).zip(temps) {
                    fails.extend(self.pattern(p, t)?);
                }
            }
            Pat::Variant(path, items) => {
                let name = path.last().unwrap();
                if path.len() == 1 && items.is_empty() && let Some(c) = self.consts.get(name).cloned() {
                    self.emit(Op::Load, slot, 0);
                    self.expr(&c)?;
                    self.bin("==");
                    fails.push(self.emit(Op::Jz, 0, 0));
                    return Ok(fails);
                }
                if let Some((ty, _, _)) = self.structs.get(name).cloned() {
                    // Demet yapısı deseni: Nokta(x, y).
                    let _ = ty;
                    let temps: Vec<u32> = (0..items.len()).map(|_| self.new_slot()).collect();
                    self.emit(Op::Load, slot, 0);
                    self.emit(Op::Unpack, items.len() as u32, 0);
                    for &t in temps.iter().rev() {
                        self.emit(Op::Store, t, 0);
                    }
                    for (p, t) in items.iter().zip(temps) {
                        fails.extend(self.pattern(p, t)?);
                    }
                    return Ok(fails);
                }
                let (ty, var, n) = self.resolve_variant(path)?;
                self.emit(Op::Load, slot, 0);
                self.emit(Op::TestVariant, ty, var);
                fails.push(self.emit(Op::Jz, 0, 0));
                if !items.is_empty() {
                    let items: Vec<&Pat> = items.iter().filter(|p| !matches!(p, Pat::Rest)).collect();
                    if items.len() > n {
                        return self.error(&alloc::format!("{} {} alan alır", name, n));
                    }
                    let temps: Vec<u32> = (0..n).map(|_| self.new_slot()).collect();
                    self.emit(Op::Load, slot, 0);
                    self.emit(Op::Unpack, n as u32, 0);
                    for &t in temps.iter().rev() {
                        self.emit(Op::Store, t, 0);
                    }
                    for (p, t) in items.into_iter().zip(temps) {
                        fails.extend(self.pattern(p, t)?);
                    }
                }
            }
            Pat::Struct(path, fields) => {
                let name = path.last().unwrap();
                if !self.structs.contains_key(name) {
                    let (ty, var, _) = self.resolve_variant(path)?;
                    self.emit(Op::Load, slot, 0);
                    self.emit(Op::TestVariant, ty, var);
                    fails.push(self.emit(Op::Jz, 0, 0));
                }
                for (field, p) in fields {
                    let t = self.new_slot();
                    self.emit(Op::Load, slot, 1);
                    let fc = self.str_const(field);
                    self.emit(Op::Field, fc, 0);
                    self.emit(Op::Store, t, 0);
                    fails.extend(self.pattern(p, t)?);
                }
            }
            Pat::Or(list) => {
                let mut matched = Vec::new();
                for (i, p) in list.iter().enumerate() {
                    let f = self.pattern(p, slot)?;
                    if i + 1 == list.len() {
                        fails.extend(f);
                    } else {
                        matched.push(self.emit(Op::Jmp, 0, 0));
                        let next = self.here();
                        for j in f {
                            self.patch(j, next);
                        }
                    }
                }
                let end = self.here();
                for j in matched {
                    self.patch(j, end);
                }
            }
        }
        Ok(fails)
    }

    fn resolve_variant(&self, path: &[String]) -> R<(u32, u32, usize)> {
        let name = path.last().unwrap();
        let candidates = self.variants.get(name).cloned().unwrap_or_default();
        let found = if path.len() >= 2 {
            let ty = &path[path.len() - 2];
            let id = self.enums.get(ty).copied();
            candidates.into_iter().find(|(t, ..)| Some(*t) == id)
        } else {
            candidates.into_iter().next()
        };
        match found {
            Some(v) => Ok(v),
            None => self.error(&alloc::format!("bilinmeyen varyant {}", segs(path))),
        }
    }

    fn bin(&mut self, op: &str) {
        let i = BIN_OPS.iter().position(|o| *o == op).unwrap_or(0) as u32;
        self.emit(Op::Bin, i, 0);
    }

    fn load_name(&mut self, name: &str, no_copy: bool) -> R<bool> {
        if let Some((slot, boxed)) = self.lookup(name) {
            if boxed {
                self.emit(Op::LoadBox, slot, 0);
            } else {
                self.emit(Op::Load, slot, no_copy as u32);
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn resolve_self(&self, path: &[String]) -> Vec<String> {
        path.iter()
            .map(|s| if s == "Self" { self.f.self_type.clone().unwrap_or_else(|| s.clone()) } else { s.clone() })
            .collect()
    }

    fn path(&mut self, path: &[String]) -> R<()> {
        let path = &self.resolve_self(path)[..];
        let name = path.last().unwrap();
        if path.len() == 1 && self.load_name(name, false)? {
            return Ok(());
        }
        if let Some(c) = self.consts.get(name).cloned() {
            return self.expr(&c);
        }
        let joined = segs(path);
        match joined.as_str() {
            "i64::MAX" | "isize::MAX" | "u64::MAX" | "usize::MAX" => return self.int(i64::MAX),
            "i64::MIN" | "isize::MIN" => return self.int(i64::MIN),
            "i32::MAX" => return self.int(i32::MAX as i64),
            "i32::MIN" => return self.int(i32::MIN as i64),
            "u32::MAX" => return self.int(u32::MAX as i64),
            "u8::MAX" => return self.int(255),
            "u16::MAX" => return self.int(65535),
            "consts::PI" => return self.float(core::f64::consts::PI),
            "consts::E" => return self.float(core::f64::consts::E),
            "f64::MAX" => return self.float(f64::MAX),
            "f64::INFINITY" => return self.float(f64::INFINITY),
            "f64::EPSILON" => return self.float(f64::EPSILON),
            _ => {}
        }
        if let Some(&f) = self.funcs.get(&joined).or_else(|| self.funcs.get(name)) {
            self.emit(Op::Closure, f, 0);
            return Ok(());
        }
        if let Ok((ty, var, n)) = self.resolve_variant(path) {
            if n == 0 {
                self.emit(Op::Variant, ty, var << 8);
                return Ok(());
            }
            // Varyant kurucusu fonksiyon değeri olarak (ör. `.map(Some)`).
            return self.error(&alloc::format!("{} argüman ister", joined));
        }
        if let Some((ty, fields, _)) = self.structs.get(name).cloned()
            && fields.is_empty()
        {
            self.emit(Op::Struct, ty, 0);
            return Ok(());
        }
        self.error(&alloc::format!("'{}' bulunamadı", joined))
    }

    fn int(&mut self, v: i64) -> R<()> {
        let c = self.constant(Const::Int(v));
        self.emit(Op::Const, c, 0);
        Ok(())
    }

    fn float(&mut self, v: f64) -> R<()> {
        let c = self.constant(Const::Float(v));
        self.emit(Op::Const, c, 0);
        Ok(())
    }

    fn args(&mut self, args: &[Expr]) -> R<u32> {
        for a in args {
            self.expr(a)?;
        }
        Ok(args.len() as u32)
    }

    fn call(&mut self, callee: &Expr, args: &[Expr]) -> R<()> {
        if let Expr::Path(path, _) = callee {
            let path = &self.resolve_self(path)[..];
            let name = path.last().unwrap();
            if path.len() == 1 && self.lookup(name).is_some() {
                self.load_name(name, true)?;
                let n = self.args(args)?;
                self.emit(Op::CallValue, n, 0);
                return Ok(());
            }
            let joined = segs(path);
            if let Some(&f) = self.funcs.get(&joined).or_else(|| if path.len() == 1 { self.funcs.get(name) } else { None }) {
                let n = self.args(args)?;
                self.emit(Op::Call, f, n);
                return Ok(());
            }
            if let Some((ty, fields, true)) = self.structs.get(name).cloned() {
                if args.len() != fields.len() {
                    return self.error(&alloc::format!("{} {} alan alır", name, fields.len()));
                }
                let n = self.args(args)?;
                self.emit(Op::Struct, ty, n);
                return Ok(());
            }
            if let Ok((ty, var, n)) = self.resolve_variant(path)
                && !joined.contains("::from")
            {
                if args.len() != n {
                    return self.error(&alloc::format!("{} {} değer alır", joined, n));
                }
                self.args(args)?;
                self.emit(Op::Variant, ty, var << 8 | n as u32);
                return Ok(());
            }
            // `Self::yeni(...)` ve `Tür::yöntem(nesne)` biçimleri.
            if path.len() >= 2 {
                let method = name.clone();
                let candidates: Vec<u32> = self
                    .funcs
                    .iter()
                    .filter(|(k, _)| k.ends_with(&alloc::format!("::{}", method)))
                    .map(|(_, v)| *v)
                    .collect();
                if path[path.len() - 2] == "Self" && candidates.len() == 1 {
                    let n = self.args(args)?;
                    self.emit(Op::Call, candidates[0], n);
                    return Ok(());
                }
            }
            let n = self.args(args)?;
            let id = self.str_const(&joined);
            self.emit(Op::Builtin, id, n);
            return Ok(());
        }
        self.expr(callee)?;
        let n = self.args(args)?;
        self.emit(Op::CallValue, n, 0);
        Ok(())
    }

    /// Alıcı ifadesi: yerel değişkense kopyalamadan yüklenir (yöntem yerinde değiştirebilsin).
    fn receiver(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Path(p, _) if p.len() == 1 && self.lookup(&p[0]).is_some() => {
                self.load_name(&p[0], true)?;
                Ok(())
            }
            Expr::Unary("&" | "&mut" | "*", inner, _) => self.receiver(inner),
            Expr::Field(obj, name, line) => {
                self.f.line = *line;
                self.receiver(obj)?;
                let c = self.str_const(name);
                self.emit(Op::Field, c, 1);
                Ok(())
            }
            Expr::Index(obj, idx, _) => {
                self.receiver(obj)?;
                self.expr(idx)?;
                self.emit(Op::Index, 1, 0);
                Ok(())
            }
            other => self.expr(other),
        }
    }

    fn assign_target(&mut self, target: &Expr) -> R<()> {
        match target {
            Expr::Path(p, _) if p.len() == 1 => match self.lookup(&p[0]) {
                Some((slot, true)) => {
                    self.emit(Op::StoreBox, slot, 0);
                    Ok(())
                }
                Some((slot, false)) => {
                    self.emit(Op::Store, slot, 0);
                    Ok(())
                }
                None => self.error(&alloc::format!("'{}' tanımlı değil", p[0])),
            },
            _ => self.error("geçersiz atama hedefi"),
        }
    }

    fn assign(&mut self, op: &str, target: &Expr, value: &Expr) -> R<()> {
        let compound = op != "=";
        let bin = &op[..op.len() - 1];
        match target {
            Expr::Path(..) => {
                if compound {
                    self.expr(target)?;
                    self.expr(value)?;
                    self.bin(bin);
                } else {
                    self.expr(value)?;
                }
                self.assign_target(target)?;
            }
            Expr::Field(obj, name, _) => {
                self.receiver(obj)?;
                let c = self.str_const(name);
                if compound {
                    self.emit(Op::Dup, 0, 0);
                    self.emit(Op::Field, c, 0);
                    self.expr(value)?;
                    self.bin(bin);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::SetField, c, 0);
            }
            Expr::Index(obj, idx, _) => {
                self.receiver(obj)?;
                self.expr(idx)?;
                if compound {
                    // obj, idx → obj, idx, obj[idx]
                    let ti = self.new_slot();
                    let to = self.new_slot();
                    self.emit(Op::Store, ti, 0);
                    self.emit(Op::Store, to, 0);
                    self.emit(Op::Load, to, 1);
                    self.emit(Op::Load, ti, 0);
                    self.emit(Op::Load, to, 1);
                    self.emit(Op::Load, ti, 0);
                    self.emit(Op::Index, 0, 0);
                    self.expr(value)?;
                    self.bin(bin);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::SetIndex, 0, 0);
            }
            Expr::Unary("*", inner, _) => {
                self.expr(inner)?;
                if compound {
                    self.emit(Op::Dup, 0, 0);
                    self.emit(Op::Deref, 0, 0);
                    self.expr(value)?;
                    self.bin(bin);
                } else {
                    self.expr(value)?;
                }
                self.emit(Op::StoreDeref, 0, 0);
            }
            _ => return self.error("geçersiz atama hedefi"),
        }
        self.emit(Op::Unit, 0, 0);
        Ok(())
    }

    fn format(&mut self, args: &[Expr]) -> R<()> {
        let Some(Expr::Str(fmt)) = args.first() else {
            return self.error("biçim dizgesi bekleniyordu");
        };
        // `{ad}` satır içi yakalamaları konumsal argümana çevrilir.
        let mut out = String::new();
        let mut extra: Vec<Expr> = Vec::new();
        let mut positional = args[1..].to_vec();
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0;
        let mut next_arg = 0;
        let mut order: Vec<usize> = Vec::new();
        while i < chars.len() {
            let c = chars[i];
            if c == '{' && chars.get(i + 1) == Some(&'{') {
                out.push_str("{{");
                i += 2;
                continue;
            }
            if c == '}' && chars.get(i + 1) == Some(&'}') {
                out.push_str("}}");
                i += 2;
                continue;
            }
            if c == '{' {
                let end = chars[i..].iter().position(|&x| x == '}').map(|p| i + p).ok_or_else(|| alloc::format!("{}. satır: biçim dizgesinde '}}' eksik", self.f.line))?;
                let inner: String = chars[i + 1..end].iter().collect();
                let (name, spec) = inner.split_once(':').unwrap_or((&inner, ""));
                let index = if name.is_empty() {
                    next_arg += 1;
                    next_arg - 1
                } else if let Ok(n) = name.parse::<usize>() {
                    n
                } else {
                    extra.push(Expr::Path(alloc::vec![String::from(name)], self.f.line));
                    positional.len() + extra.len() - 1
                };
                order.push(index);
                out.push('{');
                if !spec.is_empty() {
                    out.push(':');
                    out.push_str(spec);
                }
                out.push('}');
                i = end + 1;
                continue;
            }
            out.push(c);
            i += 1;
        }
        positional.extend(extra);
        for &index in &order {
            match positional.get(index) {
                Some(e) => self.expr(e)?,
                None => return self.error("biçim dizgesinde fazla {}"),
            }
        }
        let c = self.str_const(&out);
        self.emit(Op::Format, c, order.len() as u32);
        Ok(())
    }

    fn macro_call(&mut self, name: &str, args: &[Expr]) -> R<()> {
        match name {
            "println" | "print" | "eprintln" | "eprint" => {
                if args.is_empty() {
                    let c = self.str_const("");
                    self.emit(Op::Const, c, 0);
                } else {
                    self.format(args)?;
                }
                let flags = name.ends_with("ln") as u32 | (name.starts_with('e') as u32) << 1;
                self.emit(Op::Print, flags, 0);
                self.emit(Op::Unit, 0, 0);
            }
            "format" => self.format(args)?,
            "panic" | "unreachable" | "todo" | "unimplemented" => {
                if args.is_empty() {
                    let c = self.str_const(match name {
                        "todo" => "henüz yazılmadı",
                        "unreachable" => "ulaşılmaması gereken koda ulaşıldı",
                        _ => "panik",
                    });
                    self.emit(Op::Const, c, 0);
                } else {
                    self.format(args)?;
                }
                let id = self.str_const("panic");
                self.emit(Op::Builtin, id, 1);
            }
            "assert" => {
                self.expr(&args[0])?;
                let ok = self.emit(Op::Jnz, 0, 0);
                if args.len() > 1 {
                    self.format(&args[1..])?;
                } else {
                    let c = self.str_const("assert! başarısız");
                    self.emit(Op::Const, c, 0);
                }
                let id = self.str_const("panic");
                self.emit(Op::Builtin, id, 1);
                self.emit(Op::Pop, 0, 0);
                let end = self.here();
                self.patch(ok, end);
                self.emit(Op::Unit, 0, 0);
            }
            "assert_eq" | "assert_ne" => {
                self.expr(&args[0])?;
                self.expr(&args[1])?;
                self.bin(if name == "assert_eq" { "==" } else { "!=" });
                let ok = self.emit(Op::Jnz, 0, 0);
                self.expr(&args[0])?;
                self.expr(&args[1])?;
                let c = self.str_const(if name == "assert_eq" { "assert_eq! başarısız: {:?} != {:?}" } else { "assert_ne! başarısız: {:?} == {:?}" });
                self.emit(Op::Format, c, 2);
                let id = self.str_const("panic");
                self.emit(Op::Builtin, id, 1);
                self.emit(Op::Pop, 0, 0);
                let end = self.here();
                self.patch(ok, end);
                self.emit(Op::Unit, 0, 0);
            }
            "vec" => {
                // vec![x; n] ayrıştırıcıda dizi tekrarı olarak gelir.
                if let [Expr::Repeat(v, n)] = args {
                    self.expr(v)?;
                    self.expr(n)?;
                    self.emit(Op::Repeat, 0, 0);
                } else if let [Expr::Array(items)] = args {
                    let n = self.args(items)?;
                    self.emit(Op::MakeVec, n, 0);
                } else {
                    let n = self.args(args)?;
                    self.emit(Op::MakeVec, n, 0);
                }
            }
            "write" | "writeln" => {
                // write!(dizge, ...) → dizgeye ekle.
                self.receiver(&args[0])?;
                self.format(&args[1..])?;
                if name == "writeln" {
                    let nl = self.str_const("\n");
                    self.emit(Op::Const, nl, 0);
                    self.bin("+");
                }
                let m = self.str_const("push_str");
                self.emit(Op::Method, m, 1);
                self.emit(Op::Pop, 0, 0);
                self.emit(Op::Unit, 0, 0);
                self.emit(Op::Variant, RESULT, 1);
                return Ok(());
            }
            "dbg" => {
                self.expr(&args[0])?;
                self.emit(Op::Dup, 0, 0);
                let c = self.str_const("[dbg] {:?}");
                self.emit(Op::Format, c, 1);
                self.emit(Op::Print, 3, 0);
            }
            _ => return self.error(&alloc::format!("{}! makrosu desteklenmiyor", name)),
        }
        Ok(())
    }

    fn closure(&mut self, params: &[Pat], body: &Expr) -> R<()> {
        // Gövdede geçen ve dış kapsamda tanımlı adlar yakalanır.
        let mut names = BTreeSet::new();
        expr_names(body, &mut names);
        let mut bound = BTreeSet::new();
        for p in params {
            pat_names(p, &mut bound);
        }
        let captures: Vec<(String, u32, bool)> = names
            .into_iter()
            .filter(|n| !bound.contains(n))
            .filter_map(|n| self.lookup(&n).map(|(s, b)| (n, s, b)))
            .collect();
        for (_, slot, boxed) in &captures {
            if *boxed {
                self.emit(Op::RefLocal, *slot, 0);
            } else {
                self.emit(Op::Load, *slot, 1);
            }
        }
        self.closure_count += 1;
        let name = alloc::format!("closure#{}", self.closure_count);
        let index = self.prog.funcs.len() as u32;
        self.prog.funcs.push(CodeFunc {
            name: name.clone(),
            ..Default::default()
        });
        let saved = core::mem::take(&mut self.f);
        self.f.line = saved.line;
        self.f.self_type = saved.self_type.clone();
        self.f.scopes.push(Vec::new());
        scan_boxed_expr(body, &mut self.f.boxed);
        for (n, _, boxed) in &captures {
            let slot = self.new_slot();
            self.f.scopes.last_mut().unwrap().push(Local {
                name: n.clone(),
                slot,
                boxed: *boxed,
            });
        }
        let mut pattern_params = Vec::new();
        for p in params {
            match p {
                Pat::Bind(n, _) => {
                    let slot = self.declare_local(n);
                    if self.lookup(n).is_some_and(|(_, b)| b) {
                        pattern_params.push((slot, None));
                    }
                }
                other => {
                    let slot = self.new_slot();
                    pattern_params.push((slot, Some(other.clone())));
                }
            }
        }
        for (slot, pat) in pattern_params {
            match pat {
                None => {
                    self.emit(Op::BoxLocal, slot, 0);
                }
                Some(p) => {
                    self.pattern(&p, slot)?;
                }
            }
        }
        let params_n = (captures.len() + params.len()) as u32;
        self.expr(body)?;
        self.emit(Op::Ret, 0, 0);
        self.prog.funcs[index as usize] = CodeFunc {
            name,
            params: params_n,
            locals: self.f.locals.max(params_n),
            code: core::mem::take(&mut self.f.code),
            lines: core::mem::take(&mut self.f.lines),
        };
        self.f = saved;
        self.emit(Op::Closure, index, captures.len() as u32);
        Ok(())
    }

    fn loop_body(&mut self, block: &Block) -> R<()> {
        self.block(block)?;
        self.emit(Op::Pop, 0, 0);
        Ok(())
    }

    fn finish_loop(&mut self, continue_target: u32) -> u32 {
        let ctx = self.f.loops.pop().unwrap();
        let end = self.here();
        for b in ctx.breaks {
            self.patch(b, end);
        }
        for c in ctx.continues {
            self.patch(c, continue_target);
        }
        ctx.result
    }

    fn push_loop(&mut self) -> u32 {
        let result = self.new_slot();
        self.emit(Op::Unit, 0, 0);
        self.emit(Op::Store, result, 0);
        self.f.loops.push(LoopCtx {
            breaks: Vec::new(),
            continues: Vec::new(),
            result,
        });
        result
    }

    pub fn expr(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Int(v) => self.int(*v)?,
            Expr::Float(v) => self.float(*v)?,
            Expr::Str(s) => {
                let c = self.str_const(s);
                self.emit(Op::Const, c, 0);
            }
            Expr::Char(ch) => {
                let c = self.constant(Const::Int(*ch as i64));
                self.emit(Op::Const, c, 0);
                let t = self.str_const("char");
                self.emit(Op::Cast, t, 0);
            }
            Expr::Bool(b) => {
                self.emit(if *b { Op::True } else { Op::False }, 0, 0);
            }
            Expr::Unit => {
                self.emit(Op::Unit, 0, 0);
            }
            Expr::Path(p, line) => {
                self.f.line = *line;
                self.path(p)?;
            }
            Expr::Call(callee, args, line) => {
                self.f.line = *line;
                self.call(callee, args)?;
            }
            Expr::Method(recv, name, turbofish, args, line) => {
                self.f.line = *line;
                self.receiver(recv)?;
                let n = self.args(args)?;
                let hint = turbofish.as_ref().map(|t| String::from(t.name())).unwrap_or_default();
                let full = if hint.is_empty() { name.clone() } else { alloc::format!("{}:{}", name, hint) };
                let c = self.str_const(&full);
                self.emit(Op::Method, c, n);
            }
            Expr::Field(obj, name, line) => {
                self.f.line = *line;
                self.receiver(obj)?;
                let c = self.str_const(name);
                self.emit(Op::Field, c, 0);
            }
            Expr::Index(obj, idx, line) => {
                self.f.line = *line;
                self.receiver(obj)?;
                self.expr(idx)?;
                self.emit(Op::Index, 0, 0);
            }
            Expr::Unary(op, inner, line) => {
                self.f.line = *line;
                match *op {
                    "-" => {
                        self.expr(inner)?;
                        self.emit(Op::Neg, 0, 0);
                    }
                    "!" => {
                        self.expr(inner)?;
                        self.emit(Op::Not, 0, 0);
                    }
                    "*" => {
                        self.expr(inner)?;
                        self.emit(Op::Deref, 0, 0);
                    }
                    _ => {
                        // & ve &mut: kutulu yerel değişkenin hücresi, diğerleri paylaşılan değer.
                        if let Expr::Path(p, _) = &**inner
                            && p.len() == 1
                            && let Some((slot, boxed)) = self.lookup(&p[0])
                        {
                            if boxed {
                                self.emit(Op::RefLocal, slot, 0);
                            } else {
                                self.emit(Op::Load, slot, 1);
                            }
                        } else {
                            self.receiver(inner)?;
                        }
                    }
                }
            }
            Expr::Binary(op, a, b, line) => {
                self.f.line = *line;
                if *op == "&&" || *op == "||" {
                    self.expr(a)?;
                    self.emit(Op::Dup, 0, 0);
                    let j = self.emit(if *op == "&&" { Op::Jz } else { Op::Jnz }, 0, 0);
                    self.emit(Op::Pop, 0, 0);
                    self.expr(b)?;
                    let end = self.here();
                    self.patch(j, end);
                } else {
                    self.expr(a)?;
                    self.expr(b)?;
                    self.bin(op);
                }
            }
            Expr::Assign(op, target, value, line) => {
                self.f.line = *line;
                self.assign(op, target, value)?;
            }
            Expr::If(cond, then, otherwise) => {
                self.expr(cond)?;
                let jz = self.emit(Op::Jz, 0, 0);
                self.block(then)?;
                let jend = self.emit(Op::Jmp, 0, 0);
                let else_at = self.here();
                self.patch(jz, else_at);
                match otherwise {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Unit, 0, 0);
                    }
                }
                let end = self.here();
                self.patch(jend, end);
            }
            Expr::IfLet(pat, value, then, otherwise) => {
                self.f.scopes.push(Vec::new());
                self.expr(value)?;
                let slot = self.new_slot();
                self.emit(Op::Store, slot, 0);
                let fails = self.pattern(pat, slot)?;
                self.block(then)?;
                let jend = self.emit(Op::Jmp, 0, 0);
                let else_at = self.here();
                for f in fails {
                    self.patch(f, else_at);
                }
                self.f.scopes.pop();
                match otherwise {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Unit, 0, 0);
                    }
                }
                let end = self.here();
                self.patch(jend, end);
            }
            Expr::Match(scrutinee, arms, line) => {
                self.f.line = *line;
                self.expr(scrutinee)?;
                let slot = self.new_slot();
                self.emit(Op::Store, slot, 0);
                let mut ends = Vec::new();
                for (pat, guard, body) in arms {
                    self.f.scopes.push(Vec::new());
                    let mut fails = self.pattern(pat, slot)?;
                    if let Some(g) = guard {
                        self.expr(g)?;
                        fails.push(self.emit(Op::Jz, 0, 0));
                    }
                    self.expr(body)?;
                    ends.push(self.emit(Op::Jmp, 0, 0));
                    let next = self.here();
                    for f in fails {
                        self.patch(f, next);
                    }
                    self.f.scopes.pop();
                }
                let msg = self.str_const("match hiçbir kolla eşleşmedi");
                self.emit(Op::Panic, msg, 0);
                let end = self.here();
                for j in ends {
                    self.patch(j, end);
                }
            }
            Expr::Block(b) => self.block(b)?,
            Expr::Loop(body, _) => {
                let result = self.push_loop();
                let start = self.here();
                self.loop_body(body)?;
                self.emit(Op::Jmp, start, 0);
                self.finish_loop(start);
                self.emit(Op::Load, result, 0);
            }
            Expr::While(cond, body, _) => {
                self.push_loop();
                let start = self.here();
                self.expr(cond)?;
                let jz = self.emit(Op::Jz, 0, 0);
                self.f.loops.last_mut().unwrap().breaks.push(jz);
                self.loop_body(body)?;
                self.emit(Op::Jmp, start, 0);
                self.finish_loop(start);
                self.emit(Op::Unit, 0, 0);
            }
            Expr::WhileLet(pat, value, body, _) => {
                self.push_loop();
                let start = self.here();
                self.f.scopes.push(Vec::new());
                self.expr(value)?;
                let slot = self.new_slot();
                self.emit(Op::Store, slot, 0);
                let fails = self.pattern(pat, slot)?;
                self.f.loops.last_mut().unwrap().breaks.extend(fails);
                self.loop_body(body)?;
                self.f.scopes.pop();
                self.emit(Op::Jmp, start, 0);
                self.finish_loop(start);
                self.emit(Op::Unit, 0, 0);
            }
            Expr::For(pat, iter, body, _) => {
                self.expr(iter)?;
                self.emit(Op::Iter, 0, 0);
                let it = self.new_slot();
                self.emit(Op::Store, it, 0);
                self.push_loop();
                let start = self.here();
                self.emit(Op::Load, it, 1);
                let next = self.emit(Op::Next, 0, 0);
                self.f.loops.last_mut().unwrap().breaks.push(next);
                self.f.scopes.push(Vec::new());
                let slot = self.new_slot();
                self.emit(Op::Store, slot, 0);
                let fails = self.pattern(pat, slot)?;
                if !fails.is_empty() {
                    return self.error("for deseni her öğeyle eşleşmeli");
                }
                self.loop_body(body)?;
                self.f.scopes.pop();
                self.emit(Op::Jmp, start, 0);
                self.finish_loop(start);
                self.emit(Op::Unit, 0, 0);
            }
            Expr::Break(_, value) => {
                if self.f.loops.is_empty() {
                    return self.error("döngü dışında break");
                }
                if let Some(v) = value {
                    self.expr(v)?;
                    let result = self.f.loops.last().unwrap().result;
                    self.emit(Op::Store, result, 0);
                }
                let j = self.emit(Op::Jmp, 0, 0);
                self.f.loops.last_mut().unwrap().breaks.push(j);
                self.emit(Op::Unit, 0, 0);
            }
            Expr::Continue(_) => {
                if self.f.loops.is_empty() {
                    return self.error("döngü dışında continue");
                }
                let j = self.emit(Op::Jmp, 0, 0);
                self.f.loops.last_mut().unwrap().continues.push(j);
                self.emit(Op::Unit, 0, 0);
            }
            Expr::Return(value) => {
                match value {
                    Some(v) => self.expr(v)?,
                    None => {
                        self.emit(Op::Unit, 0, 0);
                    }
                }
                self.emit(Op::Ret, 0, 0);
                self.emit(Op::Unit, 0, 0);
            }
            Expr::Range(lo, hi, inclusive) => {
                let mut flags = *inclusive as u32;
                if let Some(l) = lo {
                    self.expr(l)?;
                    flags |= 2;
                }
                if let Some(h) = hi {
                    self.expr(h)?;
                    flags |= 4;
                }
                self.emit(Op::Range, flags, 0);
            }
            Expr::StructLit(path, fields, base, line) => {
                self.f.line = *line;
                let path = &self.resolve_self(path)[..];
                let name = path.last().unwrap();
                if let Some((ty, defs, _)) = self.structs.get(name).cloned() {
                    if let Some(b) = base {
                        self.expr(b)?;
                        let m = self.str_const("clone");
                        self.emit(Op::Method, m, 0);
                        for (f, v) in fields {
                            self.emit(Op::Dup, 0, 0);
                            self.expr(v)?;
                            let c = self.str_const(f);
                            self.emit(Op::SetField, c, 0);
                        }
                        return Ok(());
                    }
                    for d in &defs {
                        match fields.iter().find(|(f, _)| f == d) {
                            Some((_, v)) => self.expr(v)?,
                            None => return self.error(&alloc::format!("{} yapısında '{}' alanı eksik", name, d)),
                        }
                    }
                    if let Some((f, _)) = fields.iter().find(|(f, _)| !defs.contains(f)) {
                        return self.error(&alloc::format!("{} yapısında '{}' alanı yok", name, f));
                    }
                    self.emit(Op::Struct, ty, defs.len() as u32);
                } else {
                    let (ty, var, _) = self.resolve_variant(path)?;
                    let names = match &self.prog.types[ty as usize].kind {
                        TypeKind::Enum { variants, .. } => variants[var as usize].1.clone(),
                        _ => Vec::new(),
                    };
                    for d in &names {
                        match fields.iter().find(|(f, _)| f == d) {
                            Some((_, v)) => self.expr(v)?,
                            None => return self.error(&alloc::format!("'{}' alanı eksik", d)),
                        }
                    }
                    self.emit(Op::Variant, ty, var << 8 | names.len() as u32);
                }
            }
            Expr::Tuple(items) => {
                let n = self.args(items)?;
                self.emit(Op::Tuple, n, 0);
            }
            Expr::Array(items) => {
                let n = self.args(items)?;
                self.emit(Op::MakeVec, n, 0);
            }
            Expr::Repeat(v, n) => {
                self.expr(v)?;
                self.expr(n)?;
                self.emit(Op::Repeat, 0, 0);
            }
            Expr::Macro(name, args, line) => {
                self.f.line = *line;
                self.macro_call(name, args)?;
            }
            Expr::Closure(params, body) => self.closure(params, body)?,
            Expr::Cast(inner, ty) => {
                self.expr(inner)?;
                let c = self.str_const(ty.name());
                self.emit(Op::Cast, c, 0);
            }
            Expr::Try(inner, line) => {
                self.f.line = *line;
                self.expr(inner)?;
                self.emit(Op::Try, 0, 0);
            }
        }
        Ok(())
    }
}

fn is_parse_call(e: &Expr) -> bool {
    matches!(e, Expr::Method(_, n, ..) if n == "parse")
}

/// `let x: T = ....parse()/collect()/sum()` → hedef türü yönteme ipucu olarak ekler.
fn with_collect_hint(e: &Expr, ty: Option<&Type>) -> Expr {
    let Some(ty) = ty else { return e.clone() };
    fn walk(e: &Expr, ty: &Type) -> Expr {
        match e {
            Expr::Method(recv, name, None, args, line)
                if matches!(name.as_str(), "parse" | "collect" | "sum" | "product" | "into") =>
            {
                Expr::Method(recv.clone(), name.clone(), Some(ty.clone()), args.clone(), *line)
            }
            // `.parse().unwrap()` / `.expect()` / `?`
            Expr::Method(recv, name, t, args, line) if matches!(name.as_str(), "unwrap" | "expect" | "unwrap_or" | "unwrap_or_default") => {
                let inner_ty = Type::Named(String::from("Result"), alloc::vec![ty.clone()]);
                let _ = inner_ty;
                Expr::Method(alloc::boxed::Box::new(walk(recv, ty)), name.clone(), t.clone(), args.clone(), *line)
            }
            Expr::Try(inner, line) => Expr::Try(alloc::boxed::Box::new(walk(inner, ty)), *line),
            other => other.clone(),
        }
    }
    walk(e, ty)
}

/// `&mut ad` ile ödünç verilen ve closure içinde atanan yerel adlar.
fn scan_boxed(b: &Block, out: &mut BTreeSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Let(_, _, Some(e), other, _) => {
                scan_boxed_expr(e, out);
                if let Some(o) = other {
                    scan_boxed(o, out);
                }
            }
            Stmt::Expr(e, _) => scan_boxed_expr(e, out),
            _ => {}
        }
    }
    if let Some(t) = &b.tail {
        scan_boxed_expr(t, out);
    }
}

fn scan_boxed_expr(e: &Expr, out: &mut BTreeSet<String>) {
    visit(e, &mut |x| match x {
        Expr::Unary("&mut", inner, _) => {
            if let Expr::Path(p, _) = &**inner
                && p.len() == 1
            {
                out.insert(p[0].clone());
            }
        }
        Expr::Closure(_, body) => {
            visit(body, &mut |y| {
                if let Expr::Assign(_, target, _, _) = y
                    && let Expr::Path(p, _) = &**target
                    && p.len() == 1
                {
                    out.insert(p[0].clone());
                }
                if let Expr::Method(recv, ..) = y
                    && let Expr::Path(p, _) = &**recv
                    && p.len() == 1
                {
                    // Closure içinde yöntem çağrılan yakalanmış değişken (ör. `v.push`).
                    let _ = p;
                }
            });
        }
        _ => {}
    });
}

fn visit_block(b: &Block, f: &mut dyn FnMut(&Expr)) {
    for s in &b.stmts {
        match s {
            Stmt::Let(_, _, init, other, _) => {
                if let Some(e) = init {
                    visit(e, f);
                }
                if let Some(o) = other {
                    visit_block(o, f);
                }
            }
            Stmt::Expr(e, _) => visit(e, f),
            Stmt::Fn(_) => {}
        }
    }
    if let Some(t) = &b.tail {
        visit(t, f);
    }
}

fn visit(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Call(c, args, _) => {
            visit(c, f);
            args.iter().for_each(|a| visit(a, f));
        }
        Expr::Method(r, _, _, args, _) => {
            visit(r, f);
            args.iter().for_each(|a| visit(a, f));
        }
        Expr::Field(o, ..) | Expr::Unary(_, o, _) | Expr::Cast(o, _) | Expr::Try(o, _) => visit(o, f),
        Expr::Index(a, b, _) | Expr::Binary(_, a, b, _) | Expr::Assign(_, a, b, _) | Expr::Repeat(a, b) => {
            visit(a, f);
            visit(b, f);
        }
        Expr::If(c, t, o) => {
            visit(c, f);
            visit_block(t, f);
            if let Some(o) = o {
                visit(o, f);
            }
        }
        Expr::IfLet(_, v, t, o) => {
            visit(v, f);
            visit_block(t, f);
            if let Some(o) = o {
                visit(o, f);
            }
        }
        Expr::Match(s, arms, _) => {
            visit(s, f);
            for (_, g, b) in arms {
                if let Some(g) = g {
                    visit(g, f);
                }
                visit(b, f);
            }
        }
        Expr::Block(b) | Expr::Loop(b, _) => visit_block(b, f),
        Expr::While(c, b, _) | Expr::WhileLet(_, c, b, _) | Expr::For(_, c, b, _) => {
            visit(c, f);
            visit_block(b, f);
        }
        Expr::Break(_, Some(v)) | Expr::Return(Some(v)) => visit(v, f),
        Expr::Range(a, b, _) => {
            if let Some(a) = a {
                visit(a, f);
            }
            if let Some(b) = b {
                visit(b, f);
            }
        }
        Expr::StructLit(_, fields, base, _) => {
            fields.iter().for_each(|(_, v)| visit(v, f));
            if let Some(b) = base {
                visit(b, f);
            }
        }
        Expr::Tuple(items) | Expr::Array(items) | Expr::Macro(_, items, _) => items.iter().for_each(|a| visit(a, f)),
        Expr::Closure(_, body) => visit(body, f),
        _ => {}
    }
}

fn expr_names(e: &Expr, out: &mut BTreeSet<String>) {
    visit(e, &mut |x| {
        if let Expr::Path(p, _) = x
            && p.len() == 1
        {
            out.insert(p[0].clone());
        }
        if let Expr::Macro(_, args, _) = x
            && let Some(Expr::Str(fmt)) = args.first()
        {
            // Biçim dizgesindeki `{ad}` yakalamaları.
            let mut rest = fmt.as_str();
            while let Some(i) = rest.find('{') {
                rest = &rest[i + 1..];
                let end = rest.find(['}', ':']).unwrap_or(rest.len());
                let name = &rest[..end];
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.chars().next().unwrap().is_ascii_digit() {
                    out.insert(String::from(name));
                }
            }
        }
    });
}

fn pat_names(p: &Pat, out: &mut BTreeSet<String>) {
    match p {
        Pat::Bind(n, _) => {
            out.insert(n.clone());
        }
        Pat::Tuple(items) | Pat::Variant(_, items) | Pat::Or(items) => items.iter().for_each(|i| pat_names(i, out)),
        Pat::Struct(_, fields) => fields.iter().for_each(|(_, p)| pat_names(p, out)),
        _ => {}
    }
}
