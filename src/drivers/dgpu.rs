//! Sürücüsü olmayan ayrık ekran kartını (Optimus dizüstülerde NVIDIA) güç tasarruf kipine
//! (PCI D3hot) alır: boşta güç çekip ısınması işlemciyi de kısar. Ekranı (UEFI
//! framebuffer'ı) o kart sürüyorsa dokunulmaz.

use alloc::vec::Vec;
use core::fmt::Write;

use crate::drivers::pci::{self, Device};
use crate::kernel::initcall::Detail;

/// BAR'ların (taban, boyut) listesi; boyut ölçülürken bellek çözümlemesi kapatılır.
fn bar_ranges(dev: &Device) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let command = dev.read(0x04);
    dev.write(0x04, command & !0x3);
    let mut index = 0u8;
    while index < 6 {
        let offset = 0x10 + index * 4;
        let low = dev.read(offset);
        if low & 1 != 0 || low == 0 {
            index += 1;
            continue;
        }
        let wide = (low >> 1) & 0x3 == 0x2 && index < 5;
        let high = if wide { dev.read(offset + 4) } else { 0 };
        dev.write(offset, u32::MAX);
        let mask_low = dev.read(offset) & !0xf;
        dev.write(offset, low);
        let mut mask = mask_low as u64 | 0xffff_ffff_0000_0000;
        if wide {
            dev.write(offset + 4, u32::MAX);
            mask = (dev.read(offset + 4) as u64) << 32 | mask_low as u64;
            dev.write(offset + 4, high);
        }
        let base = (low & !0xf) as u64 | (high as u64) << 32;
        let size = (!mask).wrapping_add(1);
        if size != 0 {
            out.push((base, size));
        }
        index += if wide { 2 } else { 1 };
    }
    dev.write(0x04, command);
    out
}

fn framebuffer_phys() -> Option<u64> {
    let response = crate::boot::FRAMEBUFFER.response()?;
    let fb = response.framebuffers().next()?;
    let address = fb.address as u64;
    let hhdm = crate::memory::hhdm_offset();
    Some(if address >= hhdm {
        address - hhdm
    } else {
        address
    })
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    if crate::boot::has_flag("nodgpu") {
        let _ = write!(detail, "skipped (nodgpu)");
        return Ok(());
    }
    if crate::arch::cpuinfo::emulated() {
        let _ = write!(detail, "skipped (emulated)");
        return Ok(());
    }
    let fb = framebuffer_phys();
    let mut parked = 0;
    for dev in pci::devices()
        .into_iter()
        .filter(|d| d.vendor_id == 0x10de && d.class == 0x03)
    {
        let drives_display = fb.is_some_and(|fb| {
            bar_ranges(&dev)
                .iter()
                .any(|&(base, size)| (base..base.saturating_add(size)).contains(&fb))
        });
        if drives_display {
            crate::log!("dgpu: {:04x} drives the display, left on", dev.device_id);
            continue;
        }
        let Some(&pm) = dev.capabilities(0x01).first() else {
            continue;
        };
        let control = dev.read(pm + 4);
        dev.write(pm + 4, (control & !0x3) | 0x3);
        let state = dev.read(pm + 4) & 0x3;
        crate::log!(
            "dgpu: NVIDIA {:04x} at {:02x}:{:02x}.{} -> D{}",
            dev.device_id,
            dev.bus,
            dev.slot,
            dev.function,
            state
        );
        if state == 3 {
            parked += 1;
        }
    }
    if parked > 0 {
        let _ = write!(detail, "{} discrete GPU(s) put to D3", parked);
    } else {
        let _ = write!(detail, "nothing to do");
    }
    Ok(())
}

crate::initcall!(driver, 46, "Discrete GPU power", init);
