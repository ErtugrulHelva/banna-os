//! CPUID ayrıntıları: model, özellikler, önbellekler, hipervizör ve sıcaklık.

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::{__cpuid, __cpuid_count};

use super::cpu;

pub struct Info {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub features: Vec<&'static str>,
    pub hypervisor: Option<String>,
    pub caches: Vec<(u32, &'static str, u32)>,
    pub base_mhz: Option<u32>,
    pub logical_per_package: u32,
}

fn text(regs: &[u32]) -> String {
    let bytes: Vec<u8> = regs.iter().flat_map(|r| r.to_le_bytes()).collect();
    String::from_utf8_lossy(&bytes)
        .trim_matches(char::from(0))
        .trim()
        .into()
}

fn max_leaf(base: u32) -> u32 {
    __cpuid(base).eax
}

pub fn read() -> Info {
    let v = __cpuid(0);
    let vendor = text(&[v.ebx, v.edx, v.ecx]);
    let brand = cpu::brand()
        .map(|b| {
            String::from_utf8_lossy(&b)
                .trim_matches(char::from(0))
                .trim()
                .into()
        })
        .unwrap_or_default();
    let l1 = __cpuid(1);
    let base_family = (l1.eax >> 8) & 0xf;
    let family = if base_family == 0xf {
        base_family + ((l1.eax >> 20) & 0xff)
    } else {
        base_family
    };
    let model = if base_family == 0x6 || base_family == 0xf {
        ((l1.eax >> 4) & 0xf) | ((l1.eax >> 12) & 0xf0)
    } else {
        (l1.eax >> 4) & 0xf
    };

    let mut features = Vec::new();
    let edx1 = [
        (0, "fpu"),
        (4, "tsc"),
        (5, "msr"),
        (9, "apic"),
        (15, "cmov"),
        (19, "clflush"),
        (23, "mmx"),
        (25, "sse"),
        (26, "sse2"),
        (28, "htt"),
    ];
    let ecx1 = [
        (0, "sse3"),
        (9, "ssse3"),
        (12, "fma"),
        (13, "cx16"),
        (19, "sse4.1"),
        (20, "sse4.2"),
        (21, "x2apic"),
        (22, "movbe"),
        (23, "popcnt"),
        (25, "aes"),
        (26, "xsave"),
        (28, "avx"),
        (29, "f16c"),
        (30, "rdrand"),
        (31, "hypervisor"),
    ];
    for (bit, name) in edx1 {
        if l1.edx & (1 << bit) != 0 {
            features.push(name);
        }
    }
    for (bit, name) in ecx1 {
        if l1.ecx & (1 << bit) != 0 {
            features.push(name);
        }
    }
    if max_leaf(0) >= 7 {
        let l7 = __cpuid_count(7, 0);
        for (bit, name) in [
            (3, "bmi1"),
            (5, "avx2"),
            (7, "smep"),
            (8, "bmi2"),
            (16, "avx512f"),
            (18, "rdseed"),
            (20, "smap"),
            (29, "sha"),
        ] {
            if l7.ebx & (1 << bit) != 0 {
                features.push(name);
            }
        }
    }
    if max_leaf(0x8000_0000) >= 0x8000_0001 {
        let ext = __cpuid(0x8000_0001);
        for (bit, name) in [
            (11, "syscall"),
            (20, "nx"),
            (26, "1gb-pages"),
            (27, "rdtscp"),
            (29, "long-mode"),
        ] {
            if ext.edx & (1 << bit) != 0 {
                features.push(name);
            }
        }
    }
    if max_leaf(0x8000_0000) >= 0x8000_0007 && __cpuid(0x8000_0007).edx & (1 << 8) != 0 {
        features.push("invariant-tsc");
    }

    let hypervisor = (l1.ecx & (1 << 31) != 0).then(|| {
        let h = __cpuid(0x4000_0000);
        text(&[h.ebx, h.ecx, h.edx])
    });

    let mut caches = Vec::new();
    let leaf = if vendor == "GenuineIntel" {
        4
    } else {
        0x8000_001d
    };
    let supported = if leaf == 4 {
        max_leaf(0) >= 4
    } else {
        max_leaf(0x8000_0000) >= 0x8000_001d
    };
    if supported {
        for sub in 0..8 {
            let c = __cpuid_count(leaf, sub);
            let kind = c.eax & 0x1f;
            if kind == 0 {
                break;
            }
            let level = (c.eax >> 5) & 0x7;
            let ways = ((c.ebx >> 22) & 0x3ff) + 1;
            let partitions = ((c.ebx >> 12) & 0x3ff) + 1;
            let line = (c.ebx & 0xfff) + 1;
            let sets = c.ecx + 1;
            let kib = ways * partitions * line * sets / 1024;
            let name = match kind {
                1 => "data",
                2 => "instruction",
                _ => "unified",
            };
            caches.push((level, name, kib));
        }
    }

    let base_mhz = (max_leaf(0) >= 0x16)
        .then(|| __cpuid(0x16).eax & 0xffff)
        .filter(|&m| m > 0);
    Info {
        vendor,
        brand,
        family,
        model,
        stepping: l1.eax & 0xf,
        features,
        hypervisor,
        caches,
        base_mhz,
        logical_per_package: (l1.ebx >> 16) & 0xff,
    }
}

/// Hibrit Intel işlemcide (Alder/Raptor Lake...) geçerli çekirdek verimlilik (E) çekirdeği mi?
/// CPUID 0x1A: EAX[31:24] = 0x20 Atom (E), 0x40 Core (P).
pub fn is_efficiency_core() -> bool {
    let max = __cpuid(0).eax;
    let hybrid = __cpuid_count(7, 0).edx & (1 << 15) != 0;
    max >= 0x1a && hybrid && (__cpuid(0x1a).eax >> 24) == 0x20
}

/// Yazılımsal öykünme altında mı (QEMU TCG; ör. Apple Silicon'da UTM ile x86_64)? Her
/// komut ve kesme pahalıdır: masaüstü daha düşük çözünürlük ve animasyonsuz başlar.
pub fn emulated() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    static CACHED: AtomicU8 = AtomicU8::new(0);
    match CACHED.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    let l1 = __cpuid(1);
    let tcg = l1.ecx & (1 << 31) != 0 && {
        let h = __cpuid(0x4000_0000);
        text(&[h.ebx, h.ecx, h.edx]) == "TCGTCGTCGTCG"
    };
    CACHED.store(if tcg { 2 } else { 1 }, Ordering::Relaxed);
    tcg
}
