//! Intel High Definition Audio denetleyicisi ve codec'i.
//!
//! Codec ile CORB/RIRB halkaları üzerinden konuşulur. Ses işlev grubundaki fiziksel
//! olarak bağlı her çıkış pini (hat çıkışı, hoparlör, kulaklık) bağlantı listeleri
//! izlenerek bir DAC'a bağlanır; yol üzerindeki amfiler açılır. Çıkış akışı 48 kHz,
//! 16 bit, stereo; DMA tamponu döngüseldir ve mikser thread'i tarafından doldurulur.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use crate::arch::pit;
use crate::drivers::pci::{self, Device as PciDevice};
use crate::drivers::usb::Dma;
use crate::memory::paging::{NO_CACHE, map_physical};

const GCAP: u64 = 0x00;
const GCTL: u64 = 0x08;
const STATESTS: u64 = 0x0e;
const INTCTL: u64 = 0x20;
const CORB_LBASE: u64 = 0x40;
const CORB_UBASE: u64 = 0x44;
const CORB_WP: u64 = 0x48;
const CORB_RP: u64 = 0x4a;
const CORB_CTL: u64 = 0x4c;
const CORB_SIZE: u64 = 0x4e;
const RIRB_LBASE: u64 = 0x50;
const RIRB_UBASE: u64 = 0x54;
const RIRB_WP: u64 = 0x58;
const RINTCNT: u64 = 0x5a;
const RIRB_CTL: u64 = 0x5c;
const RIRB_STS: u64 = 0x5d;
const RIRB_SIZE: u64 = 0x5e;

/// 48 kHz, 16 bit, 2 kanal.
pub const FORMAT: u16 = 0x0011;
pub const STREAM_TAG: u32 = 1;
/// Döngüsel çıkış tamponu (iki BDL girdisi).
pub const BUFFER_BYTES: usize = 64 * 1024;

const WIDGET_OUTPUT: u32 = 0;
const WIDGET_MIXER: u32 = 2;
const WIDGET_SELECTOR: u32 = 3;
const WIDGET_PIN: u32 = 4;

pub struct Hda {
    base: u64,
    corb: Dma,
    rirb: Dma,
    rirb_read: u16,
    stream: u64,
    pub buffer: Dma,
    bdl: Dma,
    codec: u32,
    /// Ses seviyesi uygulanan amfiler: (düğüm, çıkış amfisi mi, giriş dizini, 0 dB adımı).
    amps: Vec<(u32, bool, u32, u32)>,
    pub outputs: Vec<&'static str>,
}

unsafe impl Send for Hda {}

impl Hda {
    fn r8(&self, off: u64) -> u8 {
        unsafe { read_volatile((self.base + off) as *const u8) }
    }
    fn r16(&self, off: u64) -> u16 {
        unsafe { read_volatile((self.base + off) as *const u16) }
    }
    fn r32(&self, off: u64) -> u32 {
        unsafe { read_volatile((self.base + off) as *const u32) }
    }
    fn w8(&self, off: u64, v: u8) {
        unsafe { write_volatile((self.base + off) as *mut u8, v) }
    }
    fn w16(&self, off: u64, v: u16) {
        unsafe { write_volatile((self.base + off) as *mut u16, v) }
    }
    fn w32(&self, off: u64, v: u32) {
        unsafe { write_volatile((self.base + off) as *mut u32, v) }
    }

    /// Codec'e komut gönderir ve yanıtı bekler.
    fn command(&mut self, nid: u32, verb: u32, payload: u32) -> Option<u32> {
        let cmd = if verb >= 0x100 {
            self.codec << 28 | nid << 20 | verb << 8 | (payload & 0xff)
        } else {
            self.codec << 28 | nid << 20 | verb << 16 | (payload & 0xffff)
        };
        let wp = (self.r16(CORB_WP) as usize + 1) % 256;
        unsafe { write_volatile((self.corb.virt as *mut u32).add(wp), cmd) };
        self.w16(CORB_WP, wp as u16);
        let deadline = pit::uptime_ms() + 100;
        while self.r16(RIRB_WP) & 0xff == self.rirb_read {
            if pit::uptime_ms() > deadline {
                return None;
            }
            core::hint::spin_loop();
        }
        self.rirb_read = (self.rirb_read + 1) % 256;
        // Denetleyici RINTCNT yanıttan sonra durur; yanıt bayrağı temizlenince sürer.
        self.w8(RIRB_STS, 0x05);
        Some(unsafe {
            read_volatile((self.rirb.virt as *const u32).add(self.rirb_read as usize * 2))
        })
    }

    fn param(&mut self, nid: u32, id: u32) -> u32 {
        self.command(nid, 0xf00, id).unwrap_or(0)
    }

    fn connections(&mut self, nid: u32) -> Vec<u32> {
        let info = self.param(nid, 0x0e);
        let long = info & 0x80 != 0;
        let count = info & 0x7f;
        let mut out = Vec::new();
        let per = if long { 2 } else { 4 };
        let mut i = 0;
        while i < count {
            let raw = self.command(nid, 0xf02, i).unwrap_or(0);
            for k in 0..per {
                if i + k >= count {
                    break;
                }
                let entry = if long {
                    (raw >> (16 * k)) & 0xffff
                } else {
                    (raw >> (8 * k)) & 0xff
                };
                let range = entry & if long { 0x8000 } else { 0x80 } != 0;
                let id = entry & if long { 0x7fff } else { 0x7f };
                if range && let Some(&last) = out.last() {
                    for n in last + 1..=id {
                        out.push(n);
                    }
                } else {
                    out.push(id);
                }
            }
            i += per;
        }
        out
    }

    fn widget_type(&mut self, nid: u32) -> u32 {
        (self.param(nid, 0x09) >> 20) & 0xf
    }

    /// Pinden bir DAC'a giden yol (pin dahil, DAC sonda).
    fn path_to_dac(&mut self, nid: u32, depth: u32) -> Option<Vec<u32>> {
        if depth > 6 {
            return None;
        }
        let kind = self.widget_type(nid);
        if kind == WIDGET_OUTPUT {
            return Some(alloc::vec![nid]);
        }
        if !matches!(kind, WIDGET_PIN | WIDGET_MIXER | WIDGET_SELECTOR) {
            return None;
        }
        for (index, next) in self.connections(nid).into_iter().enumerate() {
            if let Some(mut path) = self.path_to_dac(next, depth + 1) {
                if kind != WIDGET_MIXER {
                    self.command(nid, 0x701, index as u32);
                }
                // Karıştırıcının ilgili girişi açılır (0 dB).
                if kind == WIDGET_MIXER {
                    let steps = self.param(nid, 0x0d) & 0x7f;
                    self.command(nid, 0x3, 0x7000 | (index as u32) << 8 | steps);
                }
                path.insert(0, nid);
                return Some(path);
            }
        }
        None
    }

    fn unmute_output(&mut self, nid: u32) {
        let caps = self.param(nid, 0x09);
        if caps & (1 << 2) == 0 {
            return;
        }
        let amp = self.param(nid, 0x12);
        let zero_db = amp & 0x7f;
        self.command(nid, 0x3, 0xb000 | zero_db);
        self.amps.push((nid, true, 0, zero_db));
    }

    /// Ses seviyesi (0..100): yol üzerindeki çıkış amfilerine uygulanır.
    pub fn set_volume(&mut self, percent: u32) {
        let amps = self.amps.clone();
        for (nid, output, index, zero_db) in amps {
            let gain = zero_db * percent.min(100) / 100;
            let mute = if percent == 0 { 0x80 } else { 0 };
            let dir = if output { 0xb000 } else { 0x7000 | index << 8 };
            self.command(nid, 0x3, dir | mute | gain);
        }
    }

    fn setup_codec(&mut self) -> Result<(), &'static str> {
        let nodes = self.param(0, 0x04);
        let (start, count) = ((nodes >> 16) & 0xff, nodes & 0xff);
        let afg = (start..start + count)
            .find(|&n| self.param(n, 0x05) & 0xff == 1)
            .ok_or("no audio function group")?;
        self.command(afg, 0x7ff, 0);
        crate::sched::sleep_ms(1);
        self.command(afg, 0x705, 0);
        let nodes = self.param(afg, 0x04);
        let (first, count) = ((nodes >> 16) & 0xff, nodes & 0xff);
        let mut dacs = Vec::new();
        for nid in first..first + count {
            self.command(nid, 0x705, 0);
            let caps = self.param(nid, 0x09);
            if (caps >> 20) & 0xf != WIDGET_PIN || caps & (1 << 9) != 0 {
                continue;
            }
            let pin_caps = self.param(nid, 0x0c);
            let config = self.command(nid, 0xf1c, 0).unwrap_or(0);
            let connected = config >> 30 != 1;
            let device = (config >> 20) & 0xf;
            if pin_caps & (1 << 4) == 0 || !connected || !matches!(device, 0..=2) {
                continue;
            }
            let Some(path) = self.path_to_dac(nid, 0) else {
                continue;
            };
            let control = if device == 2 { 0xc0 } else { 0x40 };
            self.command(nid, 0x707, control);
            if pin_caps & (1 << 16) != 0 {
                self.command(nid, 0x70c, 0x02);
            }
            for &node in &path {
                self.command(node, 0x705, 0);
                self.unmute_output(node);
            }
            let dac = *path.last().unwrap();
            if !dacs.contains(&dac) {
                dacs.push(dac);
            }
            self.outputs.push(match device {
                0 => "line out",
                1 => "speaker",
                _ => "headphone",
            });
        }
        if dacs.is_empty() {
            return Err("no connected output pin");
        }
        for dac in dacs {
            self.command(dac, 0x706, STREAM_TAG << 4);
            self.command(dac, 0x2, FORMAT as u32);
        }
        Ok(())
    }

    fn setup_stream(&mut self) {
        let s = self.stream;
        self.w8(s, 1);
        let deadline = pit::uptime_ms() + 50;
        while self.r8(s) & 1 == 0 && pit::uptime_ms() < deadline {
            core::hint::spin_loop();
        }
        self.w8(s, 0);
        let deadline = pit::uptime_ms() + 50;
        while self.r8(s) & 1 != 0 && pit::uptime_ms() < deadline {
            core::hint::spin_loop();
        }
        self.w32(s + 0x08, BUFFER_BYTES as u32);
        self.w16(s + 0x0c, 1);
        self.w16(s + 0x12, FORMAT);
        self.w32(s + 0x18, self.bdl.phys as u32);
        self.w32(s + 0x1c, (self.bdl.phys >> 32) as u32);
        self.w8(s + 2, (STREAM_TAG << 4) as u8);
        self.w8(s, 0x02);
    }

    /// Donanımın döngüsel tamponda çaldığı konum (bayt).
    pub fn position(&self) -> usize {
        self.r32(self.stream + 4) as usize % BUFFER_BYTES
    }
}

fn init_controller(dev: &PciDevice) -> Result<Hda, &'static str> {
    dev.enable();
    if dev.vendor_id == 0x8086 {
        // TCSEL (0x44) bit 0-2 temizlenir: bazı codec'lerde çalma sırasında cızırtıyı önler.
        let tcsel = dev.read(0x44);
        dev.write(0x44, tcsel & !0x7);
    }
    let bar = dev.bar(0).ok_or("no MMIO BAR")?;
    let base = map_physical(bar, 0x4000, NO_CACHE).map_err(|_| "BAR mapping failed")? as u64;
    let r16 = |off: u64| unsafe { read_volatile((base + off) as *const u16) };
    let r32 = |off: u64| unsafe { read_volatile((base + off) as *const u32) };
    let w32 = |off: u64, v: u32| unsafe { write_volatile((base + off) as *mut u32, v) };

    w32(GCTL, r32(GCTL) & !1);
    let deadline = pit::uptime_ms() + 100;
    while r32(GCTL) & 1 != 0 && pit::uptime_ms() < deadline {
        core::hint::spin_loop();
    }
    crate::sched::sleep_ms(1);
    w32(GCTL, r32(GCTL) | 1);
    let deadline = pit::uptime_ms() + 100;
    while r32(GCTL) & 1 == 0 {
        if pit::uptime_ms() > deadline {
            return Err("controller reset timed out");
        }
        core::hint::spin_loop();
    }
    crate::sched::sleep_ms(2);
    let codecs = r16(STATESTS);
    if codecs == 0 {
        return Err("no codec");
    }
    let codec = codecs.trailing_zeros();
    let all_codecs = codecs;
    let gcap = r16(GCAP);
    let input_streams = ((gcap >> 8) & 0xf) as u64;
    let output_streams = ((gcap >> 12) & 0xf) as u64;
    if output_streams == 0 {
        return Err("no output stream");
    }

    let corb = Dma::alloc(1).ok_or("out of memory")?;
    let rirb = Dma::alloc(1).ok_or("out of memory")?;
    let buffer = Dma::alloc((BUFFER_BYTES / 4096) as u64).ok_or("out of memory")?;
    let bdl = Dma::alloc(1).ok_or("out of memory")?;
    let half = BUFFER_BYTES as u64 / 2;
    unsafe {
        let entries = bdl.virt as *mut u32;
        for i in 0..2u64 {
            let e = entries.add(i as usize * 4);
            let addr = buffer.phys + i * half;
            write_volatile(e, addr as u32);
            write_volatile(e.add(1), (addr >> 32) as u32);
            write_volatile(e.add(2), half as u32);
            write_volatile(e.add(3), 0);
        }
    }
    let mut hda = Hda {
        base,
        corb,
        rirb,
        rirb_read: 0,
        stream: 0x80 + input_streams * 0x20,
        buffer,
        bdl,
        codec,
        amps: Vec::new(),
        outputs: Vec::new(),
    };
    hda.w32(INTCTL, 0);
    hda.w8(CORB_CTL, 0);
    hda.w8(RIRB_CTL, 0);
    crate::sched::sleep_ms(1);
    hda.w32(CORB_LBASE, hda.corb.phys as u32);
    hda.w32(CORB_UBASE, (hda.corb.phys >> 32) as u32);
    hda.w8(CORB_SIZE, 0x02);
    hda.w16(CORB_RP, 1 << 15);
    let deadline = pit::uptime_ms() + 10;
    while hda.r16(CORB_RP) & (1 << 15) == 0 && pit::uptime_ms() < deadline {
        core::hint::spin_loop();
    }
    hda.w16(CORB_RP, 0);
    hda.w16(CORB_WP, 0);
    hda.w32(RIRB_LBASE, hda.rirb.phys as u32);
    hda.w32(RIRB_UBASE, (hda.rirb.phys >> 32) as u32);
    hda.w8(RIRB_SIZE, 0x02);
    hda.w16(RIRB_WP, 1 << 15);
    hda.w16(RINTCNT, 0xff);
    hda.w8(CORB_CTL, 0x02);
    hda.w8(RIRB_CTL, 0x02);
    crate::sched::sleep_ms(1);
    hda.rirb_read = hda.r16(RIRB_WP) & 0xff;

    // Analog çıkışı olan ilk codec (ör. Realtek ALC245; Intel HDMI codec'i atlanır).
    let mut found = Err("no codec with analog outputs");
    for address in (0..15).filter(|a| all_codecs & (1 << a) != 0) {
        hda.codec = address;
        hda.amps.clear();
        hda.outputs.clear();
        found = hda.setup_codec();
        if found.is_ok() {
            break;
        }
    }
    found?;
    let vendor = hda.param(0, 0x00);
    hda.setup_stream();
    crate::log!(
        "hda: {:02x}:{:02x}.{} codec {:08x}, outputs: {}",
        dev.bus,
        dev.slot,
        dev.function,
        vendor,
        hda.outputs.join(", ")
    );
    Ok(hda)
}

/// Ses denetleyicileri: Intel'inkiler önce (Skylake sonrası dizüstülerde DSP'li "cAVS",
/// sınıf 04:01, klasik HDA arayüzüyle de sürülür), sonra diğer HD Audio aygıtları (ör.
/// yalnızca HDMI sesi olan NVIDIA denetleyicisi).
pub fn probe() -> Option<Hda> {
    let mut candidates: Vec<PciDevice> = pci::find_all(0x04, 0x03);
    candidates.extend(
        pci::find_all(0x04, 0x01)
            .into_iter()
            .filter(|d| d.vendor_id == 0x8086),
    );
    candidates.sort_by_key(|d| d.vendor_id != 0x8086);
    candidates
        .into_iter()
        .find_map(|dev| match init_controller(&dev) {
            Ok(hda) => Some(hda),
            Err(e) => {
                crate::log!(
                    "hda: {:02x}:{:02x}.{}: {}",
                    dev.bus,
                    dev.slot,
                    dev.function,
                    e
                );
                None
            }
        })
}
