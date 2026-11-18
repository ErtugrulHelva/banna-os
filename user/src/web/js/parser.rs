//! JavaScript ayrıştırıcısı (ES2020'nin yaygın kısmı): değişken bildirimleri, fonksiyonlar,
//! ok fonksiyonları, sınıflar (alanlar, statik üyeler, get/set), yapı bozma, yayma ve
//! kalan parametreler, şablon dizgeler, isteğe bağlı zincir (`?.`), `??`, async/await,
//! etiketli döngüler ve otomatik noktalı virgül. Üreteçler (`function*`) desteklenmez.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::lexer::{Tok, Token, tokenize};

pub type Name = Rc<str>;

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(Name),
    Object(Vec<(PropKey, Pattern, Option<Expr>)>, Option<Name>),
    Array(Vec<Option<(Pattern, Option<Expr>)>>, Option<Box<Pattern>>),
    /// Atama hedefi olarak üye ifadesi (`a.b = ...` yapı bozmada).
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum PropKey {
    Name(Name),
    Computed(Expr),
}

#[derive(Debug, Clone)]
pub enum Prop {
    Value(PropKey, Expr),
    Getter(PropKey, Rc<Function>),
    Setter(PropKey, Rc<Function>),
    Spread(Expr),
}

#[derive(Debug)]
pub struct Function {
    pub name: Option<Name>,
    pub params: Vec<(Pattern, Option<Expr>)>,
    pub rest: Option<Pattern>,
    pub body: Vec<Stmt>,
    pub arrow: bool,
    pub is_async: bool,
    /// Ok fonksiyonunun gövdesi tek ifade mi.
    pub expression: bool,
}

#[derive(Debug)]
pub struct Class {
    pub name: Option<Name>,
    pub extends: Option<Expr>,
    pub constructor: Option<Rc<Function>>,
    pub methods: Vec<(PropKey, Rc<Function>, u8, bool)>,
    /// Örnek ve statik alanlar: (anahtar, ilk değer, statik).
    pub fields: Vec<(PropKey, Option<Expr>, bool)>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    Str(Name),
    Template(Vec<String>, Vec<Expr>),
    Tagged(Box<Expr>, Vec<String>, Vec<Expr>),
    Regex(Name, Name),
    Ident(Name),
    This,
    Super,
    NewTarget,
    Array(Vec<Option<Expr>>),
    Object(Vec<Prop>),
    Function(Rc<Function>),
    Class(Rc<Class>),
    Unary(&'static str, Box<Expr>),
    Update(&'static str, bool, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Logical(&'static str, Box<Expr>, Box<Expr>),
    Assign(&'static str, Box<Pattern>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>, bool),
    New(Box<Expr>, Vec<Expr>),
    Member(Box<Expr>, Box<Expr>, bool),
    Field(Box<Expr>, Name, bool),
    Sequence(Vec<Expr>),
    Spread(Box<Expr>),
    Await(Box<Expr>),
    Yield(Option<Box<Expr>>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Var(u8, Vec<(Pattern, Option<Expr>)>),
    Expr(Expr),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    ForIn(u8, Pattern, Expr, Box<Stmt>, bool),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Return(Option<Expr>),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr),
    Try(Vec<Stmt>, Option<(Option<Pattern>, Vec<Stmt>)>, Option<Vec<Stmt>>),
    Function(Rc<Function>),
    Class(Rc<Class>),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Labeled(Name, Box<Stmt>),
    Empty,
}

pub const VAR: u8 = 0;
pub const LET: u8 = 1;
pub const CONST: u8 = 2;

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    in_async: bool,
}

type R<T> = Result<T, String>;

pub fn parse(src: &str) -> R<Vec<Stmt>> {
    let mut p = Parser {
        toks: tokenize(src)?,
        pos: 0,
        in_async: false,
    };
    let mut body = Vec::new();
    while !p.at_eof() {
        body.push(p.statement()?);
    }
    Ok(body)
}

pub fn parse_expression(src: &str) -> R<Expr> {
    let mut p = Parser {
        toks: tokenize(src)?,
        pos: 0,
        in_async: true,
    };
    p.expression()
}

const RESERVED: &[&str] = &[
    "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
    "else", "export", "extends", "finally", "for", "function", "if", "import", "in", "instanceof",
    "new", "return", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while",
    "with", "null", "true", "false",
];

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn newline_before(&self) -> bool {
        self.toks[self.pos].newline
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(x) if *x == p)
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Tok::Ident(x) if &**x == w)
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

    fn error(&self, what: &str) -> String {
        alloc::format!(
            "{} bekleniyordu, {:?} bulundu (satır {})",
            what,
            self.peek(),
            self.toks[self.pos].line
        )
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(self.error(&alloc::format!("'{}'", p)))
        }
    }

    fn semicolon(&mut self) -> R<()> {
        if self.eat(";") || self.is("}") || self.at_eof() || self.newline_before() {
            Ok(())
        } else {
            Err(self.error("';'"))
        }
    }

    fn ident(&mut self) -> R<Name> {
        match self.next() {
            Tok::Ident(n) => Ok(n),
            _ => {
                self.pos -= 1;
                Err(self.error("ad"))
            }
        }
    }

    /// Özellik adı: ayrılmış sözcükler de olabilir (`obj.default`).
    fn property_name(&mut self) -> R<PropKey> {
        match self.next() {
            Tok::Ident(n) => Ok(PropKey::Name(n)),
            Tok::Str(s) => Ok(PropKey::Name(s)),
            Tok::Num(n) => Ok(PropKey::Name(Rc::from(super::number_to_string(n).as_str()))),
            Tok::Punct("[") => {
                let e = self.assignment()?;
                self.expect("]")?;
                Ok(PropKey::Computed(e))
            }
            Tok::Punct("#") => {
                let n = self.ident()?;
                Ok(PropKey::Name(Rc::from(alloc::format!("#{}", n).as_str())))
            }
            _ => {
                self.pos -= 1;
                Err(self.error("özellik adı"))
            }
        }
    }

    fn statement(&mut self) -> R<Stmt> {
        let tok = self.peek().clone();
        match tok {
            Tok::Punct("{") => {
                self.next();
                Ok(Stmt::Block(self.block_rest()?))
            }
            Tok::Punct(";") => {
                self.next();
                Ok(Stmt::Empty)
            }
            Tok::Ident(w) => match &*w {
                "var" | "const" => {
                    self.next();
                    let s = self.var_decl(if &*w == "var" { VAR } else { CONST }, true)?;
                    self.semicolon()?;
                    Ok(s)
                }
                "let" if matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[" | "{")) => {
                    self.next();
                    let s = self.var_decl(LET, true)?;
                    self.semicolon()?;
                    Ok(s)
                }
                "function" => Ok(Stmt::Function(self.function(false, false)?)),
                "async" if matches!(self.peek_at(1), Tok::Ident(f) if &**f == "function") && !self.toks[self.pos + 1].newline => {
                    self.next();
                    Ok(Stmt::Function(self.function(true, false)?))
                }
                "class" => Ok(Stmt::Class(self.class()?)),
                "if" => {
                    self.next();
                    self.expect("(")?;
                    let cond = self.expression()?;
                    self.expect(")")?;
                    let then = Box::new(self.statement()?);
                    let otherwise = if self.eat_word("else") {
                        Some(Box::new(self.statement()?))
                    } else {
                        None
                    };
                    Ok(Stmt::If(cond, then, otherwise))
                }
                "for" => self.for_statement(),
                "while" => {
                    self.next();
                    self.expect("(")?;
                    let cond = self.expression()?;
                    self.expect(")")?;
                    Ok(Stmt::While(cond, Box::new(self.statement()?)))
                }
                "do" => {
                    self.next();
                    let body = Box::new(self.statement()?);
                    if !self.eat_word("while") {
                        return Err(self.error("while"));
                    }
                    self.expect("(")?;
                    let cond = self.expression()?;
                    self.expect(")")?;
                    self.eat(";");
                    Ok(Stmt::DoWhile(body, cond))
                }
                "return" => {
                    self.next();
                    let value = if self.is(";") || self.is("}") || self.at_eof() || self.newline_before() {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    self.semicolon()?;
                    Ok(Stmt::Return(value))
                }
                "break" | "continue" => {
                    self.next();
                    let label = match self.peek() {
                        Tok::Ident(l) if !self.newline_before() && !RESERVED.contains(&&**l) => {
                            let l = l.clone();
                            self.next();
                            Some(l)
                        }
                        _ => None,
                    };
                    self.semicolon()?;
                    Ok(if &*w == "break" {
                        Stmt::Break(label)
                    } else {
                        Stmt::Continue(label)
                    })
                }
                "throw" => {
                    self.next();
                    let e = self.expression()?;
                    self.semicolon()?;
                    Ok(Stmt::Throw(e))
                }
                "try" => {
                    self.next();
                    self.expect("{")?;
                    let block = self.block_rest()?;
                    let catch = if self.eat_word("catch") {
                        let param = if self.eat("(") {
                            let p = self.binding_pattern()?;
                            self.expect(")")?;
                            Some(p)
                        } else {
                            None
                        };
                        self.expect("{")?;
                        Some((param, self.block_rest()?))
                    } else {
                        None
                    };
                    let finally = if self.eat_word("finally") {
                        self.expect("{")?;
                        Some(self.block_rest()?)
                    } else {
                        None
                    };
                    Ok(Stmt::Try(block, catch, finally))
                }
                "switch" => {
                    self.next();
                    self.expect("(")?;
                    let disc = self.expression()?;
                    self.expect(")")?;
                    self.expect("{")?;
                    let mut cases = Vec::new();
                    while !self.eat("}") {
                        let test = if self.eat_word("default") {
                            None
                        } else {
                            if !self.eat_word("case") {
                                return Err(self.error("case"));
                            }
                            Some(self.expression()?)
                        };
                        self.expect(":")?;
                        let mut body = Vec::new();
                        while !self.is_word("case") && !self.is_word("default") && !self.is("}") {
                            body.push(self.statement()?);
                        }
                        cases.push((test, body));
                    }
                    Ok(Stmt::Switch(disc, cases))
                }
                "import" if !matches!(self.peek_at(1), Tok::Punct("(" | ".")) => {
                    Err(String::from("ES modülleri (import) desteklenmiyor"))
                }
                "export" => {
                    self.next();
                    self.eat_word("default");
                    self.statement()
                }
                "debugger" => {
                    self.next();
                    self.semicolon()?;
                    Ok(Stmt::Empty)
                }
                _ if matches!(self.peek_at(1), Tok::Punct(":")) && !RESERVED.contains(&&*w) => {
                    self.next();
                    self.next();
                    Ok(Stmt::Labeled(w.clone(), Box::new(self.statement()?)))
                }
                _ => {
                    let e = self.expression()?;
                    self.semicolon()?;
                    Ok(Stmt::Expr(e))
                }
            },
            _ => {
                let e = self.expression()?;
                self.semicolon()?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn block_rest(&mut self) -> R<Vec<Stmt>> {
        let mut out = Vec::new();
        while !self.eat("}") {
            if self.at_eof() {
                return Err(self.error("'}'"));
            }
            out.push(self.statement()?);
        }
        Ok(out)
    }

    fn var_decl(&mut self, kind: u8, with_init: bool) -> R<Stmt> {
        let mut decls = Vec::new();
        loop {
            let pattern = self.binding_pattern()?;
            let init = if with_init && self.eat("=") {
                Some(self.assignment()?)
            } else {
                None
            };
            decls.push((pattern, init));
            if !self.eat(",") {
                break;
            }
        }
        Ok(Stmt::Var(kind, decls))
    }

    fn for_statement(&mut self) -> R<Stmt> {
        self.next();
        if self.is_word("await") {
            self.next();
        }
        self.expect("(")?;
        let kind = match self.peek() {
            Tok::Ident(w) if &**w == "var" => Some(VAR),
            Tok::Ident(w) if &**w == "let" => Some(LET),
            Tok::Ident(w) if &**w == "const" => Some(CONST),
            _ => None,
        };
        let init: Option<Box<Stmt>> = if self.is(";") {
            None
        } else if let Some(kind) = kind {
            self.next();
            let pattern = self.binding_pattern()?;
            if self.is_word("of") || self.is_word("in") {
                let of = self.is_word("of");
                self.next();
                let iter = if of { self.assignment()? } else { self.expression()? };
                self.expect(")")?;
                let body = self.statement()?;
                return Ok(Stmt::ForIn(kind, pattern, iter, Box::new(body), of));
            }
            let first_init = if self.eat("=") {
                Some(self.assignment_no_in()?)
            } else {
                None
            };
            let mut decls = alloc::vec![(pattern, first_init)];
            while self.eat(",") {
                let p = self.binding_pattern()?;
                let i = if self.eat("=") {
                    Some(self.assignment_no_in()?)
                } else {
                    None
                };
                decls.push((p, i));
            }
            Some(Box::new(Stmt::Var(kind, decls)))
        } else {
            let start = self.pos;
            // `for (--i; ...)`, `for (!a; ...)`: sol taraf ifadesi değilse sıradan ifade.
            if let Ok(e) = self.left_hand_side()
                && (self.is_word("of") || self.is_word("in"))
            {
                let of = self.is_word("of");
                self.next();
                let iter = self.expression()?;
                self.expect(")")?;
                let body = self.statement()?;
                let pattern = to_pattern(e)?;
                return Ok(Stmt::ForIn(255, pattern, iter, Box::new(body), of));
            }
            self.pos = start;
            Some(Box::new(Stmt::Expr(self.expression()?)))
        };
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expression()?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expression()?) };
        self.expect(")")?;
        Ok(Stmt::For(init, test, update, Box::new(self.statement()?)))
    }

    fn assignment_no_in(&mut self) -> R<Expr> {
        self.assignment()
    }

    fn binding_pattern(&mut self) -> R<Pattern> {
        if self.eat("[") {
            let mut items = Vec::new();
            let mut rest = None;
            while !self.eat("]") {
                if self.eat(",") {
                    items.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_pattern()?));
                    self.eat(",");
                    continue;
                }
                let p = self.binding_pattern()?;
                let d = if self.eat("=") { Some(self.assignment()?) } else { None };
                items.push(Some((p, d)));
                if !self.is("]") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Array(items, rest));
        }
        if self.eat("{") {
            let mut props = Vec::new();
            let mut rest = None;
            while !self.eat("}") {
                if self.eat("...") {
                    rest = Some(self.ident()?);
                    self.eat(",");
                    continue;
                }
                let key = self.property_name()?;
                let target = if self.eat(":") {
                    self.binding_pattern()?
                } else {
                    match &key {
                        PropKey::Name(n) => Pattern::Ident(n.clone()),
                        _ => return Err(self.error("':'")),
                    }
                };
                let d = if self.eat("=") { Some(self.assignment()?) } else { None };
                props.push((key, target, d));
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Object(props, rest));
        }
        Ok(Pattern::Ident(self.ident()?))
    }

    fn params(&mut self) -> R<(Vec<(Pattern, Option<Expr>)>, Option<Pattern>)> {
        self.expect("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        while !self.eat(")") {
            if self.eat("...") {
                rest = Some(self.binding_pattern()?);
                self.eat(",");
                continue;
            }
            let p = self.binding_pattern()?;
            let d = if self.eat("=") { Some(self.assignment()?) } else { None };
            params.push((p, d));
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        Ok((params, rest))
    }

    fn function_body(&mut self, is_async: bool) -> R<Vec<Stmt>> {
        let saved = self.in_async;
        self.in_async = is_async;
        self.expect("{")?;
        let body = self.block_rest();
        self.in_async = saved;
        body
    }

    fn function(&mut self, is_async: bool, expression: bool) -> R<Rc<Function>> {
        self.next();
        if self.eat("*") {
            return Err(String::from("üreteç fonksiyonları (function*) desteklenmiyor"));
        }
        let name = match self.peek() {
            Tok::Ident(n) if !self.is("(") => {
                let n = n.clone();
                self.next();
                Some(n)
            }
            _ if expression => None,
            _ => return Err(self.error("fonksiyon adı")),
        };
        let (params, rest) = self.params()?;
        let body = self.function_body(is_async)?;
        Ok(Rc::new(Function {
            name,
            params,
            rest,
            body,
            arrow: false,
            is_async,
            expression: false,
        }))
    }

    fn method(&mut self, name: Option<Name>, is_async: bool) -> R<Rc<Function>> {
        let (params, rest) = self.params()?;
        let body = self.function_body(is_async)?;
        Ok(Rc::new(Function {
            name,
            params,
            rest,
            body,
            arrow: false,
            is_async,
            expression: false,
        }))
    }

    fn class(&mut self) -> R<Rc<Class>> {
        self.next();
        let name = match self.peek() {
            Tok::Ident(n) if &**n != "extends" => {
                let n = n.clone();
                self.next();
                Some(n)
            }
            _ => None,
        };
        let extends = if self.eat_word("extends") {
            Some(self.left_hand_side()?)
        } else {
            None
        };
        self.expect("{")?;
        let mut class = Class {
            name,
            extends,
            constructor: None,
            methods: Vec::new(),
            fields: Vec::new(),
        };
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let is_static = self.is_word("static")
                && !matches!(self.peek_at(1), Tok::Punct("(" | "="));
            if is_static {
                self.next();
                if self.is("{") {
                    // Statik başlatma bloğu: hemen çalışan alan gibi.
                    self.next();
                    let body = self.block_rest()?;
                    let f = Rc::new(Function {
                        name: None,
                        params: Vec::new(),
                        rest: None,
                        body,
                        arrow: true,
                        is_async: false,
                        expression: false,
                    });
                    class.fields.push((
                        PropKey::Name(Rc::from("__static_init")),
                        Some(Expr::Call(Box::new(Expr::Function(f)), Vec::new(), false)),
                        true,
                    ));
                    continue;
                }
            }
            let mut is_async = false;
            if self.is_word("async") && !matches!(self.peek_at(1), Tok::Punct("(" | "=")) {
                self.next();
                is_async = true;
            }
            self.eat("*");
            let mut kind = 0u8;
            if (self.is_word("get") || self.is_word("set"))
                && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}"))
            {
                kind = if self.is_word("get") { 1 } else { 2 };
                self.next();
            }
            let key = self.property_name()?;
            if self.is("(") {
                let fname = match &key {
                    PropKey::Name(n) => Some(n.clone()),
                    _ => None,
                };
                let f = self.method(fname, is_async)?;
                if kind == 0 && !is_static && matches!(&key, PropKey::Name(n) if &**n == "constructor") {
                    class.constructor = Some(f);
                } else {
                    class.methods.push((key, f, kind, is_static));
                }
            } else {
                let init = if self.eat("=") { Some(self.assignment()?) } else { None };
                self.semicolon()?;
                class.fields.push((key, init, is_static));
            }
        }
        Ok(Rc::new(class))
    }

    pub fn expression(&mut self) -> R<Expr> {
        let first = self.assignment()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = alloc::vec![first];
        while self.eat(",") {
            list.push(self.assignment()?);
        }
        Ok(Expr::Sequence(list))
    }

    /// `(a, b) => ...` ya da `x => ...` ok fonksiyonu mu (ileriye bakarak).
    fn arrow_ahead(&self) -> bool {
        match self.peek() {
            Tok::Ident(_) => matches!(self.peek_at(1), Tok::Punct("=>")),
            Tok::Punct("(") => {
                let mut depth = 0;
                let mut i = self.pos;
                while i < self.toks.len() {
                    match &self.toks[i].tok {
                        Tok::Punct("(" | "[" | "{") => depth += 1,
                        Tok::Punct(")" | "]" | "}") => {
                            depth -= 1;
                            if depth == 0 {
                                return matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::Punct("=>")));
                            }
                        }
                        Tok::Eof => return false,
                        _ => {}
                    }
                    i += 1;
                }
                false
            }
            _ => false,
        }
    }

    fn arrow(&mut self, is_async: bool) -> R<Expr> {
        let (params, rest) = if let Tok::Ident(n) = self.peek().clone() {
            self.next();
            (alloc::vec![(Pattern::Ident(n), None)], None)
        } else {
            self.params()?
        };
        self.expect("=>")?;
        let saved = self.in_async;
        self.in_async = is_async;
        let (body, expression) = if self.is("{") {
            self.next();
            (self.block_rest()?, false)
        } else {
            (alloc::vec![Stmt::Return(Some(self.assignment()?))], true)
        };
        self.in_async = saved;
        Ok(Expr::Function(Rc::new(Function {
            name: None,
            params,
            rest,
            body,
            arrow: true,
            is_async,
            expression,
        })))
    }

    fn assignment(&mut self) -> R<Expr> {
        if self.is_word("async")
            && !self.toks[self.pos + 1].newline
            && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("("))
        {
            let save = self.pos;
            self.next();
            if self.arrow_ahead() {
                return self.arrow(true);
            }
            self.pos = save;
        }
        if self.arrow_ahead() {
            return self.arrow(false);
        }
        if self.is_word("yield") {
            self.next();
            let arg = if self.is(")") || self.is("]") || self.is("}") || self.is(",") || self.is(";") || self.newline_before() {
                None
            } else {
                Some(Box::new(self.assignment()?))
            };
            return Ok(Expr::Yield(arg));
        }
        let left = self.conditional()?;
        let op = match self.peek() {
            Tok::Punct(p) if matches!(*p, "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "**=" | "<<=" | ">>=" | ">>>=" | "&=" | "|=" | "^=" | "&&=" | "||=" | "??=") => *p,
            _ => return Ok(left),
        };
        self.next();
        let target = if op == "=" {
            to_pattern(left)?
        } else {
            Pattern::Expr(Box::new(left))
        };
        let value = self.assignment()?;
        Ok(Expr::Assign(op, Box::new(target), Box::new(value)))
    }

    fn conditional(&mut self) -> R<Expr> {
        let cond = self.binary(0)?;
        if self.eat("?") {
            let then = self.assignment()?;
            self.expect(":")?;
            let otherwise = self.assignment()?;
            return Ok(Expr::Conditional(Box::new(cond), Box::new(then), Box::new(otherwise)));
        }
        Ok(cond)
    }

    fn binary_op(&self) -> Option<(&'static str, u8)> {
        let op = match self.peek() {
            Tok::Punct(p) => *p,
            Tok::Ident(w) if &**w == "instanceof" => "instanceof",
            Tok::Ident(w) if &**w == "in" => "in",
            _ => return None,
        };
        let prec = match op {
            "??" => 1,
            "||" => 2,
            "&&" => 3,
            "|" => 4,
            "^" => 5,
            "&" => 6,
            "==" | "!=" | "===" | "!==" => 7,
            "<" | ">" | "<=" | ">=" | "instanceof" | "in" => 8,
            "<<" | ">>" | ">>>" => 9,
            "+" | "-" => 10,
            "*" | "/" | "%" => 11,
            "**" => 12,
            _ => return None,
        };
        Some((op, prec))
    }

    fn binary(&mut self, min: u8) -> R<Expr> {
        let mut left = self.unary()?;
        while let Some((op, prec)) = self.binary_op() {
            if prec <= min && !(op == "**" && prec == min && min == 12)
                && prec <= min {
                    break;
                }
            self.next();
            // `**` sağdan birleşir.
            let right = if op == "**" {
                self.binary(prec - 1)?
            } else {
                self.binary(prec)?
            };
            left = if matches!(op, "&&" | "||" | "??") {
                Expr::Logical(op, Box::new(left), Box::new(right))
            } else {
                Expr::Binary(op, Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> R<Expr> {
        let op = match self.peek() {
            Tok::Punct(p @ ("!" | "-" | "+" | "~")) => Some(*p),
            Tok::Ident(w) if matches!(&**w, "typeof" | "void" | "delete") => Some(match &**w {
                "typeof" => "typeof",
                "void" => "void",
                _ => "delete",
            }),
            Tok::Ident(w) if &**w == "await" && self.in_async => {
                self.next();
                return Ok(Expr::Await(Box::new(self.unary()?)));
            }
            Tok::Punct(p @ ("++" | "--")) => {
                let p = *p;
                self.next();
                let target = self.unary()?;
                return Ok(Expr::Update(p, true, Box::new(target)));
            }
            _ => None,
        };
        if let Some(op) = op {
            self.next();
            return Ok(Expr::Unary(op, Box::new(self.unary()?)));
        }
        let e = self.postfix()?;
        if self.is("**") {
            self.next();
            let right = self.unary()?;
            return Ok(Expr::Binary("**", Box::new(e), Box::new(right)));
        }
        Ok(e)
    }

    fn postfix(&mut self) -> R<Expr> {
        let e = self.left_hand_side()?;
        if !self.newline_before()
            && let Tok::Punct(p @ ("++" | "--")) = self.peek()
        {
            let p = *p;
            self.next();
            return Ok(Expr::Update(p, false, Box::new(e)));
        }
        Ok(e)
    }

    fn arguments(&mut self) -> R<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.eat(")") {
            if self.eat("...") {
                args.push(Expr::Spread(Box::new(self.assignment()?)));
            } else {
                args.push(self.assignment()?);
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        Ok(args)
    }

    fn left_hand_side(&mut self) -> R<Expr> {
        let mut e = if self.is_word("new") {
            self.next();
            if self.eat(".") {
                self.ident()?;
                Expr::NewTarget
            } else {
                let callee = self.member_only()?;
                let args = if self.eat("(") { self.arguments()? } else { Vec::new() };
                Expr::New(Box::new(callee), args)
            }
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let name = match self.property_name()? {
                    PropKey::Name(n) => n,
                    PropKey::Computed(_) => return Err(self.error("özellik adı")),
                };
                e = Expr::Field(Box::new(e), name, false);
            } else if self.eat("?.") {
                if self.eat("(") {
                    e = Expr::Call(Box::new(e), self.arguments()?, true);
                } else if self.eat("[") {
                    let key = self.expression()?;
                    self.expect("]")?;
                    e = Expr::Member(Box::new(e), Box::new(key), true);
                } else {
                    let name = match self.property_name()? {
                        PropKey::Name(n) => n,
                        PropKey::Computed(_) => return Err(self.error("özellik adı")),
                    };
                    e = Expr::Field(Box::new(e), name, true);
                }
            } else if self.eat("[") {
                let key = self.expression()?;
                self.expect("]")?;
                e = Expr::Member(Box::new(e), Box::new(key), false);
            } else if self.eat("(") {
                e = Expr::Call(Box::new(e), self.arguments()?, false);
            } else if let Tok::Template(parts, exprs) = self.peek().clone() {
                self.next();
                let exprs = exprs
                    .iter()
                    .map(|s| parse_expression(s))
                    .collect::<R<Vec<_>>>()?;
                e = Expr::Tagged(Box::new(e), parts, exprs);
            } else {
                return Ok(e);
            }
        }
    }

    /// `new X.Y(...)` için çağrısız üye zinciri.
    fn member_only(&mut self) -> R<Expr> {
        let mut e = if self.is_word("new") {
            self.next();
            let callee = self.member_only()?;
            let args = if self.eat("(") { self.arguments()? } else { Vec::new() };
            Expr::New(Box::new(callee), args)
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let name = match self.property_name()? {
                    PropKey::Name(n) => n,
                    _ => return Err(self.error("özellik adı")),
                };
                e = Expr::Field(Box::new(e), name, false);
            } else if self.eat("[") {
                let key = self.expression()?;
                self.expect("]")?;
                e = Expr::Member(Box::new(e), Box::new(key), false);
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> R<Expr> {
        let tok = self.next();
        Ok(match tok {
            Tok::Num(n) => Expr::Num(n),
            Tok::Str(s) => Expr::Str(s),
            Tok::Template(parts, exprs) => {
                let exprs = exprs
                    .iter()
                    .map(|s| parse_expression(s))
                    .collect::<R<Vec<_>>>()?;
                Expr::Template(parts, exprs)
            }
            Tok::Regex(p, f) => Expr::Regex(Rc::from(p.as_str()), Rc::from(f.as_str())),
            Tok::Punct("(") => {
                let e = self.expression()?;
                self.expect(")")?;
                e
            }
            Tok::Punct("[") => {
                let mut items = Vec::new();
                while !self.eat("]") {
                    if self.is(",") {
                        self.next();
                        items.push(None);
                        continue;
                    }
                    if self.eat("...") {
                        items.push(Some(Expr::Spread(Box::new(self.assignment()?))));
                    } else {
                        items.push(Some(self.assignment()?));
                    }
                    if !self.is("]") {
                        self.expect(",")?;
                    }
                }
                Expr::Array(items)
            }
            Tok::Punct("{") => self.object_literal()?,
            Tok::Ident(w) => match &*w {
                "function" => {
                    self.pos -= 1;
                    Expr::Function(self.function(false, true)?)
                }
                "async" if self.is_word("function") => Expr::Function(self.function(true, true)?),
                "class" => {
                    self.pos -= 1;
                    Expr::Class(self.class()?)
                }
                "this" => Expr::This,
                "super" => Expr::Super,
                "null" => Expr::Ident(Rc::from("null")),
                "true" | "false" | "undefined" => Expr::Ident(w),
                "import" => {
                    return Err(String::from("dinamik import() desteklenmiyor"));
                }
                _ => Expr::Ident(w),
            },
            Tok::Punct("#") => {
                let n = self.ident()?;
                Expr::Ident(Rc::from(alloc::format!("#{}", n).as_str()))
            }
            other => {
                self.pos -= 1;
                let _ = other;
                return Err(self.error("ifade"));
            }
        })
    }

    fn object_literal(&mut self) -> R<Expr> {
        let mut props = Vec::new();
        while !self.eat("}") {
            if self.eat("...") {
                props.push(Prop::Spread(self.assignment()?));
            } else {
                let mut is_async = false;
                if self.is_word("async") && !matches!(self.peek_at(1), Tok::Punct("(" | ":" | "," | "}")) {
                    self.next();
                    is_async = true;
                }
                self.eat("*");
                let accessor = if (self.is_word("get") || self.is_word("set"))
                    && !matches!(self.peek_at(1), Tok::Punct("(" | ":" | "," | "}"))
                {
                    let g = self.is_word("get");
                    self.next();
                    Some(g)
                } else {
                    None
                };
                let key = self.property_name()?;
                if let Some(getter) = accessor {
                    let f = self.method(None, false)?;
                    props.push(if getter {
                        Prop::Getter(key, f)
                    } else {
                        Prop::Setter(key, f)
                    });
                } else if self.is("(") {
                    let name = match &key {
                        PropKey::Name(n) => Some(n.clone()),
                        _ => None,
                    };
                    props.push(Prop::Value(key, Expr::Function(self.method(name, is_async)?)));
                } else if self.eat(":") {
                    props.push(Prop::Value(key, self.assignment()?));
                } else {
                    let PropKey::Name(n) = &key else {
                        return Err(self.error("':'"));
                    };
                    // Kısaltma: `{ a }` ya da yapı bozma varsayılanı `{ a = 1 }`.
                    let value = if self.eat("=") {
                        let d = self.assignment()?;
                        Expr::Assign(
                            "=",
                            Box::new(Pattern::Ident(n.clone())),
                            Box::new(d),
                        )
                    } else {
                        Expr::Ident(n.clone())
                    };
                    props.push(Prop::Value(key, value));
                }
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(Expr::Object(props))
    }
}

/// İfadeyi atama hedefine çevirir (`[a, b] = ...`, `({x} = ...)`).
pub fn to_pattern(e: Expr) -> R<Pattern> {
    Ok(match e {
        Expr::Ident(n) => Pattern::Ident(n),
        Expr::Field(..) | Expr::Member(..) => Pattern::Expr(Box::new(e)),
        Expr::Array(items) => {
            let mut out = Vec::new();
            let mut rest = None;
            for item in items {
                match item {
                    None => out.push(None),
                    Some(Expr::Spread(inner)) => rest = Some(Box::new(to_pattern(*inner)?)),
                    Some(Expr::Assign("=", target, default)) => out.push(Some((*target, Some(*default)))),
                    Some(other) => out.push(Some((to_pattern(other)?, None))),
                }
            }
            Pattern::Array(out, rest)
        }
        Expr::Object(props) => {
            let mut out = Vec::new();
            let mut rest = None;
            for p in props {
                match p {
                    Prop::Value(key, Expr::Assign("=", target, default)) => out.push((key, *target, Some(*default))),
                    Prop::Value(key, value) => out.push((key, to_pattern(value)?, None)),
                    Prop::Spread(Expr::Ident(n)) => rest = Some(n),
                    _ => return Err(String::from("geçersiz yapı bozma")),
                }
            }
            Pattern::Object(out, rest)
        }
        Expr::Assign("=", target, _) => *target,
        _ => return Err(String::from("geçersiz atama hedefi")),
    })
}
