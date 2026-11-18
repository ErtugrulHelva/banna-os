#![no_std]
#![no_main]

use banna::{println, process};

banna::entry!(main);

/// Uzun bir kayan nokta hesabı; başka işlemler araya girse de sonuç aynı olmalı.
fn compute(seed: f64) -> f64 {
    let mut x = seed;
    let mut acc = 0.0f64;
    for i in 0..200_000 {
        x = x * 1.000_000_1 + 0.5 / (i as f64 + 1.0);
        acc += x.sqrt_approx() * 1e-6;
        if i % 20_000 == 0 {
            process::yield_now();
        }
    }
    acc
}

trait SqrtApprox {
    fn sqrt_approx(self) -> f64;
}

impl SqrtApprox for f64 {
    fn sqrt_approx(self) -> f64 {
        let mut g = if self > 1.0 { self / 2.0 } else { 1.0 };
        for _ in 0..6 {
            g = 0.5 * (g + self / g);
        }
        g
    }
}

fn main(args: &[&str]) -> i32 {
    let seed: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.5);
    let first = compute(seed);
    let second = compute(seed);
    let ok = first.to_bits() == second.to_bits();
    println!(
        "fputest seed {}: {} ({})",
        seed,
        first,
        if ok { "stable" } else { "MISMATCH" }
    );
    if ok { 0 } else { 1 }
}
