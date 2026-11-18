//! Piksel döngüleri: SSE2 ile alfa karışımı (her komutta 4 piksel).
//!
//! Çekirdek soft-float derlenir, derleyici XMM yazmaçlarını hiç kullanmaz; zamanlayıcı
//! her thread'in SSE durumunu FXSAVE ile sakladığından elle yazılmış SSE2 burada güvenlidir.

use core::arch::asm;

/// SSE2 karışım döngüsü. Yazmaçları SSE ile derlenen kodda (kullanıcı alanı) bildirilir;
/// çekirdek soft-float olduğundan orada XMM sınıfı kullanılamaz ve gerekmez.
macro_rules! blend_loop {
    ($p:expr, $n:expr, $src:expr, $inv:expr; $($clobber:tt)*) => {
        asm!(
            "movdqu xmm6, [{src}]",
            "movdqu xmm7, [{inv}]",
            "pxor xmm5, xmm5",
            "2:",
            "movdqu xmm0, [{p}]",
            "movdqa xmm1, xmm0",
            "punpcklbw xmm0, xmm5",
            "punpckhbw xmm1, xmm5",
            "pmullw xmm0, xmm7",
            "pmullw xmm1, xmm7",
            "paddw xmm0, xmm6",
            "paddw xmm1, xmm6",
            "psrlw xmm0, 8",
            "psrlw xmm1, 8",
            "packuswb xmm0, xmm1",
            "movdqu [{p}], xmm0",
            "add {p}, 16",
            "dec {n}",
            "jnz 2b",
            p = inout(reg) $p => _,
            n = inout(reg) $n => _,
            src = in(reg) $src,
            inv = in(reg) $inv,
            $($clobber)*
            options(nostack),
        )
    };
}

/// Satırı yarı saydam sabit renkle karıştırır (`color` 0xAARRGGBB).
pub fn blend_const(row: &mut [u32], color: u32) {
    let a = color >> 24;
    if a == 0 {
        return;
    }
    if a == 255 {
        row.fill(color & 0x00ff_ffff);
        return;
    }
    let inv = 255 - a;
    let (r, g, b) = ((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff);
    // Kanal sırası bellekte B, G, R, X; X kanalı 0 kalır.
    let premultiplied: [u16; 8] = {
        let p = [(b * a) as u16, (g * a) as u16, (r * a) as u16, 0];
        [p[0], p[1], p[2], p[3], p[0], p[1], p[2], p[3]]
    };
    let weights = [inv as u16; 8];
    let chunks = row.len() / 4;
    if chunks > 0 {
        unsafe {
            #[cfg(target_feature = "sse2")]
            blend_loop!(row.as_mut_ptr(), chunks, premultiplied.as_ptr(), weights.as_ptr();
                out("xmm0") _, out("xmm1") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,);
            #[cfg(not(target_feature = "sse2"))]
            blend_loop!(row.as_mut_ptr(), chunks, premultiplied.as_ptr(), weights.as_ptr(););
        }
    }
    for px in &mut row[chunks * 4..] {
        *px = super::blend(*px, color);
    }
}

/// Kenar yumuşatmalı yazı: her pikselin örtüsü (`coverage`, 0..255) renk alfasıyla çarpılır.
pub fn blend_coverage(row: &mut [u32], coverage: &[u8], color: u32, alpha: u32) {
    let rgb = color & 0x00ff_ffff;
    for (p, &c) in row.iter_mut().zip(coverage) {
        match c as u32 * alpha {
            0 => {}
            65025 => *p = rgb,
            v => *p = super::blend(*p, rgb | (v / 255) << 24),
        }
    }
}

pub fn self_test() -> bool {
    let colors = [0x80ff_8040u32, 0x1000_00ff, 0xfe12_3456, 0x0100_ff00];
    let base: [u32; 11] = [
        0x0000_0000,
        0x00ff_ffff,
        0x0012_3456,
        0x0080_8080,
        0x00ff_0000,
        0x0000_ff00,
        0x0000_00ff,
        0x0001_0203,
        0x00fe_fdfc,
        0x0055_aa55,
        0x00aa_55aa,
    ];
    colors.iter().all(|&color| {
        let mut fast = base;
        blend_const(&mut fast, color);
        fast.iter()
            .zip(base.iter())
            .all(|(&f, &b)| f == super::blend(b, color))
    })
}
