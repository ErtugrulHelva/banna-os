//! USB ses sınıfı (UAC 1 ve 2) çıkışı: kulaklık, USB ses kartı, USB hoparlör.
//!
//! 48 kHz, 16 bit, stereo PCM destekleyen ilk AudioStreaming alternatif ayarı seçilir;
//! örnekler isochronous OUT akışıyla (xHCI) ses katmanının kuyruğundan çekilir. Geri
//! bildirim uç noktası (asenkron cihazlar) kullanılmaz: uzun çalmada çok küçük kayma olabilir.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::host::{self, EndpointConfig, Setup};
use super::{InterfaceInfo, Speed, UsbDevice};
use crate::sync::SpinLock;

const RATE: u32 = crate::drivers::audio::RATE;
/// Sınıfa özgü arayüz tanımlayıcısı alt türleri.
const AS_GENERAL: u8 = 0x01;
const FORMAT_TYPE: u8 = 0x02;
const CLOCK_SOURCE: u8 = 0x0a;
/// Kuyrukta tutulan TD sayısı (her biri bir hizmet aralığı): ~32 ms ses.
const QUEUE_MS: usize = 32;

/// Çalan cihaz (denetleyici, yuva).
static ACTIVE: SpinLock<Option<(usize, u8)>> = SpinLock::new(None);

struct Choice {
    interface: u8,
    alternate: u8,
    endpoint: u8,
    max_packet: u16,
    interval: u8,
}

/// AudioStreaming alternatifinin biçimi uygun mu (PCM, 2 kanal, 16 bit, 48 kHz).
fn suitable(iface: &InterfaceInfo, uac2: bool) -> bool {
    let mut pcm = false;
    let mut channels = 0;
    let mut bits = 0;
    let mut rate_ok = uac2;
    for d in descriptors(&iface.extra) {
        match (d.get(2).copied(), uac2) {
            (Some(AS_GENERAL), false) if d.len() >= 7 => {
                pcm = u16::from_le_bytes([d[5], d[6]]) == 1;
            }
            (Some(AS_GENERAL), true) if d.len() >= 11 => {
                pcm = d[5] == 1 && u32::from_le_bytes([d[6], d[7], d[8], d[9]]) & 1 != 0;
                channels = d[10];
            }
            (Some(FORMAT_TYPE), false) if d.len() >= 8 && d[3] == 1 => {
                channels = d[4];
                bits = d[6];
                let count = d[7] as usize;
                let rates: Vec<u32> = d[8..]
                    .chunks(3)
                    .filter(|c| c.len() == 3)
                    .map(|c| c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16)
                    .collect();
                rate_ok = if count == 0 {
                    rates.len() >= 2 && rates[0] <= RATE && RATE <= rates[1]
                } else {
                    rates.contains(&RATE)
                };
            }
            (Some(FORMAT_TYPE), true) if d.len() >= 6 && d[3] == 1 => bits = d[5],
            _ => {}
        }
    }
    pcm && channels == 2 && bits == 16 && rate_ok
}

/// Arka arkaya dizilmiş tanımlayıcıları böler.
fn descriptors(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    core::iter::from_fn(move || {
        let len = *rest.first()? as usize;
        if len < 2 || len > rest.len() {
            return None;
        }
        let (d, tail) = rest.split_at(len);
        rest = tail;
        Some(d)
    })
}

fn choose(interfaces: &[InterfaceInfo], uac2: bool) -> Option<Choice> {
    interfaces
        .iter()
        .filter(|i| i.class == 1 && i.subclass == 2 && i.alternate != 0)
        .filter(|i| suitable(i, uac2))
        .find_map(|i| {
            let ep = i
                .endpoints
                .iter()
                .find(|e| e.transfer_type() == 1 && !e.is_in())?;
            Some(Choice {
                interface: i.number,
                alternate: i.alternate,
                endpoint: ep.address,
                max_packet: ep.max_packet,
                interval: ep.interval,
            })
        })
}

fn set_rate(device: &UsbDevice, control: &InterfaceInfo, choice: &Choice, uac2: bool) {
    let ok = if uac2 {
        // UAC2: örnekleme hızı saat kaynağı varlığına yazılır.
        let clock = descriptors(&control.extra)
            .find(|d| d.len() >= 4 && d[2] == CLOCK_SOURCE)
            .map(|d| d[3]);
        clock.is_some_and(|id| {
            let mut data = RATE.to_le_bytes();
            device
                .control(
                    Setup {
                        request_type: 0x21,
                        request: 0x01,
                        value: 0x0100,
                        index: (id as u16) << 8 | control.number as u16,
                        length: 4,
                    },
                    &mut data,
                )
                .is_ok()
        })
    } else {
        let mut data = [RATE as u8, (RATE >> 8) as u8, (RATE >> 16) as u8];
        device
            .control(
                Setup {
                    request_type: 0x22,
                    request: 0x01,
                    value: 0x0100,
                    index: choice.endpoint as u16,
                    length: 3,
                },
                &mut data,
            )
            .is_ok()
    };
    if !ok {
        crate::log!("uac: sample rate request not accepted, using the device default");
    }
}

/// AudioControl arayüzüne bağlanır; çıkış akışı kurulursa sürücü adı.
pub fn attach(
    device: &UsbDevice,
    control: &InterfaceInfo,
    interfaces: &[InterfaceInfo],
) -> Option<&'static str> {
    if ACTIVE.lock().is_some() {
        return None;
    }
    let uac2 = control.protocol == 0x20;
    let Some(choice) = choose(interfaces, uac2) else {
        crate::log!("uac: '{}' has no 48 kHz 16-bit stereo output", device.name);
        return None;
    };
    // Hizmet aralığı: FS'de 2^(b-1) ms, HS/SS'de 2^(b-1) × 125 µs.
    let exponent = choice.interval.clamp(1, 16) as u32 - 1;
    let interval_us = match device.speed {
        Speed::Low | Speed::Full => 1000 << exponent,
        _ => 125 << exponent,
    };
    let frames = (RATE as u64 * interval_us as u64 / 1_000_000) as usize;
    let bytes = frames * 4;
    if frames == 0
        || bytes
            > (choice.max_packet as usize & 0x7ff) * (1 + (choice.max_packet as usize >> 11 & 3))
    {
        crate::log!("uac: packet too small for 48 kHz stereo ({} bytes)", bytes);
        return None;
    }
    if !device.control_out(0x01, 11, choice.alternate as u16, choice.interface as u16) {
        crate::log!("uac: SET_INTERFACE failed");
        return None;
    }
    let config = EndpointConfig {
        address: choice.endpoint,
        transfer: 1,
        max_packet: choice.max_packet,
        interval: choice.interval,
    };
    if !host::configure_endpoints(device.controller, device.slot, device.speed, &[config]) {
        crate::log!("uac: configure endpoint failed");
        return None;
    }
    set_rate(device, control, &choice, uac2);
    let count = (QUEUE_MS * 1000 / interval_us as usize).max(4);
    let fill = Arc::new(|buf: &mut [u8]| crate::drivers::audio::fill_usb(buf));
    if !host::start_iso_out(
        device.controller,
        device.slot,
        choice.endpoint,
        bytes,
        count,
        fill,
    ) {
        crate::log!("uac: isochronous streaming needs an xHCI controller");
        return None;
    }
    *ACTIVE.lock() = Some((device.controller, device.slot));
    crate::drivers::audio::set_usb_output(true);
    crate::log!(
        "uac: '{}' UAC{} output, {} bytes every {} us",
        device.name,
        if uac2 { 2 } else { 1 },
        bytes,
        interval_us
    );
    Some("usb-audio")
}

pub fn detach(device: &UsbDevice) {
    let mut active = ACTIVE.lock();
    if *active == Some((device.controller, device.slot)) {
        *active = None;
        crate::drivers::audio::set_usb_output(false);
    }
}
