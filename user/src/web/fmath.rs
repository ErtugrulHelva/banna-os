//! `no_std` kayan nokta işlevleri (core'da yok): yuvarlama, karekök (SSE2), üstel,
//! logaritma ve trigonometri. JavaScript `Math` ve yerleşim hesapları için yeterli doğruluk.

pub fn trunc(x: f64) -> f64 {
    if !x.is_finite() || x.abs() >= 4_503_599_627_370_496.0 {
        x
    } else {
        (x as i64) as f64
    }
}

pub fn floor(x: f64) -> f64 {
    let t = trunc(x);
    if t > x { t - 1.0 } else { t }
}

pub fn ceil(x: f64) -> f64 {
    let t = trunc(x);
    if t < x { t + 1.0 } else { t }
}

/// JavaScript `Math.round`: .5 yukarı yuvarlanır.
pub fn round(x: f64) -> f64 {
    floor(x + 0.5)
}

pub fn floorf(x: f32) -> f32 {
    floor(x as f64) as f32
}

pub fn ceilf(x: f32) -> f32 {
    ceil(x as f64) as f32
}

pub fn roundf(x: f32) -> f32 {
    round(x as f64) as f32
}

pub fn sqrt(x: f64) -> f64 {
    if x < 0.0 {
        return f64::NAN;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let out: f64;
        unsafe {
            core::arch::asm!("sqrtsd {0}, {1}", out(xmm_reg) out, in(xmm_reg) x, options(pure, nomem, nostack));
        }
        out
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // Ana makine testleri için Newton yöntemi.
        if x == 0.0 || x.is_infinite() {
            return x;
        }
        let mut g = if x > 1.0 { x / 2.0 } else { 1.0 };
        for _ in 0..60 {
            g = 0.5 * (g + x / g);
        }
        g
    }
}

const LN2: f64 = core::f64::consts::LN_2;

pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x > 709.0 {
        return f64::INFINITY;
    }
    if x < -745.0 {
        return 0.0;
    }
    // e^x = 2^k · e^r, |r| ≤ ln2/2
    let k = round(x / LN2);
    let r = x - k * LN2;
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..30 {
        term *= r / n as f64;
        sum += term;
        if term.abs() < 1e-17 * sum.abs() {
            break;
        }
    }
    sum * pow2(k as i32)
}

fn pow2(k: i32) -> f64 {
    if k > 1023 {
        return pow2(1023) * pow2(k - 1023);
    }
    if k < -1022 {
        return pow2(-1022) * pow2(k + 1022);
    }
    f64::from_bits(((k + 1023) as u64) << 52)
}

pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return x;
    }
    // x = m · 2^e, m ∈ [1, 2)
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i64 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if e == -1023 {
        // Alt normal sayılar.
        return ln(x * pow2(54)) - 54.0 * LN2;
    }
    if m > core::f64::consts::SQRT_2 {
        m /= 2.0;
        e += 1;
    }
    // ln(m) = 2·atanh((m-1)/(m+1))
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut term = s;
    let mut sum = 0.0;
    let mut n = 1.0;
    while n < 60.0 {
        sum += term / n;
        term *= s2;
        n += 2.0;
    }
    2.0 * sum + e as f64 * LN2
}

pub fn log10(x: f64) -> f64 {
    ln(x) / core::f64::consts::LN_10
}

pub fn log2(x: f64) -> f64 {
    ln(x) / LN2
}

pub fn pow(base: f64, exp_: f64) -> f64 {
    if exp_ == 0.0 {
        return 1.0;
    }
    if base.is_nan() || exp_.is_nan() {
        return f64::NAN;
    }
    if exp_ == trunc(exp_) && exp_.abs() < 1024.0 {
        let mut result = 1.0;
        let mut b = base;
        let mut n = exp_.abs() as u64;
        while n > 0 {
            if n & 1 == 1 {
                result *= b;
            }
            b *= b;
            n >>= 1;
        }
        return if exp_ < 0.0 { 1.0 / result } else { result };
    }
    if base < 0.0 {
        return f64::NAN;
    }
    if base == 0.0 {
        return if exp_ > 0.0 { 0.0 } else { f64::INFINITY };
    }
    exp(exp_ * ln(base))
}

const PI: f64 = core::f64::consts::PI;

/// Açıyı [-π, π] aralığına indirir.
fn reduce(x: f64) -> f64 {
    let two_pi = 2.0 * PI;
    x - two_pi * round(x / two_pi)
}

pub fn sin(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let mut r = reduce(x);
    // [-π/2, π/2] aralığına katla.
    if r > PI / 2.0 {
        r = PI - r;
    } else if r < -PI / 2.0 {
        r = -PI - r;
    }
    let r2 = r * r;
    let mut term = r;
    let mut sum = r;
    for n in 1..14 {
        term *= -r2 / ((2 * n) as f64 * (2 * n + 1) as f64);
        sum += term;
    }
    sum
}

pub fn cos(x: f64) -> f64 {
    sin(x + PI / 2.0)
}

pub fn tan(x: f64) -> f64 {
    sin(x) / cos(x)
}

pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.abs() > 1.0 {
        let r = PI / 2.0 - atan(1.0 / x.abs());
        return if x < 0.0 { -r } else { r };
    }
    // Yakınsamayı hızlandırmak için yarım açı: atan(x) = 2·atan(x / (1 + √(1+x²)))
    let y = x / (1.0 + sqrt(1.0 + x * x));
    let y2 = y * y;
    let mut term = y;
    let mut sum = 0.0;
    let mut n = 1.0;
    let mut sign = 1.0;
    while n < 80.0 {
        sum += sign * term / n;
        term *= y2;
        n += 2.0;
        sign = -sign;
    }
    2.0 * sum
}

pub fn atan2(y: f64, x: f64) -> f64 {
    if x > 0.0 {
        atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 {
            atan(y / x) + PI
        } else {
            atan(y / x) - PI
        }
    } else if y > 0.0 {
        PI / 2.0
    } else if y < 0.0 {
        -PI / 2.0
    } else {
        0.0
    }
}

pub fn asin(x: f64) -> f64 {
    if x.abs() > 1.0 {
        return f64::NAN;
    }
    atan2(x, sqrt(1.0 - x * x))
}

pub fn acos(x: f64) -> f64 {
    if x.abs() > 1.0 {
        return f64::NAN;
    }
    atan2(sqrt(1.0 - x * x), x)
}
