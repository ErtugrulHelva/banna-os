//! `calc [ifade]`: dört işlem, üs (^), mod (%), parantez ve ondalık sayılar.
//! Argümansız çalışınca satır satır hesaplar (boş satır ya da `q` ile çıkar).

#![no_std]
#![no_main]

use banna::{String, Vec, eprintln, io, print, println};

banna::entry!(main);

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().filter(|c| !c.is_whitespace()).collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn expr(&mut self) -> Result<f64, &'static str> {
        let mut value = self.term()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.pos += 1;
            let rhs = self.term()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, &'static str> {
        let mut value = self.power()?;
        while let Some(op @ ('*' | '/' | '%' | 'x')) = self.peek() {
            self.pos += 1;
            let rhs = self.power()?;
            value = match op {
                '/' | '%' if rhs == 0.0 => return Err("sıfıra bölme"),
                '/' => value / rhs,
                '%' => value - rhs * trunc(value / rhs),
                _ => value * rhs,
            };
        }
        Ok(value)
    }

    fn power(&mut self) -> Result<f64, &'static str> {
        let base = self.unary()?;
        if self.peek() == Some('^') {
            self.pos += 1;
            let exponent = self.power()?;
            return pow(base, exponent);
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<f64, &'static str> {
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Ok(-self.unary()?)
            }
            Some('+') => {
                self.pos += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<f64, &'static str> {
        if self.peek() == Some('(') {
            self.pos += 1;
            let value = self.expr()?;
            if self.peek() != Some(')') {
                return Err("')' eksik");
            }
            self.pos += 1;
            return Ok(value);
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' || c == ',' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err("sayı bekleniyordu");
        }
        let text: String = self.chars[start..self.pos]
            .iter()
            .map(|&c| if c == ',' { '.' } else { c })
            .collect();
        text.parse().map_err(|_| "geçersiz sayı")
    }
}

fn trunc(x: f64) -> f64 {
    if x.abs() >= 9.0e15 {
        x
    } else {
        (x as i64) as f64
    }
}

fn pow(base: f64, exponent: f64) -> Result<f64, &'static str> {
    if exponent != trunc(exponent) || exponent.abs() > 4096.0 {
        return Err("üs tam sayı olmalı (en fazla 4096)");
    }
    let mut result = 1.0;
    for _ in 0..exponent.abs() as u32 {
        result *= base;
    }
    Ok(if exponent < 0.0 { 1.0 / result } else { result })
}

fn evaluate(text: &str) -> Result<f64, &'static str> {
    let mut parser = Parser::new(text);
    let value = parser.expr()?;
    if parser.pos != parser.chars.len() {
        return Err("beklenmeyen karakter");
    }
    Ok(value)
}

fn show(value: f64) -> String {
    if value == trunc(value) && value.abs() < 1.0e15 {
        banna::format!("{}", value as i64)
    } else {
        let text = banna::format!("{:.10}", value);
        String::from(text.trim_end_matches('0').trim_end_matches('.'))
    }
}

fn main(args: &[&str]) -> i32 {
    if args.len() > 1 {
        let text = args[1..].join(" ");
        return match evaluate(&text) {
            Ok(v) => {
                println!("{}", show(v));
                0
            }
            Err(e) => {
                eprintln!("calc: {}", e);
                1
            }
        };
    }
    let mut buf = [0u8; 256];
    loop {
        print!("> ");
        let n = io::read_fd(0, &mut buf);
        if n <= 0 {
            return 0;
        }
        let line = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
        let line = line.trim();
        if line.is_empty() || line == "q" {
            return 0;
        }
        match evaluate(line) {
            Ok(v) => println!("{}", show(v)),
            Err(e) => println!("hata: {}", e),
        }
    }
}
