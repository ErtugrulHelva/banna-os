//! SNTP istemcisi (RFC 4330): ağ yapılandırılınca saati eşitler, sonra 6 saatte bir yeniler.
//! HTTPS sertifika doğrulaması doğru saate bağlıdır.

use super::{Ipv4, dns};
use crate::sched;

const SERVERS: [&str; 3] = ["pool.ntp.org", "time.cloudflare.com", "time.google.com"];
/// 1900-01-01 ile 1970-01-01 arası saniye.
const NTP_EPOCH: i64 = 2_208_988_800;

fn query(server: Ipv4) -> Option<i64> {
    let port = super::udp_bind()?;
    let mut request = [0u8; 48];
    request[0] = 0x23; // LI 0, sürüm 4, istemci kipi
    let sent = super::udp_send(server, port, 123, &request);
    let reply = if sent {
        super::udp_receive(port, 3000)
    } else {
        None
    };
    super::udp_unbind(port);
    let data = reply?.data;
    if data.len() < 48 || data[0] & 0x07 != 4 || data[1] == 0 {
        return None;
    }
    let seconds = u32::from_be_bytes([data[40], data[41], data[42], data[43]]) as i64;
    let fraction = u32::from_be_bytes([data[44], data[45], data[46], data[47]]) as i64;
    // En yakın saniyeye yuvarla.
    Some(seconds - NTP_EPOCH + (fraction >> 31))
}

fn sync() -> bool {
    for name in SERVERS {
        let Ok(addresses) = dns::resolve(name) else {
            continue;
        };
        for address in addresses.into_iter().take(2) {
            if let Some(unix) = query(address)
                && let Some(correction) = crate::drivers::rtc::set_unix_time(unix)
            {
                crate::log!(
                    "ntp: clock synchronized with {} ({}), RTC off by {} s",
                    name,
                    address,
                    correction
                );
                return true;
            }
        }
    }
    false
}

fn ntpd() -> i64 {
    loop {
        if super::wait_configured(60_000) {
            let next = if sync() { 6 * 3600 * 1000 } else { 60_000 };
            sched::sleep_ms(next);
        }
    }
}

pub fn start() {
    sched::spawn_detached("ntpd", ntpd);
}
