//! `.banna` bayt kodu: sabit havuzu, fonksiyonlar, tür tanımları ve dosya biçimi.
//!
//! Dosya: `#!/bin/banna-run\n` + "BNA1" + bölümler (küçük endian). Her komut 9 bayt:
//! işlem kodu (u8), a (u32), b (u32).

use alloc::string::String;
use alloc::vec::Vec;

pub const MAGIC: &[u8] = b"#!/bin/banna-run\nBNA1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    Const,
    Unit,
    True,
    False,
    Load,
    Store,
    LoadBox,
    StoreBox,
    BoxLocal,
    RefLocal,
    Deref,
    StoreDeref,
    Pop,
    Dup,
    Bin,
    Neg,
    Not,
    Jmp,
    Jz,
    Jnz,
    Call,
    CallValue,
    Ret,
    Builtin,
    Method,
    Field,
    SetField,
    Index,
    SetIndex,
    Struct,
    Variant,
    MakeVec,
    Tuple,
    Range,
    Repeat,
    Format,
    Print,
    Iter,
    Next,
    TestVariant,
    Unpack,
    Cast,
    Closure,
    Try,
    Panic,
    Line,
    Swap,
    StructBase,
}

impl Op {
    fn from_u8(v: u8) -> Option<Op> {
        if v <= Op::StructBase as u8 {
            Some(unsafe { core::mem::transmute::<u8, Op>(v) })
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ins {
    pub op: Op,
    pub a: u32,
    pub b: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Clone, Debug, Default)]
pub struct Func {
    pub name: String,
    pub params: u32,
    pub locals: u32,
    pub code: Vec<Ins>,
    pub lines: Vec<u32>,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    Struct { fields: Vec<String>, copy: bool },
    /// Varyant adları ve alanları (adlı alanlar ya da sayısal sıra).
    Enum { variants: Vec<(String, Vec<String>)>, copy: bool },
}

#[derive(Clone, Debug)]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeKind,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub consts: Vec<Const>,
    pub funcs: Vec<Func>,
    pub types: Vec<TypeDef>,
    /// (tür, yöntem) → fonksiyon.
    pub methods: Vec<(String, String, u32)>,
    pub main: u32,
}

/// İkili işleçler (`Op::Bin`'in `a` alanı).
pub const BIN_OPS: &[&str] = &["+", "-", "*", "/", "%", "==", "!=", "<", ">", "<=", ">=", "&", "|", "^", "<<", ">>", "&&", "||"];

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn bytes(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.d.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }
}

impl Program {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(MAGIC.to_vec());
        w.u32(self.consts.len() as u32);
        for c in &self.consts {
            match c {
                Const::Int(v) => {
                    w.u8(0);
                    w.0.extend_from_slice(&v.to_le_bytes());
                }
                Const::Float(v) => {
                    w.u8(1);
                    w.0.extend_from_slice(&v.to_bits().to_le_bytes());
                }
                Const::Str(s) => {
                    w.u8(2);
                    w.str(s);
                }
            }
        }
        w.u32(self.types.len() as u32);
        for t in &self.types {
            w.str(&t.name);
            match &t.kind {
                TypeKind::Struct { fields, copy } => {
                    w.u8(0);
                    w.u8(*copy as u8);
                    w.u32(fields.len() as u32);
                    for f in fields {
                        w.str(f);
                    }
                }
                TypeKind::Enum { variants, copy } => {
                    w.u8(1);
                    w.u8(*copy as u8);
                    w.u32(variants.len() as u32);
                    for (name, fields) in variants {
                        w.str(name);
                        w.u32(fields.len() as u32);
                        for f in fields {
                            w.str(f);
                        }
                    }
                }
            }
        }
        w.u32(self.funcs.len() as u32);
        for f in &self.funcs {
            w.str(&f.name);
            w.u32(f.params);
            w.u32(f.locals);
            w.u32(f.code.len() as u32);
            for (i, ins) in f.code.iter().enumerate() {
                w.u8(ins.op as u8);
                w.u32(ins.a);
                w.u32(ins.b);
                w.u32(f.lines.get(i).copied().unwrap_or(0));
            }
        }
        w.u32(self.methods.len() as u32);
        for (t, m, f) in &self.methods {
            w.str(t);
            w.str(m);
            w.u32(*f);
        }
        w.u32(self.main);
        w.0
    }

    pub fn decode(data: &[u8]) -> Option<Program> {
        let body = data.strip_prefix(MAGIC)?;
        let mut r = Rd { d: body, p: 0 };
        let mut p = Program::default();
        for _ in 0..r.u32()? {
            p.consts.push(match r.u8()? {
                0 => Const::Int(r.u64()? as i64),
                1 => Const::Float(f64::from_bits(r.u64()?)),
                _ => Const::Str(r.str()?),
            });
        }
        for _ in 0..r.u32()? {
            let name = r.str()?;
            let kind = r.u8()?;
            let copy = r.u8()? != 0;
            let kind = if kind == 0 {
                let n = r.u32()?;
                let mut fields = Vec::new();
                for _ in 0..n {
                    fields.push(r.str()?);
                }
                TypeKind::Struct { fields, copy }
            } else {
                let n = r.u32()?;
                let mut variants = Vec::new();
                for _ in 0..n {
                    let vn = r.str()?;
                    let k = r.u32()?;
                    let mut fields = Vec::new();
                    for _ in 0..k {
                        fields.push(r.str()?);
                    }
                    variants.push((vn, fields));
                }
                TypeKind::Enum { variants, copy }
            };
            p.types.push(TypeDef { name, kind });
        }
        for _ in 0..r.u32()? {
            let mut f = Func {
                name: r.str()?,
                params: r.u32()?,
                locals: r.u32()?,
                ..Default::default()
            };
            for _ in 0..r.u32()? {
                let op = Op::from_u8(r.u8()?)?;
                let a = r.u32()?;
                let b = r.u32()?;
                f.code.push(Ins { op, a, b });
                f.lines.push(r.u32()?);
            }
            p.funcs.push(f);
        }
        for _ in 0..r.u32()? {
            let t = r.str()?;
            let m = r.str()?;
            p.methods.push((t, m, r.u32()?));
        }
        p.main = r.u32()?;
        Some(p)
    }
}
