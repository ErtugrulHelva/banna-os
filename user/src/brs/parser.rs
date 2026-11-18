//! Banna Rust ayrıştırıcısı: Rust'ın bir alt kümesini sözdizimi ağacına çevirir.
//! Genel türler (`<T>`), yaşam süreleri ve `where` yan tümceleri ayrıştırılıp yok sayılır.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::lexer::{Tok, Token, tokenize};

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    Named(String, Vec<Type>),
    Ref(Box<Type>, bool),
    Tuple(Vec<Type>),
    Array(Box<Type>),
    Infer,
}

impl Type {
    pub fn name(&self) -> &str {
        match self {
            Type::Named(n, _) => n,
            Type::Ref(t, _) => t.name(),
            _ => "",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Pat {
    Wild,
    Bind(String, bool),
    Lit(Box<Expr>),
    Range(Box<Expr>, Box<Expr>),
    Tuple(Vec<Pat>),
    /// `Yol(alt, desenler)` — enum varyantı ya da demet yapısı.
    Variant(Vec<String>, Vec<Pat>),
    Struct(Vec<String>, Vec<(String, Pat)>),
    Or(Vec<Pat>),
    Rest,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Box<Expr>>,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(Pat, Option<Type>, Option<Expr>, Option<Block>, u32),
    Expr(Expr, u32),
    Fn(Func),
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
    Unit,
    Path(Vec<String>, u32),
    Call(Box<Expr>, Vec<Expr>, u32),
    Method(Box<Expr>, String, Option<Type>, Vec<Expr>, u32),
    Field(Box<Expr>, String, u32),
    Index(Box<Expr>, Box<Expr>, u32),
    Unary(&'static str, Box<Expr>, u32),
    Binary(&'static str, Box<Expr>, Box<Expr>, u32),
    Assign(&'static str, Box<Expr>, Box<Expr>, u32),
    If(Box<Expr>, Block, Option<Box<Expr>>),
    IfLet(Pat, Box<Expr>, Block, Option<Box<Expr>>),
    Match(Box<Expr>, Vec<(Pat, Option<Expr>, Expr)>, u32),
    Block(Block),
    Loop(Block, Option<String>),
    While(Box<Expr>, Block, Option<String>),
    WhileLet(Pat, Box<Expr>, Block, Option<String>),
    For(Pat, Box<Expr>, Block, Option<String>),
    Break(Option<String>, Option<Box<Expr>>),
    Continue(Option<String>),
    Return(Option<Box<Expr>>),
    Range(Option<Box<Expr>>, Option<Box<Expr>>, bool),
    StructLit(Vec<String>, Vec<(String, Expr)>, Option<Box<Expr>>, u32),
    Tuple(Vec<Expr>),
    Array(Vec<Expr>),
    Repeat(Box<Expr>, Box<Expr>),
    Macro(String, Vec<Expr>, u32),
    Closure(Vec<Pat>, Box<Expr>),
    Cast(Box<Expr>, Type),
    Try(Box<Expr>, u32),
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    /// `self` parametresi var mı.
    pub method: bool,
    pub params: Vec<(Pat, Type)>,
    pub ret: Option<Type>,
    pub body: Block,
    pub line: u32,
}

#[derive(Clone, Debug)]
pub enum VariantShape {
    Unit,
    Tuple(usize),
    Struct(Vec<String>),
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(Func),
    Struct {
        name: String,
        fields: Vec<String>,
        tuple: bool,
        copy: bool,
    },
    Enum {
        name: String,
        variants: Vec<(String, VariantShape)>,
        copy: bool,
    },
    Impl(String, Vec<Func>),
    Const(String, Option<Type>, Expr),
}

struct Parser {
    t: Vec<Token>,
    p: usize,
    no_struct: bool,
}

pub type R<T> = Result<T, String>;

pub fn parse(src: &str) -> R<Vec<Item>> {
    let mut p = Parser {
        t: tokenize(src)?,
        p: 0,
        no_struct: false,
    };
    let mut items = Vec::new();
    while !p.at(&Tok::Eof) {
        if let Some(item) = p.item()? {
            items.push(item);
        }
    }
    Ok(items)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.t[self.p].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        &self.t[(self.p + n).min(self.t.len() - 1)].tok
    }
    fn line(&self) -> u32 {
        self.t[self.p].line
    }
    fn at(&self, t: &Tok) -> bool {
        self.peek() == t
    }
    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::P(x) if *x == p)
    }
    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Tok::Ident(x) if x == w)
    }
    fn next(&mut self) -> Tok {
        let t = self.t[self.p].tok.clone();
        if self.p < self.t.len() - 1 {
            self.p += 1;
        }
        t
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.is(p) {
            self.next();
            true
        } else {
            false
        }
    }
    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.next();
            true
        } else {
            false
        }
    }
    fn err<T>(&self, what: &str) -> R<T> {
        Err(alloc::format!(
            "{}. satır: {} bekleniyordu, '{}' bulundu",
            self.line(),
            what,
            describe(self.peek())
        ))
    }
    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            self.err(&alloc::format!("'{}'", p))
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            _ => {
                self.p -= 1;
                self.err("ad")
            }
        }
    }

    /// `<...>` genel parametre listesini atlar.
    fn skip_generics(&mut self) {
        if !self.is("<") {
            return;
        }
        let mut depth = 0;
        loop {
            match self.next() {
                Tok::P("<") => depth += 1,
                Tok::P(">") => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                Tok::P(">>") => {
                    depth -= 2;
                    if depth <= 0 {
                        return;
                    }
                }
                Tok::Eof => return,
                _ => {}
            }
        }
    }

    fn attributes(&mut self) -> Vec<String> {
        let mut derives = Vec::new();
        while self.is("#") {
            self.next();
            self.eat("!");
            if !self.eat("[") {
                break;
            }
            let mut depth = 1;
            while depth > 0 {
                match self.next() {
                    Tok::P("[") => depth += 1,
                    Tok::P("]") => depth -= 1,
                    Tok::Ident(w) => derives.push(w),
                    Tok::Eof => break,
                    _ => {}
                }
            }
        }
        derives
    }

    fn item(&mut self) -> R<Option<Item>> {
        let attrs = self.attributes();
        let copy = attrs.iter().any(|a| a == "Copy");
        self.eat_word("pub");
        if self.is("(") {
            // pub(crate)
            while !self.eat(")") {
                self.next();
            }
        }
        if self.eat_word("use") || self.eat_word("mod") || self.eat_word("extern") {
            while !self.eat(";") {
                if self.is("{") {
                    let mut depth = 0;
                    loop {
                        match self.next() {
                            Tok::P("{") => depth += 1,
                            Tok::P("}") => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            Tok::Eof => break,
                            _ => {}
                        }
                    }
                    if !self.is(";") {
                        return Ok(None);
                    }
                    continue;
                }
                if self.at(&Tok::Eof) {
                    break;
                }
                self.next();
            }
            return Ok(None);
        }
        if self.is_word("fn") || self.is_word("const") && matches!(self.peek_at(1), Tok::Ident(w) if w == "fn") || self.is_word("async") || self.is_word("unsafe") && matches!(self.peek_at(1), Tok::Ident(w) if w == "fn") {
            return Ok(Some(Item::Fn(self.func()?)));
        }
        if self.eat_word("const") || self.eat_word("static") {
            self.eat_word("mut");
            let name = self.ident()?;
            let ty = if self.eat(":") { Some(self.ty()?) } else { None };
            self.expect("=")?;
            let e = self.expr()?;
            self.expect(";")?;
            return Ok(Some(Item::Const(name, ty, e)));
        }
        if self.eat_word("struct") {
            let name = self.ident()?;
            self.skip_generics();
            let mut fields = Vec::new();
            let mut tuple = false;
            if self.eat("(") {
                tuple = true;
                let mut n = 0;
                while !self.eat(")") {
                    self.eat_word("pub");
                    self.ty()?;
                    n += 1;
                    self.eat(",");
                }
                fields = (0..n).map(|i| alloc::format!("{}", i)).collect();
                self.eat(";");
            } else if self.eat("{") {
                while !self.eat("}") {
                    self.attributes();
                    self.eat_word("pub");
                    fields.push(self.ident()?);
                    self.expect(":")?;
                    self.ty()?;
                    self.eat(",");
                }
            } else {
                self.expect(";")?;
            }
            return Ok(Some(Item::Struct { name, fields, tuple, copy }));
        }
        if self.eat_word("enum") {
            let name = self.ident()?;
            self.skip_generics();
            self.expect("{")?;
            let mut variants = Vec::new();
            while !self.eat("}") {
                self.attributes();
                let v = self.ident()?;
                let shape = if self.eat("(") {
                    let mut n = 0;
                    while !self.eat(")") {
                        self.ty()?;
                        n += 1;
                        self.eat(",");
                    }
                    VariantShape::Tuple(n)
                } else if self.eat("{") {
                    let mut names = Vec::new();
                    while !self.eat("}") {
                        names.push(self.ident()?);
                        self.expect(":")?;
                        self.ty()?;
                        self.eat(",");
                    }
                    VariantShape::Struct(names)
                } else {
                    if self.eat("=") {
                        self.expr()?;
                    }
                    VariantShape::Unit
                };
                variants.push((v, shape));
                self.eat(",");
            }
            return Ok(Some(Item::Enum { name, variants, copy }));
        }
        if self.eat_word("impl") {
            self.skip_generics();
            let first = self.ty()?;
            let target = if self.eat_word("for") { self.ty()? } else { first };
            self.skip_where();
            self.expect("{")?;
            let mut methods = Vec::new();
            while !self.eat("}") {
                self.attributes();
                self.eat_word("pub");
                if self.is_word("type") || self.is_word("const") && !matches!(self.peek_at(1), Tok::Ident(w) if w == "fn") {
                    while !self.eat(";") {
                        self.next();
                    }
                    continue;
                }
                methods.push(self.func()?);
            }
            return Ok(Some(Item::Impl(String::from(target.name()), methods)));
        }
        if self.eat_word("trait") {
            // Özellik bildirimleri yok sayılır (yöntemler impl bloklarından gelir).
            while !self.is("{") {
                self.next();
            }
            let mut depth = 0;
            loop {
                match self.next() {
                    Tok::P("{") => depth += 1,
                    Tok::P("}") => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Tok::Eof => break,
                    _ => {}
                }
            }
            return Ok(None);
        }
        if self.eat_word("type") {
            while !self.eat(";") {
                self.next();
            }
            return Ok(None);
        }
        self.err("öğe (fn, struct, enum, impl)")
    }

    fn skip_where(&mut self) {
        if self.eat_word("where") {
            while !self.is("{") && !self.is(";") && !self.at(&Tok::Eof) {
                self.next();
            }
        }
    }

    fn func(&mut self) -> R<Func> {
        while self.eat_word("const") || self.eat_word("async") || self.eat_word("unsafe") || self.eat_word("pub") {}
        if !self.eat_word("fn") {
            return self.err("fn");
        }
        let line = self.line();
        let name = self.ident()?;
        self.skip_generics();
        self.expect("(")?;
        let mut params = Vec::new();
        let mut method = false;
        while !self.eat(")") {
            // self, &self, &mut self, mut self
            let save = self.p;
            self.eat("&");
            if matches!(self.peek(), Tok::Lifetime) {
                self.next();
            }
            self.eat_word("mut");
            if self.eat_word("self") {
                method = true;
                if self.eat(":") {
                    self.ty()?;
                }
                self.eat(",");
                continue;
            }
            self.p = save;
            let pat = self.pattern()?;
            self.expect(":")?;
            let ty = self.ty()?;
            params.push((pat, ty));
            self.eat(",");
        }
        let ret = if self.eat("->") { Some(self.ty()?) } else { None };
        self.skip_where();
        if self.eat(";") {
            return Ok(Func {
                name,
                method,
                params,
                ret,
                body: Block { stmts: Vec::new(), tail: None, line },
                line,
            });
        }
        let body = self.block()?;
        Ok(Func { name, method, params, ret, body, line })
    }

    fn ty(&mut self) -> R<Type> {
        if self.eat("&") {
            if matches!(self.peek(), Tok::Lifetime) {
                self.next();
            }
            let m = self.eat_word("mut");
            return Ok(Type::Ref(Box::new(self.ty()?), m));
        }
        if self.eat("*") {
            self.eat_word("mut");
            self.eat_word("const");
            return self.ty();
        }
        if self.eat("(") {
            let mut items = Vec::new();
            while !self.eat(")") {
                items.push(self.ty()?);
                self.eat(",");
            }
            return Ok(if items.is_empty() { Type::Named(String::from("()"), Vec::new()) } else { Type::Tuple(items) });
        }
        if self.eat("[") {
            let inner = self.ty()?;
            if self.eat(";") {
                self.expr()?;
            }
            self.expect("]")?;
            return Ok(Type::Array(Box::new(inner)));
        }
        if self.eat("_") {
            return Ok(Type::Infer);
        }
        if self.eat_word("impl") || self.eat_word("dyn") {
            let t = self.ty()?;
            while self.eat("+") {
                self.ty()?;
            }
            return Ok(t);
        }
        if self.eat("!") {
            return Ok(Type::Named(String::from("!"), Vec::new()));
        }
        let mut name = self.ident()?;
        while self.eat("::") {
            name = self.ident()?;
        }
        let mut args = Vec::new();
        if self.eat("<") {
            loop {
                if matches!(self.peek(), Tok::Lifetime) {
                    self.next();
                } else {
                    args.push(self.ty()?);
                }
                if self.eat(",") {
                    continue;
                }
                if self.eat(">") {
                    break;
                }
                if self.is(">>") {
                    // `Vec<Vec<i64>>`: iç tür kapanışı dışarıdakiyle birlikte gelir.
                    self.t[self.p].tok = Tok::P(">");
                    break;
                }
                return self.err("'>'");
            }
        }
        if name == "Fn" || name == "FnMut" || name == "FnOnce" {
            if self.is("(") {
                self.ty()?;
            }
            if self.eat("->") {
                self.ty()?;
            }
        }
        Ok(Type::Named(name, args))
    }

    fn block(&mut self) -> R<Block> {
        let line = self.line();
        self.expect("{")?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        let mut tail = None;
        while !self.eat("}") {
            if self.at(&Tok::Eof) {
                return self.err("'}'");
            }
            if self.eat(";") {
                continue;
            }
            let line = self.line();
            if self.is("#") {
                self.attributes();
                continue;
            }
            if self.is_word("let") {
                self.next();
                let pat = self.pattern()?;
                let ty = if self.eat(":") { Some(self.ty()?) } else { None };
                let init = if self.eat("=") { Some(self.expr()?) } else { None };
                let otherwise = if self.eat_word("else") { Some(self.block()?) } else { None };
                self.expect(";")?;
                stmts.push(Stmt::Let(pat, ty, init, otherwise, line));
                continue;
            }
            if self.is_word("fn") {
                stmts.push(Stmt::Fn(self.func()?));
                continue;
            }
            if self.is_word("use") {
                while !self.eat(";") {
                    self.next();
                }
                continue;
            }
            let e = self.expr()?;
            if self.eat(";") {
                stmts.push(Stmt::Expr(e, line));
            } else if self.is("}") {
                tail = Some(Box::new(e));
            } else if ends_with_block(&e) {
                stmts.push(Stmt::Expr(e, line));
            } else {
                return self.err("';'");
            }
        }
        self.no_struct = saved;
        Ok(Block { stmts, tail, line })
    }

    fn pattern(&mut self) -> R<Pat> {
        let first = self.pattern_one()?;
        if self.is("|") && !matches!(self.peek_at(1), Tok::P("|")) {
            let mut list = alloc::vec![first];
            while self.eat("|") {
                list.push(self.pattern_one()?);
            }
            return Ok(Pat::Or(list));
        }
        Ok(first)
    }

    fn pattern_one(&mut self) -> R<Pat> {
        if self.eat("_") {
            return Ok(Pat::Wild);
        }
        if self.eat("..") {
            return Ok(Pat::Rest);
        }
        if self.eat("&") {
            self.eat_word("mut");
            return self.pattern_one();
        }
        if self.eat("(") {
            let mut items = Vec::new();
            while !self.eat(")") {
                items.push(self.pattern()?);
                self.eat(",");
            }
            return Ok(if items.len() == 1 { items.pop().unwrap() } else { Pat::Tuple(items) });
        }
        if self.eat("[") {
            let mut items = Vec::new();
            while !self.eat("]") {
                items.push(self.pattern()?);
                self.eat(",");
            }
            return Ok(Pat::Tuple(items));
        }
        let lit = match self.peek().clone() {
            Tok::Int(v) => Some(Expr::Int(v)),
            Tok::Float(v) => Some(Expr::Float(v)),
            Tok::Str(s) => Some(Expr::Str(s)),
            Tok::Char(c) => Some(Expr::Char(c)),
            Tok::P("-") => {
                self.next();
                match self.next() {
                    Tok::Int(v) => {
                        self.p -= 1;
                        Some(Expr::Int(-v))
                    }
                    Tok::Float(v) => {
                        self.p -= 1;
                        Some(Expr::Float(-v))
                    }
                    _ => return self.err("sayı"),
                }
            }
            Tok::Ident(w) if w == "true" || w == "false" => Some(Expr::Bool(w == "true")),
            _ => None,
        };
        if let Some(lit) = lit {
            self.next();
            if self.eat("..=") || self.eat("...") {
                let hi = self.pattern_one()?;
                let Pat::Lit(hi) = hi else { return self.err("aralık sonu") };
                return Ok(Pat::Range(Box::new(lit), hi));
            }
            if self.eat("..") {
                let Pat::Lit(hi) = self.pattern_one()? else { return self.err("aralık sonu") };
                // Dışlayıcı aralık: üst sınır bir eksik.
                let hi = match *hi {
                    Expr::Int(v) => Expr::Int(v - 1),
                    Expr::Char(c) => Expr::Char(char::from_u32(c as u32 - 1).unwrap_or(c)),
                    other => other,
                };
                return Ok(Pat::Range(Box::new(lit), Box::new(hi)));
            }
            return Ok(Pat::Lit(Box::new(lit)));
        }
        if self.eat_word("ref") {
            self.eat_word("mut");
            return Ok(Pat::Bind(self.ident()?, false));
        }
        let mutable = self.eat_word("mut");
        let mut path = alloc::vec![self.ident()?];
        while self.eat("::") {
            path.push(self.ident()?);
        }
        if self.eat("(") {
            let mut items = Vec::new();
            while !self.eat(")") {
                items.push(self.pattern()?);
                self.eat(",");
            }
            return Ok(Pat::Variant(path, items));
        }
        if self.is("{") && !self.no_struct || self.is("{") && path.len() > 1 {
            self.next();
            let mut fields = Vec::new();
            while !self.eat("}") {
                if self.eat("..") {
                    continue;
                }
                self.eat_word("ref");
                self.eat_word("mut");
                let f = self.ident()?;
                let p = if self.eat(":") { self.pattern()? } else { Pat::Bind(f.clone(), false) };
                fields.push((f, p));
                self.eat(",");
            }
            return Ok(Pat::Struct(path, fields));
        }
        if self.eat("@") {
            return self.pattern_one();
        }
        let name = &path[0];
        // Büyük harfle başlayan tek ad: birim varyant (None, Kırmızı) ya da sabit.
        if path.len() > 1 || name.chars().next().is_some_and(|c| c.is_uppercase()) {
            return Ok(Pat::Variant(path, Vec::new()));
        }
        Ok(Pat::Bind(path.pop().unwrap(), mutable))
    }

    pub fn expr(&mut self) -> R<Expr> {
        self.assign()
    }

    fn assign(&mut self) -> R<Expr> {
        let line = self.line();
        let left = self.range()?;
        let op = match self.peek() {
            Tok::P(p) if matches!(*p, "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "&=" | "|=" | "<<=" | ">>=") => *p,
            _ => return Ok(left),
        };
        self.next();
        let right = self.assign()?;
        Ok(Expr::Assign(op, Box::new(left), Box::new(right), line))
    }

    fn range(&mut self) -> R<Expr> {
        if self.is("..") || self.is("..=") {
            let inclusive = self.is("..=");
            self.next();
            let end = if self.range_end() { None } else { Some(Box::new(self.binary(0)?)) };
            return Ok(Expr::Range(None, end, inclusive));
        }
        let left = self.binary(0)?;
        if self.is("..") || self.is("..=") {
            let inclusive = self.is("..=");
            self.next();
            let end = if self.range_end() { None } else { Some(Box::new(self.binary(0)?)) };
            return Ok(Expr::Range(Some(Box::new(left)), end, inclusive));
        }
        Ok(left)
    }

    fn range_end(&self) -> bool {
        self.is(")") || self.is("]") || self.is("}") || self.is(",") || self.is(";") || (self.is("{") && self.no_struct)
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::P(p) => *p,
                Tok::Ident(w) if w == "as" => {
                    self.next();
                    let ty = self.ty()?;
                    left = Expr::Cast(Box::new(left), ty);
                    continue;
                }
                _ => break,
            };
            let prec = match op {
                "||" => 1,
                "&&" => 2,
                "==" | "!=" | "<" | ">" | "<=" | ">=" => 3,
                "|" => 4,
                "^" => 5,
                "&" => 6,
                "<<" | ">>" => 7,
                "+" | "-" => 8,
                "*" | "/" | "%" => 9,
                _ => break,
            };
            if prec <= min {
                break;
            }
            let line = self.line();
            self.next();
            let right = self.binary(prec)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right), line);
        }
        Ok(left)
    }

    fn unary(&mut self) -> R<Expr> {
        let line = self.line();
        if self.eat("-") {
            return Ok(Expr::Unary("-", Box::new(self.unary()?), line));
        }
        if self.eat("!") {
            return Ok(Expr::Unary("!", Box::new(self.unary()?), line));
        }
        if self.eat("*") {
            return Ok(Expr::Unary("*", Box::new(self.unary()?), line));
        }
        if self.eat("&") || self.is("&&") {
            if self.eat("&&") {
                // `&&x` → iki kat başvuru = bir başvuru.
            }
            let m = self.eat_word("mut");
            let inner = self.unary()?;
            return Ok(Expr::Unary(if m { "&mut" } else { "&" }, Box::new(inner), line));
        }
        self.postfix()
    }

    fn args(&mut self, close: &str) -> R<Vec<Expr>> {
        let mut out = Vec::new();
        let saved = self.no_struct;
        self.no_struct = false;
        while !self.eat(close) {
            out.push(self.expr()?);
            if !self.is(close) {
                self.expect(",")?;
            }
        }
        self.no_struct = saved;
        Ok(out)
    }

    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            let line = self.line();
            if self.eat("?") {
                e = Expr::Try(Box::new(e), line);
            } else if self.eat(".") {
                match self.next() {
                    Tok::Int(n) => e = Expr::Field(Box::new(e), alloc::format!("{}", n), line),
                    Tok::Float(f) => {
                        // `x.0.1` sözcük çözümleyicide ondalık olarak gelir.
                        let s = alloc::format!("{}", f);
                        let (a, b) = s.split_once('.').unwrap_or((&s, "0"));
                        e = Expr::Field(Box::new(e), String::from(a), line);
                        e = Expr::Field(Box::new(e), String::from(b), line);
                    }
                    Tok::Ident(name) => {
                        if name == "await" {
                            continue;
                        }
                        let turbofish = if self.is("::") && matches!(self.peek_at(1), Tok::P("<")) {
                            self.next();
                            self.next();
                            let t = self.ty()?;
                            while !self.eat(">") {
                                self.next();
                            }
                            Some(t)
                        } else {
                            None
                        };
                        if self.eat("(") {
                            let args = self.args(")")?;
                            e = Expr::Method(Box::new(e), name, turbofish, args, line);
                        } else {
                            e = Expr::Field(Box::new(e), name, line);
                        }
                    }
                    _ => {
                        self.p -= 1;
                        return self.err("alan ya da yöntem adı");
                    }
                }
            } else if self.is("(") && !matches!(e, Expr::Block(_) | Expr::If(..) | Expr::Match(..)) {
                self.next();
                let args = self.args(")")?;
                e = Expr::Call(Box::new(e), args, line);
            } else if self.is("[") {
                self.next();
                let saved = self.no_struct;
                self.no_struct = false;
                let idx = self.expr()?;
                self.no_struct = saved;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx), line);
            } else {
                return Ok(e);
            }
        }
    }

    fn label(&mut self) -> Option<String> {
        None
    }

    fn cond(&mut self) -> R<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = saved;
        e
    }

    fn primary(&mut self) -> R<Expr> {
        let line = self.line();
        let tok = self.next();
        Ok(match tok {
            Tok::Int(v) => Expr::Int(v),
            Tok::Float(v) => Expr::Float(v),
            Tok::Str(s) => Expr::Str(s),
            Tok::Char(c) => Expr::Char(c),
            Tok::Lifetime => {
                // Etiketli döngü: 'dış: loop { ... }
                self.expect(":")?;
                let label = Some(String::from("etiket"));
                let e = self.primary()?;
                return Ok(match e {
                    Expr::Loop(b, _) => Expr::Loop(b, label),
                    Expr::While(c, b, _) => Expr::While(c, b, label),
                    Expr::For(p, i, b, _) => Expr::For(p, i, b, label),
                    other => other,
                });
            }
            Tok::P("(") => {
                let saved = self.no_struct;
                self.no_struct = false;
                if self.eat(")") {
                    self.no_struct = saved;
                    return Ok(Expr::Unit);
                }
                let first = self.expr()?;
                let e = if self.eat(",") {
                    let mut items = alloc::vec![first];
                    while !self.eat(")") {
                        items.push(self.expr()?);
                        self.eat(",");
                    }
                    Expr::Tuple(items)
                } else {
                    self.expect(")")?;
                    first
                };
                self.no_struct = saved;
                e
            }
            Tok::P("[") => {
                let saved = self.no_struct;
                self.no_struct = false;
                if self.eat("]") {
                    self.no_struct = saved;
                    return Ok(Expr::Array(Vec::new()));
                }
                let first = self.expr()?;
                let e = if self.eat(";") {
                    let n = self.expr()?;
                    self.expect("]")?;
                    Expr::Repeat(Box::new(first), Box::new(n))
                } else {
                    let mut items = alloc::vec![first];
                    while self.eat(",") {
                        if self.is("]") {
                            break;
                        }
                        items.push(self.expr()?);
                    }
                    self.expect("]")?;
                    Expr::Array(items)
                };
                self.no_struct = saved;
                e
            }
            Tok::P("{") => {
                self.p -= 1;
                Expr::Block(self.block()?)
            }
            Tok::P("|") | Tok::P("||") => {
                let mut params = Vec::new();
                if tok == Tok::P("|") {
                    while !self.eat("|") {
                        let p = self.pattern_one()?;
                        if self.eat(":") {
                            self.ty()?;
                        }
                        params.push(p);
                        self.eat(",");
                    }
                }
                let body = if self.eat("->") {
                    self.ty()?;
                    Expr::Block(self.block()?)
                } else {
                    self.expr()?
                };
                Expr::Closure(params, Box::new(body))
            }
            Tok::Ident(w) => match w.as_str() {
                "true" => Expr::Bool(true),
                "false" => Expr::Bool(false),
                "move" => return self.primary(),
                "if" => return self.if_expr(),
                "match" => {
                    let scrut = self.cond()?;
                    self.expect("{")?;
                    let mut arms = Vec::new();
                    while !self.eat("}") {
                        let saved = self.no_struct;
                        self.no_struct = false;
                        let p = self.pattern()?;
                        self.no_struct = saved;
                        let guard = if self.eat_word("if") { Some(self.expr()?) } else { None };
                        self.expect("=>")?;
                        let body = self.expr()?;
                        if !self.eat(",") && !self.is("}") && !ends_with_block(&body) {
                            return self.err("','");
                        }
                        arms.push((p, guard, body));
                    }
                    Expr::Match(Box::new(scrut), arms, line)
                }
                "loop" => Expr::Loop(self.block()?, self.label()),
                "while" => {
                    if self.eat_word("let") {
                        let p = self.pattern()?;
                        self.expect("=")?;
                        let e = self.cond()?;
                        Expr::WhileLet(p, Box::new(e), self.block()?, None)
                    } else {
                        let c = self.cond()?;
                        Expr::While(Box::new(c), self.block()?, None)
                    }
                }
                "for" => {
                    let p = self.pattern()?;
                    if !self.eat_word("in") {
                        return self.err("in");
                    }
                    let e = self.cond()?;
                    Expr::For(p, Box::new(e), self.block()?, None)
                }
                "break" => {
                    if matches!(self.peek(), Tok::Lifetime) {
                        self.next();
                    }
                    let value = if self.is(";") || self.is("}") || self.is(",") {
                        None
                    } else {
                        Some(Box::new(self.expr()?))
                    };
                    Expr::Break(None, value)
                }
                "continue" => {
                    if matches!(self.peek(), Tok::Lifetime) {
                        self.next();
                    }
                    Expr::Continue(None)
                }
                "return" => {
                    let value = if self.is(";") || self.is("}") {
                        None
                    } else {
                        Some(Box::new(self.expr()?))
                    };
                    Expr::Return(value)
                }
                "unsafe" => Expr::Block(self.block()?),
                _ => {
                    let mut path = alloc::vec![w];
                    loop {
                        if self.is("::") {
                            self.next();
                            if self.is("<") {
                                self.skip_generics();
                                continue;
                            }
                            path.push(self.ident()?);
                        } else {
                            break;
                        }
                    }
                    if self.eat("!") {
                        let name = path.pop().unwrap();
                        let close = match self.next() {
                            Tok::P("(") => ")",
                            Tok::P("[") => "]",
                            Tok::P("{") => "}",
                            _ => return self.err("makro argümanları"),
                        };
                        if name == "vec" && !self.is(close) {
                            // vec![değer; sayı]
                            let saved = self.no_struct;
                            self.no_struct = false;
                            let first = self.expr()?;
                            if self.eat(";") {
                                let n = self.expr()?;
                                self.expect(close)?;
                                self.no_struct = saved;
                                return Ok(Expr::Macro(name, alloc::vec![Expr::Repeat(Box::new(first), Box::new(n))], line));
                            }
                            let mut args = alloc::vec![first];
                            while self.eat(",") {
                                if self.is(close) {
                                    break;
                                }
                                args.push(self.expr()?);
                            }
                            self.expect(close)?;
                            self.no_struct = saved;
                            return Ok(Expr::Macro(name, args, line));
                        }
                        let args = self.args(close)?;
                        return Ok(Expr::Macro(name, args, line));
                    }
                    if self.is("{") && !self.no_struct && path.last().is_some_and(|s| s.chars().next().is_some_and(|c| c.is_uppercase())) {
                        self.next();
                        let mut fields = Vec::new();
                        let mut base = None;
                        while !self.eat("}") {
                            if self.eat("..") {
                                base = Some(Box::new(self.expr()?));
                                continue;
                            }
                            let name = match self.next() {
                                Tok::Ident(n) => n,
                                Tok::Int(n) => alloc::format!("{}", n),
                                _ => return self.err("alan adı"),
                            };
                            let value = if self.eat(":") { self.expr()? } else { Expr::Path(alloc::vec![name.clone()], line) };
                            fields.push((name, value));
                            self.eat(",");
                        }
                        return Ok(Expr::StructLit(path, fields, base, line));
                    }
                    Expr::Path(path, line)
                }
            },
            _ => {
                self.p -= 1;
                return self.err("ifade");
            }
        })
    }

    fn if_expr(&mut self) -> R<Expr> {
        if self.eat_word("let") {
            let p = self.pattern()?;
            self.expect("=")?;
            let e = self.cond()?;
            let then = self.block()?;
            let otherwise = self.else_part()?;
            return Ok(Expr::IfLet(p, Box::new(e), then, otherwise));
        }
        let c = self.cond()?;
        let then = self.block()?;
        let otherwise = self.else_part()?;
        Ok(Expr::If(Box::new(c), then, otherwise))
    }

    fn else_part(&mut self) -> R<Option<Box<Expr>>> {
        if !self.eat_word("else") {
            return Ok(None);
        }
        if self.eat_word("if") {
            return Ok(Some(Box::new(self.if_expr()?)));
        }
        Ok(Some(Box::new(Expr::Block(self.block()?))))
    }
}

fn ends_with_block(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(..) | Expr::IfLet(..) | Expr::Match(..) | Expr::Block(_) | Expr::Loop(..) | Expr::While(..) | Expr::WhileLet(..) | Expr::For(..)
    )
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Int(v) => alloc::format!("{}", v),
        Tok::Float(v) => alloc::format!("{}", v),
        Tok::Str(s) => alloc::format!("\"{}\"", s),
        Tok::Char(c) => alloc::format!("'{}'", c),
        Tok::Ident(s) => s.clone(),
        Tok::Lifetime => String::from("'a"),
        Tok::P(p) => String::from(*p),
        Tok::Eof => String::from("dosya sonu"),
    }
}
