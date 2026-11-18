//! Banna Rust (brs): Banna OS üzerinde çalışan Rust alt kümesi derleyicisi ve `.banna`
//! sanal makinesi. `brct build` kaynağı derler, `banna-run` (ya da doğrudan
//! `./uygulama.banna`) çalıştırır.

pub mod code;
pub mod compiler;
pub mod lexer;
pub mod parser;
pub mod vm;

/// Kaynağı derleyip `.banna` dosya içeriğini üretir.
pub fn build(source: &str) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
    Ok(compiler::compile(source)?.encode())
}
