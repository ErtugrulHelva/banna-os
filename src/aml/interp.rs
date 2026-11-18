//! AML bytecode yorumlayıcısı: tablo yükleme ve metod çalıştırma.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::namespace::{NamePath, NameSeg, Namespace, NodeId, ROOT};
use super::region;
use super::value::{
    BufferField, FieldKind, FieldUnit, Object, OpRegion, RegionSpace, Target, Value,
};
use crate::arch::tsc;

const MAX_DEPTH: u32 = 48;
const MAX_LOOP_ITERATIONS: u64 = 2_000_000;

/// Alanlar tanı loglarında (`{:?}`) okunur.
#[allow(dead_code)]
#[derive(Debug)]
pub enum AmlError {
    UnexpectedEnd,
    UnknownOpcode(u16, usize),
    NameNotFound(String),
    Type(&'static str),
    Timeout,
    TooDeep,
    Fatal,
}

pub type Result<T> = core::result::Result<T, AmlError>;

pub enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

pub struct Frame {
    pub locals: [Value; 8],
    pub args: [Value; 7],
    pub scope: NodeId,
    created: Vec<NodeId>,
    method: bool,
}

impl Frame {
    pub fn load(scope: NodeId) -> Self {
        Self {
            locals: Default::default(),
            args: Default::default(),
            scope,
            created: Vec::new(),
            method: false,
        }
    }

    fn method(scope: NodeId, args: Vec<Value>) -> Self {
        let mut frame = Self::load(scope);
        frame.method = true;
        for (slot, value) in frame.args.iter_mut().zip(args) {
            *slot = value;
        }
        frame
    }
}

pub struct Cursor {
    code: &'static [u8],
    pub pos: usize,
    end: usize,
}

impl Cursor {
    pub fn new(code: &'static [u8]) -> Self {
        Self {
            code,
            pos: 0,
            end: code.len(),
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.end
    }

    fn peek(&self) -> Result<u8> {
        if self.pos < self.end {
            Ok(self.code[self.pos])
        } else {
            Err(AmlError::UnexpectedEnd)
        }
    }

    fn peek_at(&self, offset: usize) -> Result<u8> {
        if self.pos + offset < self.end {
            Ok(self.code[self.pos + offset])
        } else {
            Err(AmlError::UnexpectedEnd)
        }
    }

    fn byte(&mut self) -> Result<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    fn uint(&mut self, bytes: usize) -> Result<u64> {
        let mut value = 0u64;
        for i in 0..bytes {
            value |= (self.byte()? as u64) << (8 * i);
        }
        Ok(value)
    }

    fn pkg_length_raw(&mut self) -> Result<usize> {
        let lead = self.byte()?;
        let count = (lead >> 6) as usize;
        if count == 0 {
            return Ok((lead & 0x3f) as usize);
        }
        let mut length = (lead & 0x0f) as usize;
        for i in 0..count {
            length |= (self.byte()? as usize) << (4 + 8 * i);
        }
        Ok(length)
    }

    /// PkgLength'i okur ve paketin bittiği mutlak konumu döndürür.
    fn pkg_end(&mut self) -> Result<usize> {
        let start = self.pos;
        let end = start + self.pkg_length_raw()?;
        if end > self.end {
            return Err(AmlError::UnexpectedEnd);
        }
        Ok(end)
    }

    fn name_seg(&mut self) -> Result<NameSeg> {
        let mut seg = [0u8; 4];
        for b in seg.iter_mut() {
            *b = self.byte()?;
        }
        Ok(seg)
    }

    fn name_string(&mut self) -> Result<NamePath> {
        let mut path = NamePath {
            absolute: false,
            parents: 0,
            segments: Vec::new(),
        };
        if self.peek()? == b'\\' {
            self.pos += 1;
            path.absolute = true;
        } else {
            while self.peek()? == b'^' {
                self.pos += 1;
                path.parents += 1;
            }
        }
        match self.peek()? {
            0x00 => self.pos += 1,
            0x2e => {
                self.pos += 1;
                path.segments.push(self.name_seg()?);
                path.segments.push(self.name_seg()?);
            }
            0x2f => {
                self.pos += 1;
                let count = self.byte()?;
                for _ in 0..count {
                    path.segments.push(self.name_seg()?);
                }
            }
            _ => path.segments.push(self.name_seg()?),
        }
        Ok(path)
    }
}

fn is_name_start(b: u8) -> bool {
    matches!(b, b'\\' | b'^' | b'_' | b'A'..=b'Z' | 0x2e | 0x2f)
}

fn path_text(path: &NamePath) -> String {
    let mut out = String::new();
    if path.absolute {
        out.push('\\');
    }
    for _ in 0..path.parents {
        out.push('^');
    }
    for (i, seg) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(core::str::from_utf8(seg).unwrap_or("????"));
    }
    out
}

pub struct Interpreter<'a> {
    pub ns: &'a mut Namespace,
    pub int_mask: u64,
    depth: u32,
    deadline_ns: u64,
}

impl<'a> Interpreter<'a> {
    pub fn new(ns: &'a mut Namespace, int_mask: u64) -> Self {
        Self {
            ns,
            int_mask,
            depth: 0,
            deadline_ns: tsc::now_ns() + 10_000_000_000,
        }
    }

    pub fn load_table(&mut self, code: &'static [u8]) -> Result<()> {
        let mut frame = Frame::load(ROOT);
        let mut cur = Cursor::new(code);
        self.exec_body(&mut frame, &mut cur, ROOT, code.len())?;
        Ok(())
    }

    pub fn invoke(&mut self, id: NodeId, args: Vec<Value>) -> Result<Value> {
        if self.depth >= MAX_DEPTH {
            return Err(AmlError::TooDeep);
        }
        match self.ns.object(id).clone() {
            Object::NativeMethod { func, .. } => Ok(func(&args)),
            Object::Method { code, .. } => {
                self.depth += 1;
                let mut frame = Frame::method(id, args);
                let mut cur = Cursor::new(code);
                let result = self.exec_list(&mut frame, &mut cur, code.len());
                for node in frame.created.iter().rev() {
                    self.ns.remove(*node);
                }
                self.depth -= 1;
                match result? {
                    Flow::Return(value) => Ok(value),
                    _ => Ok(Value::Integer(0)),
                }
            }
            Object::Name(value) => Ok(value),
            _ => Err(AmlError::Type("not a method")),
        }
    }

    fn exec_list(&mut self, frame: &mut Frame, cur: &mut Cursor, end: usize) -> Result<Flow> {
        while cur.pos < end {
            match self.exec_term(frame, cur)? {
                Flow::Next => {}
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Next)
    }

    /// Bir kapsamın gövdesini çalıştırır; tablo yüklerken hatalı gövde atlanır.
    fn exec_body(
        &mut self,
        frame: &mut Frame,
        cur: &mut Cursor,
        scope: NodeId,
        end: usize,
    ) -> Result<Flow> {
        let saved_scope = frame.scope;
        frame.scope = scope;
        let saved_end = cur.end;
        cur.end = end;
        let result = if frame.method {
            self.exec_list(frame, cur, end)
        } else {
            loop {
                if cur.pos >= end {
                    break Ok(Flow::Next);
                }
                let start = cur.pos;
                match self.exec_term(frame, cur) {
                    Ok(_) => {}
                    Err(error) => {
                        crate::log!(
                            "aml: load error in {} at +{:#x}: {:?}; skipping rest of scope",
                            self.ns.path_of(scope),
                            start,
                            error
                        );
                        break Ok(Flow::Next);
                    }
                }
            }
        };
        cur.end = saved_end;
        cur.pos = end;
        frame.scope = saved_scope;
        result
    }

    fn create(&mut self, frame: &mut Frame, path: &NamePath, object: Object) -> Result<NodeId> {
        let (parent, name) = self
            .ns
            .resolve_parent(frame.scope, path)
            .ok_or_else(|| AmlError::NameNotFound(path_text(path)))?;
        let existed = self.ns.child(parent, name).is_some();
        let id = self.ns.add(parent, name, object);
        if frame.method && !existed {
            frame.created.push(id);
        }
        Ok(id)
    }

    fn check_deadline(&self) -> Result<()> {
        if tsc::now_ns() > self.deadline_ns {
            Err(AmlError::Timeout)
        } else {
            Ok(())
        }
    }

    fn exec_term(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Flow> {
        let op = cur.peek()?;
        match op {
            0x08 => {
                cur.byte()?;
                let path = cur.name_string()?;
                let value = self.eval(frame, cur)?;
                self.create(frame, &path, Object::Name(value))?;
            }
            0x10 => {
                cur.byte()?;
                let end = cur.pkg_end()?;
                let path = cur.name_string()?;
                let node = match self.ns.resolve(frame.scope, &path) {
                    Some(node) => node,
                    None => self.create(frame, &path, Object::Scope)?,
                };
                return self.exec_body(frame, cur, node, end);
            }
            0x14 => {
                cur.byte()?;
                let end = cur.pkg_end()?;
                let path = cur.name_string()?;
                let flags = cur.byte()?;
                let code = &cur.code[cur.pos..end];
                self.create(
                    frame,
                    &path,
                    Object::Method {
                        code,
                        args: flags & 7,
                    },
                )?;
                cur.pos = end;
            }
            0x15 => {
                cur.byte()?;
                let path = cur.name_string()?;
                let kind = cur.byte()?;
                let args = cur.byte()?;
                if self.ns.resolve_parent(frame.scope, &path).is_some()
                    && self.ns.resolve(frame.scope, &path).is_none()
                {
                    let args = if kind == 8 { args } else { 0 };
                    self.create(frame, &path, Object::External(args))?;
                }
            }
            0x06 => {
                cur.byte()?;
                let source = cur.name_string()?;
                let alias = cur.name_string()?;
                if let Some(target) = self.ns.resolve(frame.scope, &source) {
                    self.create(frame, &alias, Object::Alias(target))?;
                }
            }
            0xa0 => return self.exec_if(frame, cur),
            0xa1 => {
                cur.byte()?;
                cur.pos = cur.pkg_end()?;
            }
            0xa2 => return self.exec_while(frame, cur),
            0xa4 => {
                cur.byte()?;
                let value = if cur.at_end() {
                    Value::Integer(0)
                } else {
                    self.eval(frame, cur)?
                };
                let value = match value {
                    Value::Reference(target) => self.read_target(frame, &target)?,
                    other => other,
                };
                return Ok(Flow::Return(value));
            }
            0xa5 => {
                cur.byte()?;
                return Ok(Flow::Break);
            }
            0x9f => {
                cur.byte()?;
                return Ok(Flow::Continue);
            }
            0xa3 | 0xcc => {
                cur.byte()?;
            }
            0x86 => {
                cur.byte()?;
                let target = self.target(frame, cur)?;
                let value = self.eval_int(frame, cur)?;
                if let Target::Node(node) = target {
                    crate::log!("aml: Notify({}, {:#x})", self.ns.path_of(node), value);
                    let mut queue = super::NOTIFICATIONS.lock();
                    if queue.len() < 64 {
                        queue.push((node, value));
                    }
                }
            }
            0x8a | 0x8b | 0x8c | 0x8d | 0x8f => {
                cur.byte()?;
                let source = self.source_target(frame, cur)?;
                let index = self.eval_int(frame, cur)?;
                let path = cur.name_string()?;
                let (bit_offset, bit_length) = match op {
                    0x8a => (index * 8, 32),
                    0x8b => (index * 8, 16),
                    0x8c => (index * 8, 8),
                    0x8d => (index, 1),
                    _ => (index * 8, 64),
                };
                self.create(
                    frame,
                    &path,
                    Object::BufferField(BufferField {
                        source,
                        bit_offset,
                        bit_length,
                    }),
                )?;
            }
            0x5b => return self.exec_ext(frame, cur),
            _ => {
                self.eval(frame, cur)?;
            }
        }
        Ok(Flow::Next)
    }

    fn exec_ext(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Flow> {
        let op = cur.peek_at(1)?;
        match op {
            0x80 => {
                cur.pos += 2;
                let path = cur.name_string()?;
                let space = RegionSpace::from_byte(cur.byte()?);
                let base = self.eval_int(frame, cur)?;
                let length = self.eval_int(frame, cur)?;
                self.create(
                    frame,
                    &path,
                    Object::Region(OpRegion {
                        space,
                        base,
                        length,
                        pci: None,
                    }),
                )?;
            }
            0x81 => {
                cur.pos += 2;
                let end = cur.pkg_end()?;
                let path = cur.name_string()?;
                let region = self.resolve(frame, &path)?;
                let flags = cur.byte()?;
                self.field_list(frame, cur, end, FieldKind::Region(region), flags)?;
            }
            0x86 => {
                cur.pos += 2;
                let end = cur.pkg_end()?;
                let index = cur.name_string()?;
                let data = cur.name_string()?;
                let index = self.resolve(frame, &index)?;
                let data = self.resolve(frame, &data)?;
                let flags = cur.byte()?;
                self.field_list(frame, cur, end, FieldKind::Index { index, data }, flags)?;
            }
            0x87 => {
                cur.pos += 2;
                let end = cur.pkg_end()?;
                let region = cur.name_string()?;
                let bank = cur.name_string()?;
                let region = self.resolve(frame, &region)?;
                let bank = self.resolve(frame, &bank)?;
                let value = self.eval_int(frame, cur)?;
                let flags = cur.byte()?;
                self.field_list(
                    frame,
                    cur,
                    end,
                    FieldKind::Bank {
                        region,
                        bank,
                        value,
                    },
                    flags,
                )?;
            }
            0x82..=0x85 => {
                cur.pos += 2;
                let end = cur.pkg_end()?;
                let path = cur.name_string()?;
                let object = match op {
                    0x82 => Object::Device,
                    0x83 => {
                        cur.byte()?;
                        cur.uint(4)?;
                        cur.byte()?;
                        Object::Processor
                    }
                    0x84 => {
                        cur.byte()?;
                        cur.uint(2)?;
                        Object::PowerResource
                    }
                    _ => Object::ThermalZone,
                };
                let node = self.create(frame, &path, object)?;
                return self.exec_body(frame, cur, node, end);
            }
            0x01 => {
                cur.pos += 2;
                let path = cur.name_string()?;
                cur.byte()?;
                self.create(frame, &path, Object::Mutex)?;
            }
            0x02 => {
                cur.pos += 2;
                let path = cur.name_string()?;
                self.create(frame, &path, Object::Event)?;
            }
            0x13 => {
                cur.pos += 2;
                let source = self.source_target(frame, cur)?;
                let bit_offset = self.eval_int(frame, cur)?;
                let bit_length = self.eval_int(frame, cur)?;
                let path = cur.name_string()?;
                self.create(
                    frame,
                    &path,
                    Object::BufferField(BufferField {
                        source,
                        bit_offset,
                        bit_length,
                    }),
                )?;
            }
            0x88 => {
                cur.pos += 2;
                let path = cur.name_string()?;
                for _ in 0..3 {
                    self.eval(frame, cur)?;
                }
                self.create(frame, &path, Object::Name(Value::Uninitialized))?;
            }
            0x27 | 0x26 | 0x24 => {
                cur.pos += 2;
                self.target(frame, cur)?;
            }
            0x22 => {
                cur.pos += 2;
                let ms = self.eval_int(frame, cur)?.min(2000);
                if crate::sched::is_running() {
                    crate::sched::sleep_ms(ms);
                } else {
                    tsc::busy_wait_us(ms * 1000);
                }
            }
            0x21 => {
                cur.pos += 2;
                let us = self.eval_int(frame, cur)?.min(100);
                tsc::busy_wait_us(us);
            }
            0x32 => {
                cur.pos += 2;
                let kind = cur.byte()?;
                let code = cur.uint(4)?;
                let arg = self.eval_int(frame, cur)?;
                crate::log!(
                    "aml: Fatal(type {:#x}, code {:#x}, arg {:#x})",
                    kind,
                    code,
                    arg
                );
                return Err(AmlError::Fatal);
            }
            _ => {
                self.eval(frame, cur)?;
            }
        }
        Ok(Flow::Next)
    }

    fn field_list(
        &mut self,
        frame: &mut Frame,
        cur: &mut Cursor,
        end: usize,
        kind: FieldKind,
        flags: u8,
    ) -> Result<()> {
        let mut bit_offset = 0u64;
        let mut access = flags & 0x0f;
        let update_rule = (flags >> 5) & 0x3;
        while cur.pos < end {
            match cur.peek()? {
                0x00 => {
                    cur.byte()?;
                    bit_offset += cur.pkg_length_raw()? as u64;
                }
                0x01 => {
                    cur.byte()?;
                    access = cur.byte()? & 0x0f;
                    cur.byte()?;
                }
                0x02 => {
                    cur.byte()?;
                    if cur.peek()? == 0x11 {
                        self.eval(frame, cur)?;
                    } else {
                        cur.name_string()?;
                    }
                }
                0x03 => {
                    cur.byte()?;
                    access = cur.byte()? & 0x0f;
                    cur.byte()?;
                    cur.byte()?;
                }
                _ => {
                    let name = cur.name_seg()?;
                    let bit_length = cur.pkg_length_raw()? as u64;
                    let access_bytes = match access {
                        2 => 2,
                        3 => 4,
                        4 => 8,
                        _ => 1,
                    };
                    let path = NamePath {
                        absolute: false,
                        parents: 0,
                        segments: vec![name],
                    };
                    self.create(
                        frame,
                        &path,
                        Object::Field(FieldUnit {
                            kind: kind.clone(),
                            bit_offset,
                            bit_length,
                            access_bytes,
                            update_rule,
                        }),
                    )?;
                    bit_offset += bit_length;
                }
            }
        }
        cur.pos = end;
        Ok(())
    }

    fn exec_if(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Flow> {
        cur.byte()?;
        let end = cur.pkg_end()?;
        let taken = self.eval_int(frame, cur)? != 0;
        let mut flow = Flow::Next;
        if taken {
            flow = self.exec_scoped(frame, cur, end)?;
        }
        cur.pos = end;
        if !cur.at_end() && cur.peek()? == 0xa1 {
            cur.byte()?;
            let else_end = cur.pkg_end()?;
            if !taken {
                flow = self.exec_scoped(frame, cur, else_end)?;
            }
            cur.pos = else_end;
        }
        Ok(flow)
    }

    fn exec_scoped(&mut self, frame: &mut Frame, cur: &mut Cursor, end: usize) -> Result<Flow> {
        if frame.method {
            let saved = cur.end;
            cur.end = end;
            let flow = self.exec_list(frame, cur, end);
            cur.end = saved;
            flow
        } else {
            let scope = frame.scope;
            self.exec_body(frame, cur, scope, end)
        }
    }

    fn exec_while(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Flow> {
        cur.byte()?;
        let end = cur.pkg_end()?;
        let predicate = cur.pos;
        let mut iterations = 0u64;
        loop {
            cur.pos = predicate;
            if self.eval_int(frame, cur)? == 0 {
                break;
            }
            match self.exec_scoped(frame, cur, end)? {
                Flow::Break => break,
                Flow::Return(value) => {
                    cur.pos = end;
                    return Ok(Flow::Return(value));
                }
                _ => {}
            }
            iterations += 1;
            if iterations % 1024 == 0 {
                self.check_deadline()?;
            }
            if iterations > MAX_LOOP_ITERATIONS {
                return Err(AmlError::Timeout);
            }
        }
        cur.pos = end;
        Ok(Flow::Next)
    }

    fn resolve(&self, frame: &Frame, path: &NamePath) -> Result<NodeId> {
        self.ns
            .resolve(frame.scope, path)
            .ok_or_else(|| AmlError::NameNotFound(path_text(path)))
    }

    pub fn eval_int(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<u64> {
        let value = self.eval(frame, cur)?;
        self.integer_of(frame, value)
    }

    fn integer_of(&mut self, frame: &mut Frame, value: Value) -> Result<u64> {
        let value = self.resolve_value(frame, value)?;
        value
            .as_integer()
            .map(|v| v & self.int_mask)
            .ok_or(AmlError::Type("expected an integer"))
    }

    fn resolve_value(&mut self, frame: &mut Frame, value: Value) -> Result<Value> {
        match value {
            Value::Reference(target) => self.read_target(frame, &target),
            Value::Node(node) => self.read_target(frame, &Target::Node(node)),
            other => Ok(other),
        }
    }

    fn ones(&self) -> u64 {
        self.int_mask
    }

    fn boolean(&self, value: bool) -> Value {
        Value::Integer(if value { self.ones() } else { 0 })
    }

    pub fn eval(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Value> {
        let start = cur.pos;
        let op = cur.peek()?;
        let value = match op {
            0x00 => {
                cur.byte()?;
                Value::Integer(0)
            }
            0x01 => {
                cur.byte()?;
                Value::Integer(1)
            }
            0xff => {
                cur.byte()?;
                Value::Integer(self.ones())
            }
            0x0a => {
                cur.byte()?;
                Value::Integer(cur.uint(1)?)
            }
            0x0b => {
                cur.byte()?;
                Value::Integer(cur.uint(2)?)
            }
            0x0c => {
                cur.byte()?;
                Value::Integer(cur.uint(4)?)
            }
            0x0e => {
                cur.byte()?;
                Value::Integer(cur.uint(8)?)
            }
            0x0d => {
                cur.byte()?;
                let mut s = String::new();
                loop {
                    let b = cur.byte()?;
                    if b == 0 {
                        break;
                    }
                    s.push(b as char);
                }
                Value::String(s)
            }
            0x11 => {
                cur.byte()?;
                let end = cur.pkg_end()?;
                let size = self.eval_int(frame, cur)? as usize;
                let init = &cur.code[cur.pos..end];
                let mut bytes = vec![0u8; size.max(init.len())];
                bytes[..init.len()].copy_from_slice(init);
                cur.pos = end;
                Value::Buffer(bytes)
            }
            0x12 | 0x13 => {
                cur.byte()?;
                let end = cur.pkg_end()?;
                let count = if op == 0x12 {
                    cur.byte()? as usize
                } else {
                    self.eval_int(frame, cur)? as usize
                };
                let mut items = Vec::with_capacity(count);
                while cur.pos < end {
                    let item = if is_name_start(cur.peek()?) {
                        let path = cur.name_string()?;
                        match self.ns.resolve(frame.scope, &path) {
                            Some(node) => match self.ns.object(node) {
                                Object::Name(value) => value.clone(),
                                _ => Value::Node(node),
                            },
                            None => Value::String(path_text(&path)),
                        }
                    } else {
                        self.eval(frame, cur)?
                    };
                    items.push(item);
                }
                cur.pos = end;
                while items.len() < count {
                    items.push(Value::Uninitialized);
                }
                Value::Package(items)
            }
            0x60..=0x67 => {
                cur.byte()?;
                frame.locals[(op - 0x60) as usize].clone()
            }
            0x68..=0x6e => {
                cur.byte()?;
                frame.args[(op - 0x68) as usize].clone()
            }
            0x70 | 0x9d => {
                cur.byte()?;
                let value = self.eval(frame, cur)?;
                let value = match value {
                    Value::Node(node) => self.read_target(frame, &Target::Node(node))?,
                    other => other,
                };
                let target = self.target(frame, cur)?;
                self.store(frame, &target, value.clone())?;
                value
            }
            0x71 => {
                cur.byte()?;
                let target = self.target(frame, cur)?;
                Value::Reference(Box::new(target))
            }
            0x72 | 0x74 | 0x77 | 0x79 | 0x7a | 0x7b | 0x7c | 0x7d | 0x7e | 0x7f | 0x85 => {
                cur.byte()?;
                let a = self.eval_int(frame, cur)?;
                let b = self.eval_int(frame, cur)?;
                let result = match op {
                    0x72 => a.wrapping_add(b),
                    0x74 => a.wrapping_sub(b),
                    0x77 => a.wrapping_mul(b),
                    0x79 => {
                        if b >= 64 {
                            0
                        } else {
                            a << b
                        }
                    }
                    0x7a => {
                        if b >= 64 {
                            0
                        } else {
                            a >> b
                        }
                    }
                    0x7b => a & b,
                    0x7c => !(a & b),
                    0x7d => a | b,
                    0x7e => !(a | b),
                    0x7f => a ^ b,
                    _ => {
                        if b == 0 {
                            return Err(AmlError::Type("modulo by zero"));
                        }
                        a % b
                    }
                } & self.int_mask;
                let target = self.target(frame, cur)?;
                self.store(frame, &target, Value::Integer(result))?;
                Value::Integer(result)
            }
            0x75 | 0x76 => {
                cur.byte()?;
                let target = self.target(frame, cur)?;
                let current = self.read_target(frame, &target)?;
                let current = self.integer_of(frame, current)?;
                let result = if op == 0x75 {
                    current.wrapping_add(1)
                } else {
                    current.wrapping_sub(1)
                } & self.int_mask;
                self.store(frame, &target, Value::Integer(result))?;
                Value::Integer(result)
            }
            0x78 => {
                cur.byte()?;
                let a = self.eval_int(frame, cur)?;
                let b = self.eval_int(frame, cur)?;
                if b == 0 {
                    return Err(AmlError::Type("divide by zero"));
                }
                let remainder = self.target(frame, cur)?;
                let quotient = self.target(frame, cur)?;
                self.store(frame, &remainder, Value::Integer(a % b))?;
                self.store(frame, &quotient, Value::Integer(a / b))?;
                Value::Integer(a / b)
            }
            0x80..=0x82 => {
                cur.byte()?;
                let a = self.eval_int(frame, cur)?;
                let result = match op {
                    0x80 => !a & self.int_mask,
                    0x81 => {
                        if a == 0 {
                            0
                        } else {
                            64 - a.leading_zeros() as u64
                        }
                    }
                    _ => {
                        if a == 0 {
                            0
                        } else {
                            a.trailing_zeros() as u64 + 1
                        }
                    }
                };
                let target = self.target(frame, cur)?;
                self.store(frame, &target, Value::Integer(result))?;
                Value::Integer(result)
            }
            0x83 => {
                cur.byte()?;
                let value = self.eval(frame, cur)?;
                match value {
                    Value::String(path) => {
                        let path = NamePath::parse(&path).ok_or(AmlError::Type("bad path"))?;
                        let node = self.resolve(frame, &path)?;
                        self.read_target(frame, &Target::Node(node))?
                    }
                    other => self.resolve_value(frame, other)?,
                }
            }
            0x73 | 0x84 => {
                cur.byte()?;
                let a = self.eval(frame, cur)?;
                let a = self.resolve_value(frame, a)?;
                let b = self.eval(frame, cur)?;
                let b = self.resolve_value(frame, b)?;
                let result = if op == 0x84 {
                    let mut bytes = a.as_bytes().unwrap_or_default();
                    if bytes.len() >= 2 && bytes[bytes.len() - 2] == 0x79 {
                        bytes.truncate(bytes.len() - 2);
                    }
                    bytes.extend(b.as_bytes().unwrap_or_default());
                    Value::Buffer(bytes)
                } else {
                    self.concatenate(a, b)
                };
                let target = self.target(frame, cur)?;
                self.store(frame, &target, result.clone())?;
                result
            }
            0x87 => {
                cur.byte()?;
                let target = self.target(frame, cur)?;
                let value = self.read_target(frame, &target)?;
                let value = self.resolve_value(frame, value)?;
                Value::Integer(match value {
                    Value::Buffer(b) => b.len() as u64,
                    Value::String(s) => s.len() as u64,
                    Value::Package(p) => p.len() as u64,
                    _ => return Err(AmlError::Type("SizeOf on a non-sized object")),
                })
            }
            0x88 => {
                cur.byte()?;
                let source = self.source_target(frame, cur)?;
                let index = self.eval_int(frame, cur)? as usize;
                let reference =
                    Value::Reference(Box::new(Target::Element(Box::new(source), index)));
                let target = self.target(frame, cur)?;
                if !matches!(target, Target::None) {
                    self.store_raw(frame, &target, reference.clone())?;
                }
                reference
            }
            0x89 => {
                cur.byte()?;
                let package = self.eval(frame, cur)?;
                let package = self.resolve_value(frame, package)?;
                let op1 = cur.byte()?;
                let obj1 = self.eval_int(frame, cur)?;
                let op2 = cur.byte()?;
                let obj2 = self.eval_int(frame, cur)?;
                let start = self.eval_int(frame, cur)? as usize;
                let matches = |op: u8, element: u64, operand: u64| match op {
                    0 => true,
                    1 => element == operand,
                    2 => element <= operand,
                    3 => element < operand,
                    4 => element >= operand,
                    5 => element > operand,
                    _ => false,
                };
                let mut found = self.ones();
                if let Value::Package(items) = package {
                    for (i, item) in items.iter().enumerate().skip(start) {
                        if let Some(v) = item.as_integer()
                            && matches(op1, v, obj1)
                            && matches(op2, v, obj2)
                        {
                            found = i as u64;
                            break;
                        }
                    }
                }
                Value::Integer(found)
            }
            0x8e => {
                cur.byte()?;
                let target = self.target(frame, cur)?;
                Value::Integer(match target {
                    Target::Node(node) => self.ns.object(node).type_code(),
                    other => self.read_target(frame, &other)?.type_code(),
                })
            }
            0x90 | 0x91 => {
                cur.byte()?;
                let a = self.eval_int(frame, cur)? != 0;
                let b = self.eval_int(frame, cur)? != 0;
                self.boolean(if op == 0x90 { a && b } else { a || b })
            }
            0x92 => {
                cur.byte()?;
                match cur.peek()? {
                    0x93..=0x95 => {
                        let inner = cur.byte()?;
                        let ordering = self.compare(frame, cur)?;
                        self.boolean(match inner {
                            0x93 => ordering != core::cmp::Ordering::Equal,
                            0x94 => ordering != core::cmp::Ordering::Greater,
                            _ => ordering != core::cmp::Ordering::Less,
                        })
                    }
                    _ => {
                        let a = self.eval_int(frame, cur)?;
                        self.boolean(a == 0)
                    }
                }
            }
            0x93..=0x95 => {
                cur.byte()?;
                let ordering = self.compare(frame, cur)?;
                self.boolean(match op {
                    0x93 => ordering == core::cmp::Ordering::Equal,
                    0x94 => ordering == core::cmp::Ordering::Greater,
                    _ => ordering == core::cmp::Ordering::Less,
                })
            }
            0x96..=0x99 => {
                cur.byte()?;
                let value = self.eval(frame, cur)?;
                let value = self.resolve_value(frame, value)?;
                let result = match op {
                    0x96 => Value::Buffer(value.as_bytes().unwrap_or_default()),
                    0x97 => match &value {
                        Value::Buffer(b) => Value::String(
                            b.iter()
                                .map(|x| format!("{}", x))
                                .collect::<Vec<_>>()
                                .join(","),
                        ),
                        other => Value::String(format!("{}", other.as_integer().unwrap_or(0))),
                    },
                    0x98 => match &value {
                        Value::Buffer(b) => Value::String(
                            b.iter()
                                .map(|x| format!("0x{:02X}", x))
                                .collect::<Vec<_>>()
                                .join(","),
                        ),
                        Value::String(s) => Value::String(s.clone()),
                        other => Value::String(format!("{:X}", other.as_integer().unwrap_or(0))),
                    },
                    _ => match &value {
                        Value::String(s) => {
                            let s = s.trim();
                            let parsed = if let Some(hex) =
                                s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
                            {
                                u64::from_str_radix(hex, 16).unwrap_or(0)
                            } else {
                                s.parse::<u64>().unwrap_or(0)
                            };
                            Value::Integer(parsed & self.int_mask)
                        }
                        other => Value::Integer(other.as_integer().unwrap_or(0) & self.int_mask),
                    },
                };
                let target = self.target(frame, cur)?;
                self.store(frame, &target, result.clone())?;
                result
            }
            0x9c => {
                cur.byte()?;
                let value = self.eval(frame, cur)?;
                let value = self.resolve_value(frame, value)?;
                let limit = self.eval_int(frame, cur)? as usize;
                let bytes = value.as_bytes().unwrap_or_default();
                let s: String = bytes
                    .iter()
                    .take(limit.min(bytes.len()))
                    .take_while(|&&b| b != 0)
                    .map(|&b| b as char)
                    .collect();
                let target = self.target(frame, cur)?;
                self.store(frame, &target, Value::String(s.clone()))?;
                Value::String(s)
            }
            0x9e => {
                cur.byte()?;
                let value = self.eval(frame, cur)?;
                let value = self.resolve_value(frame, value)?;
                let index = self.eval_int(frame, cur)? as usize;
                let length = self.eval_int(frame, cur)? as usize;
                let result = match value {
                    Value::String(s) => Value::String(s.chars().skip(index).take(length).collect()),
                    other => {
                        let bytes = other.as_bytes().unwrap_or_default();
                        let start = index.min(bytes.len());
                        let end = (start + length).min(bytes.len());
                        Value::Buffer(bytes[start..end].to_vec())
                    }
                };
                let target = self.target(frame, cur)?;
                self.store(frame, &target, result.clone())?;
                result
            }
            0x5b => self.eval_ext(frame, cur)?,
            b if is_name_start(b) => {
                let path = cur.name_string()?;
                self.name_value(frame, cur, &path)?
            }
            _ => return Err(AmlError::UnknownOpcode(op as u16, start)),
        };
        Ok(value)
    }

    fn eval_ext(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Value> {
        let start = cur.pos;
        let op = cur.peek_at(1)?;
        cur.pos += 2;
        Ok(match op {
            0x12 => {
                let exists = if is_name_start(cur.peek()?) {
                    let path = cur.name_string()?;
                    self.ns.resolve(frame.scope, &path).map(Target::Node)
                } else {
                    Some(self.target(frame, cur)?)
                };
                let target = self.target(frame, cur)?;
                match exists {
                    Some(found) => {
                        if !matches!(target, Target::None) {
                            self.store_raw(frame, &target, Value::Reference(Box::new(found)))?;
                        }
                        Value::Integer(self.ones())
                    }
                    None => Value::Integer(0),
                }
            }
            0x23 => {
                self.target(frame, cur)?;
                cur.uint(2)?;
                Value::Integer(0)
            }
            0x25 => {
                self.target(frame, cur)?;
                self.eval_int(frame, cur)?;
                Value::Integer(0)
            }
            0x28 | 0x29 => {
                let value = self.eval_int(frame, cur)?;
                let result = if op == 0x28 {
                    let mut out = 0u64;
                    let mut scale = 1u64;
                    let mut v = value;
                    while v != 0 {
                        out += (v & 0xf) * scale;
                        scale *= 10;
                        v >>= 4;
                    }
                    out
                } else {
                    let mut out = 0u64;
                    let mut shift = 0;
                    let mut v = value;
                    while v != 0 && shift < 64 {
                        out |= (v % 10) << shift;
                        shift += 4;
                        v /= 10;
                    }
                    out
                };
                let target = self.target(frame, cur)?;
                self.store(frame, &target, Value::Integer(result))?;
                Value::Integer(result)
            }
            0x30 => Value::Integer(2),
            0x31 => Value::Uninitialized,
            0x33 => Value::Integer(tsc::now_ns() / 100),
            _ => return Err(AmlError::UnknownOpcode(0x5b00 | op as u16, start)),
        })
    }

    fn name_value(
        &mut self,
        frame: &mut Frame,
        cur: &mut Cursor,
        path: &NamePath,
    ) -> Result<Value> {
        let node = match self.ns.resolve(frame.scope, path) {
            Some(node) => node,
            None if !frame.method => {
                crate::log!("aml: unresolved name {} during load", path_text(path));
                return Ok(Value::Integer(0));
            }
            None => return Err(AmlError::NameNotFound(path_text(path))),
        };
        if let Some(count) = self.ns.object(node).method_args() {
            let mut args = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let arg = self.eval(frame, cur)?;
                args.push(arg);
            }
            if matches!(self.ns.object(node), Object::External(_)) {
                return Err(AmlError::NameNotFound(path_text(path)));
            }
            return self.invoke(node, args);
        }
        if matches!(self.ns.object(node), Object::Method { .. }) {
            return self.invoke(node, Vec::new());
        }
        self.read_target(frame, &Target::Node(node))
    }

    fn compare(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<core::cmp::Ordering> {
        let a = self.eval(frame, cur)?;
        let a = self.resolve_value(frame, a)?;
        let b = self.eval(frame, cur)?;
        let b = self.resolve_value(frame, b)?;
        Ok(match (&a, &b) {
            (Value::String(x), _) => {
                let y = match &b {
                    Value::String(s) => s.clone(),
                    other => {
                        String::from_utf8_lossy(&other.as_bytes().unwrap_or_default()).into_owned()
                    }
                };
                x.as_str().cmp(y.as_str())
            }
            (Value::Buffer(x), _) => x
                .as_slice()
                .cmp(b.as_bytes().unwrap_or_default().as_slice()),
            _ => {
                let x = a
                    .as_integer()
                    .ok_or(AmlError::Type("bad comparison operand"))?
                    & self.int_mask;
                let y = b
                    .as_integer()
                    .ok_or(AmlError::Type("bad comparison operand"))?
                    & self.int_mask;
                x.cmp(&y)
            }
        })
    }

    fn concatenate(&self, a: Value, b: Value) -> Value {
        match a {
            Value::String(mut s) => {
                match b {
                    Value::String(t) => s.push_str(&t),
                    Value::Integer(v) => s.push_str(&format!("{:X}", v)),
                    other => {
                        for byte in other.as_bytes().unwrap_or_default() {
                            s.push_str(&format!("{:02X}", byte));
                        }
                    }
                }
                Value::String(s)
            }
            Value::Integer(v) => {
                let width = if self.int_mask == u32::MAX as u64 {
                    4
                } else {
                    8
                };
                let mut bytes = v.to_le_bytes()[..width].to_vec();
                let other = b.as_integer().unwrap_or(0);
                bytes.extend_from_slice(&other.to_le_bytes()[..width]);
                Value::Buffer(bytes)
            }
            other => {
                let mut bytes = other.as_bytes().unwrap_or_default();
                bytes.extend(b.as_bytes().unwrap_or_default());
                Value::Buffer(bytes)
            }
        }
    }

    /// Index() ve Create*Field için kaynak: adlandırılmış nesneye yazılabilmesi için hedef olarak tutulur.
    fn source_target(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Target> {
        let op = cur.peek()?;
        match op {
            0x60..=0x67 => {
                cur.byte()?;
                Ok(Target::Local(op - 0x60))
            }
            0x68..=0x6e => {
                cur.byte()?;
                Ok(Target::Arg(op - 0x68))
            }
            b if is_name_start(b) => {
                let save = cur.pos;
                let path = cur.name_string()?;
                let node = self.resolve(frame, &path)?;
                if self.ns.object(node).method_args().is_some()
                    || matches!(self.ns.object(node), Object::Method { .. })
                {
                    cur.pos = save;
                    let value = self.eval(frame, cur)?;
                    return Ok(Target::Temporary(Box::new(value)));
                }
                Ok(Target::Node(node))
            }
            _ => {
                let value = self.eval(frame, cur)?;
                Ok(match value {
                    Value::Reference(target) => *target,
                    other => Target::Temporary(Box::new(other)),
                })
            }
        }
    }

    fn target(&mut self, frame: &mut Frame, cur: &mut Cursor) -> Result<Target> {
        let op = cur.peek()?;
        match op {
            0x00 => {
                cur.byte()?;
                Ok(Target::None)
            }
            0x60..=0x67 => {
                cur.byte()?;
                Ok(Target::Local(op - 0x60))
            }
            0x68..=0x6e => {
                cur.byte()?;
                Ok(Target::Arg(op - 0x68))
            }
            0x5b if cur.peek_at(1)? == 0x31 => {
                cur.pos += 2;
                Ok(Target::Debug)
            }
            0x83 => {
                cur.byte()?;
                let value = self.eval(frame, cur)?;
                match value {
                    Value::Reference(target) => Ok(*target),
                    Value::String(path) => {
                        let path = NamePath::parse(&path).ok_or(AmlError::Type("bad path"))?;
                        Ok(Target::Node(self.resolve(frame, &path)?))
                    }
                    Value::Node(node) => Ok(Target::Node(node)),
                    _ => Err(AmlError::Type("DerefOf target is not a reference")),
                }
            }
            b if is_name_start(b) => {
                let save = cur.pos;
                let path = cur.name_string()?;
                let node = self.resolve(frame, &path)?;
                if self.ns.object(node).method_args().is_some()
                    || matches!(self.ns.object(node), Object::Method { .. })
                {
                    cur.pos = save;
                    let value = self.eval(frame, cur)?;
                    return Ok(match value {
                        Value::Reference(target) => *target,
                        other => Target::Temporary(Box::new(other)),
                    });
                }
                Ok(Target::Node(node))
            }
            _ => {
                let value = self.eval(frame, cur)?;
                Ok(match value {
                    Value::Reference(target) => *target,
                    other => Target::Temporary(Box::new(other)),
                })
            }
        }
    }

    pub fn read_target(&mut self, frame: &mut Frame, target: &Target) -> Result<Value> {
        Ok(match target {
            Target::None | Target::Debug => Value::Uninitialized,
            Target::Local(n) => frame.locals[*n as usize].clone(),
            Target::Arg(n) => frame.args[*n as usize].clone(),
            Target::Temporary(value) => (**value).clone(),
            Target::Node(node) => match self.ns.object(*node).clone() {
                Object::Name(value) => value,
                Object::Field(field) => region::read_field(self, &field)?,
                Object::BufferField(field) => {
                    let source = self.read_target(frame, &field.source)?;
                    let source = self.resolve_value(frame, source)?;
                    let bytes = source.as_bytes().unwrap_or_default();
                    region::extract_bits(&bytes, field.bit_offset, field.bit_length)
                }
                Object::Method { .. } => self.invoke(*node, Vec::new())?,
                _ => Value::Node(*node),
            },
            Target::Element(container, index) => {
                let value = self.read_target(frame, container)?;
                let value = match value {
                    Value::Reference(inner) => self.read_target(frame, &inner)?,
                    other => other,
                };
                match value {
                    Value::Package(items) => items
                        .get(*index)
                        .cloned()
                        .ok_or(AmlError::Type("package index out of range"))?,
                    Value::Buffer(bytes) => Value::Integer(
                        *bytes
                            .get(*index)
                            .ok_or(AmlError::Type("buffer index out of range"))?
                            as u64,
                    ),
                    Value::String(s) => Value::Integer(
                        *s.as_bytes()
                            .get(*index)
                            .ok_or(AmlError::Type("string index out of range"))?
                            as u64,
                    ),
                    _ => return Err(AmlError::Type("Index on a non-indexable object")),
                }
            }
        })
    }

    /// Değeri dönüştürmeden yazar (Index/CondRefOf referansları için).
    fn store_raw(&mut self, frame: &mut Frame, target: &Target, value: Value) -> Result<()> {
        match target {
            Target::Local(n) => frame.locals[*n as usize] = value,
            Target::Arg(n) => frame.args[*n as usize] = value,
            Target::Node(node) => {
                if let Object::Name(_) = self.ns.object(*node) {
                    self.ns.node_mut(*node).object = Object::Name(value);
                } else {
                    self.store(frame, target, value)?;
                }
            }
            _ => self.store(frame, target, value)?,
        }
        Ok(())
    }

    pub fn store(&mut self, frame: &mut Frame, target: &Target, value: Value) -> Result<()> {
        match target {
            Target::None | Target::Temporary(_) => {}
            Target::Debug => crate::log!("aml: Debug = {:?}", value),
            Target::Local(n) => frame.locals[*n as usize] = value,
            Target::Arg(n) => {
                if let Value::Reference(inner) = frame.args[*n as usize].clone() {
                    self.store(frame, &inner, value)?;
                } else {
                    frame.args[*n as usize] = value;
                }
            }
            Target::Node(node) => match self.ns.object(*node).clone() {
                Object::Name(old) => {
                    let converted = match (&old, value) {
                        (Value::Integer(_), v) => {
                            Value::Integer(v.as_integer().unwrap_or(0) & self.int_mask)
                        }
                        (
                            Value::Buffer(old_bytes),
                            v @ (Value::Integer(_) | Value::String(_) | Value::Buffer(_)),
                        ) => {
                            let mut bytes = v.as_bytes().unwrap_or_default();
                            bytes.resize(old_bytes.len(), 0);
                            Value::Buffer(bytes)
                        }
                        (Value::String(_), Value::Integer(v)) => Value::String(format!("{:X}", v)),
                        (_, v) => v,
                    };
                    self.ns.node_mut(*node).object = Object::Name(converted);
                }
                Object::Field(field) => region::write_field(self, &field, &value)?,
                Object::BufferField(field) => {
                    let source = self.read_target(frame, &field.source)?;
                    let source = self.resolve_value(frame, source)?;
                    let mut bytes = source.as_bytes().unwrap_or_default();
                    let data = match &value {
                        Value::Integer(v) => v.to_le_bytes().to_vec(),
                        other => other.as_bytes().unwrap_or_default(),
                    };
                    region::insert_bits(&mut bytes, field.bit_offset, field.bit_length, &data);
                    self.store_raw(frame, &field.source, Value::Buffer(bytes))?;
                }
                _ => return Err(AmlError::Type("store to a non-data object")),
            },
            Target::Element(container, index) => {
                let current = self.read_target(frame, container)?;
                let (container, current) = match current {
                    Value::Reference(inner) => {
                        let value = self.read_target(frame, &inner)?;
                        (*inner, value)
                    }
                    other => ((**container).clone(), other),
                };
                let updated = match current {
                    Value::Package(mut items) => {
                        let slot = items
                            .get_mut(*index)
                            .ok_or(AmlError::Type("package index out of range"))?;
                        *slot = value;
                        Value::Package(items)
                    }
                    Value::Buffer(mut bytes) => {
                        let slot = bytes
                            .get_mut(*index)
                            .ok_or(AmlError::Type("buffer index out of range"))?;
                        *slot = value.as_integer().unwrap_or(0) as u8;
                        Value::Buffer(bytes)
                    }
                    _ => return Err(AmlError::Type("Index store on a non-indexable object")),
                };
                self.store_raw(frame, &container, updated)?;
            }
        }
        Ok(())
    }
}
