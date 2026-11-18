//! AML değerleri ve ad alanı nesneleri.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::namespace::NodeId;

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Uninitialized,
    Integer(u64),
    String(String),
    Buffer(Vec<u8>),
    Package(Vec<Value>),
    Reference(Box<Target>),
    Node(NodeId),
}

impl Value {
    pub fn as_integer(&self) -> Option<u64> {
        match self {
            Value::Integer(v) => Some(*v),
            Value::Buffer(bytes) => {
                let mut out = [0u8; 8];
                let n = bytes.len().min(8);
                out[..n].copy_from_slice(&bytes[..n]);
                Some(u64::from_le_bytes(out))
            }
            Value::String(s) => parse_integer(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Value::Buffer(b) => Some(b.clone()),
            Value::String(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                Some(bytes)
            }
            Value::Integer(v) => Some(v.to_le_bytes().to_vec()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_package(&self) -> Option<&[Value]> {
        match self {
            Value::Package(items) => Some(items),
            _ => None,
        }
    }

    pub fn type_code(&self) -> u64 {
        match self {
            Value::Uninitialized => 0,
            Value::Integer(_) => 1,
            Value::String(_) => 2,
            Value::Buffer(_) => 3,
            Value::Package(_) => 4,
            Value::Reference(_) | Value::Node(_) => 0,
        }
    }
}

fn parse_integer(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    let digits: String = text.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    u64::from_str_radix(&digits, 16).ok().or(Some(0))
}

/// Bir değerin yazılabileceği yer.
#[derive(Clone, Debug)]
pub enum Target {
    None,
    Debug,
    Local(u8),
    Arg(u8),
    Node(NodeId),
    Element(Box<Target>, usize),
    Temporary(Box<Value>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionSpace {
    SystemMemory,
    SystemIo,
    PciConfig,
    EmbeddedControl,
    SmBus,
    Cmos,
    Other(u8),
}

impl RegionSpace {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::SystemMemory,
            1 => Self::SystemIo,
            2 => Self::PciConfig,
            3 => Self::EmbeddedControl,
            4 => Self::SmBus,
            5 => Self::Cmos,
            other => Self::Other(other),
        }
    }

    pub fn id(&self) -> u8 {
        match self {
            Self::SystemMemory => 0,
            Self::SystemIo => 1,
            Self::PciConfig => 2,
            Self::EmbeddedControl => 3,
            Self::SmBus => 4,
            Self::Cmos => 5,
            Self::Other(b) => *b,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpRegion {
    pub space: RegionSpace,
    pub base: u64,
    pub length: u64,
    pub pci: Option<(u8, u8, u8)>,
}

#[derive(Clone, Debug)]
pub enum FieldKind {
    Region(NodeId),
    Index {
        index: NodeId,
        data: NodeId,
    },
    Bank {
        region: NodeId,
        bank: NodeId,
        value: u64,
    },
}

#[derive(Clone, Debug)]
pub struct FieldUnit {
    pub kind: FieldKind,
    pub bit_offset: u64,
    pub bit_length: u64,
    pub access_bytes: u8,
    pub update_rule: u8,
}

#[derive(Clone, Debug)]
pub struct BufferField {
    pub source: Target,
    pub bit_offset: u64,
    pub bit_length: u64,
}

pub type NativeMethod = fn(&[Value]) -> Value;

#[derive(Clone)]
pub enum Object {
    Scope,
    Device,
    Processor,
    ThermalZone,
    PowerResource,
    Mutex,
    Event,
    External(u8),
    Name(Value),
    Method { code: &'static [u8], args: u8 },
    NativeMethod { args: u8, func: NativeMethod },
    Region(OpRegion),
    Field(FieldUnit),
    BufferField(BufferField),
    Alias(NodeId),
}

impl Object {
    pub fn type_code(&self) -> u64 {
        match self {
            Object::Name(v) => v.type_code(),
            Object::Field(_) => 5,
            Object::Device => 6,
            Object::Event => 7,
            Object::Method { .. } | Object::NativeMethod { .. } => 8,
            Object::Mutex => 9,
            Object::Region(_) => 10,
            Object::PowerResource => 11,
            Object::Processor => 12,
            Object::ThermalZone => 13,
            Object::BufferField(_) => 14,
            _ => 0,
        }
    }

    pub fn method_args(&self) -> Option<u8> {
        match self {
            Object::Method { args, .. } | Object::NativeMethod { args, .. } => Some(*args),
            Object::External(args) if *args > 0 => Some(*args),
            _ => None,
        }
    }
}
