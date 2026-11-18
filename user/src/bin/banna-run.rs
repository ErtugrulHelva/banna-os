//! `banna-run <program.banna> [argüman...]`: Banna Rust (`brct build`) ile derlenmiş
//! `.banna` programlarını çalıştırır. Çekirdek `#!/bin/banna-run` satırını görünce
//! `./program.banna` doğrudan da çalışır.

#![no_std]
#![no_main]

use banna::brs::code::Program;
use banna::brs::vm::{Host, Vm};
use banna::{String, Vec, eprintln, fs, io, time};

banna::entry!(main);

struct BannaHost {
    args: Vec<String>,
    pending: Vec<u8>,
    seed: u64,
}

impl Host for BannaHost {
    fn write(&mut self, text: &str, stderr: bool) {
        if stderr {
            io::write_err(text);
        } else {
            io::write_out(text);
        }
    }

    fn read_line(&mut self) -> Option<String> {
        loop {
            if let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=i).collect();
                return Some(String::from_utf8_lossy(&line[..line.len() - 1]).trim_end_matches('\r').into());
            }
            let mut buf = [0u8; 256];
            let n = io::read_stdin(&mut buf);
            if n == 0 {
                if self.pending.is_empty() {
                    return None;
                }
                let rest = core::mem::take(&mut self.pending);
                return Some(String::from_utf8_lossy(&rest).into_owned());
            }
            self.pending.extend_from_slice(&buf[..n]);
        }
    }

    fn args(&self) -> Vec<String> {
        self.args.clone()
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, String> {
        fs::read(path).map_err(|e| String::from(fs::error_text(e)))
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        fs::write(path, data).map_err(|e| String::from(fs::error_text(e)))
    }

    fn sleep_ms(&mut self, ms: u64) {
        time::sleep_ms(ms);
    }

    fn now_ms(&self) -> u64 {
        time::uptime_ms()
    }

    fn random(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }
}

fn main(args: &[&str]) -> i32 {
    let Some(&path) = args.get(1) else {
        eprintln!("kullanım: banna-run <program.banna> [argüman...]");
        return 2;
    };
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("banna-run: {}: {}", path, fs::error_text(e));
            return 1;
        }
    };
    let Some(program) = Program::decode(&data) else {
        eprintln!("banna-run: {} bir .banna programı değil (brct build ile derleyin)", path);
        return 1;
    };
    let mut host = BannaHost {
        args: args[1..].iter().map(|a| String::from(*a)).collect(),
        pending: Vec::new(),
        seed: time::now_ns() | 1,
    };
    let mut vm = Vm::new(program, &mut host);
    match vm.run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{}", message);
            101
        }
    }
}
