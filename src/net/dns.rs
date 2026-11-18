//! DNS çözücü (A kayıtları): sıkıştırılmış adlar, yeniden deneme ve TTL'li önbellek.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::wire::{self, Ipv4};
use super::{config, now_ms, random_u32, udp_bind, udp_receive, udp_send, udp_unbind};
use crate::sync::SpinLock;

const PORT: u16 = 53;
const ATTEMPTS: usize = 3;
const TIMEOUT_MS: u64 = 2000;

static CACHE: SpinLock<BTreeMap<String, (Vec<Ipv4>, u64)>> = SpinLock::new(BTreeMap::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    NotConfigured,
    InvalidName,
    Timeout,
    NotFound,
    ServerFailure,
}

impl DnsError {
    pub fn message(self) -> &'static str {
        match self {
            DnsError::NotConfigured => "no DNS server (network not configured)",
            DnsError::InvalidName => "invalid host name",
            DnsError::Timeout => "DNS server did not answer",
            DnsError::NotFound => "host not found",
            DnsError::ServerFailure => "DNS server failure",
        }
    }
}

fn query(id: u16, name: &str) -> Option<Vec<u8>> {
    let mut q = Vec::with_capacity(32 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]);
    Some(q)
}

/// Sıkıştırma işaretçilerini izleyerek adı atlar; adın bittiği konumu döndürür.
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    for _ in 0..128 {
        let len = *msg.get(at)?;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(at + 2);
        }
        at += 1 + len as usize;
    }
    None
}

fn parse(msg: &[u8], id: u16) -> Result<(Vec<Ipv4>, u32), DnsError> {
    if msg.len() < 12 || wire::be16(msg, 0) != Some(id) || msg[2] & 0x80 == 0 {
        return Err(DnsError::ServerFailure);
    }
    match msg[3] & 0x0f {
        0 => {}
        3 => return Err(DnsError::NotFound),
        _ => return Err(DnsError::ServerFailure),
    }
    let questions = wire::be16(msg, 4).unwrap_or(0);
    let answers = wire::be16(msg, 6).unwrap_or(0);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(msg, at).ok_or(DnsError::ServerFailure)? + 4;
    }
    let mut out = Vec::new();
    let mut ttl = u32::MAX;
    for _ in 0..answers {
        at = skip_name(msg, at).ok_or(DnsError::ServerFailure)?;
        let kind = wire::be16(msg, at).ok_or(DnsError::ServerFailure)?;
        let class = wire::be16(msg, at + 2).ok_or(DnsError::ServerFailure)?;
        let record_ttl = wire::be32(msg, at + 4).ok_or(DnsError::ServerFailure)?;
        let len = wire::be16(msg, at + 8).ok_or(DnsError::ServerFailure)? as usize;
        let data = msg
            .get(at + 10..at + 10 + len)
            .ok_or(DnsError::ServerFailure)?;
        if kind == 1 && class == 1 && len == 4 {
            out.push(Ipv4(data.try_into().unwrap()));
            ttl = ttl.min(record_ttl);
        }
        at += 10 + len;
    }
    if out.is_empty() {
        Err(DnsError::NotFound)
    } else {
        Ok((out, ttl))
    }
}

pub fn resolve(name: &str) -> Result<Vec<Ipv4>, DnsError> {
    if let Some(ip) = Ipv4::parse(name) {
        return Ok(alloc::vec![ip]);
    }
    let key = name.trim_end_matches('.').to_ascii_lowercase();
    if key.is_empty() || key.len() > 253 {
        return Err(DnsError::InvalidName);
    }
    if let Some((addresses, expires)) = CACHE.lock().get(&key)
        && *expires > now_ms()
    {
        return Ok(addresses.clone());
    }
    let servers = config().map(|c| c.dns).unwrap_or_default();
    if servers.is_empty() {
        return Err(DnsError::NotConfigured);
    }
    let port = udp_bind().ok_or(DnsError::ServerFailure)?;
    let mut last = DnsError::Timeout;
    for attempt in 0..ATTEMPTS {
        let server = servers[attempt % servers.len()];
        let id = random_u32() as u16;
        let packet = query(id, &key).ok_or(DnsError::InvalidName)?;
        udp_send(server, port, PORT, &packet);
        let deadline = now_ms() + TIMEOUT_MS;
        while now_ms() < deadline {
            let Some(reply) = udp_receive(port, deadline - now_ms()) else {
                break;
            };
            if reply.source_port != PORT || reply.source != server {
                continue;
            }
            match parse(&reply.data, id) {
                Ok((addresses, ttl)) => {
                    udp_unbind(port);
                    let expires = now_ms() + (ttl.clamp(5, 3600) as u64) * 1000;
                    CACHE.lock().insert(key, (addresses.clone(), expires));
                    return Ok(addresses);
                }
                Err(DnsError::ServerFailure) if wire::be16(&reply.data, 0) != Some(id) => continue,
                Err(e) => {
                    last = e;
                    break;
                }
            }
        }
        if last == DnsError::NotFound {
            break;
        }
    }
    udp_unbind(port);
    Err(last)
}

pub fn self_test() -> bool {
    let q = query(0x1234, "example.com").unwrap_or_default();
    let mut answer = q.clone();
    answer[2] = 0x81;
    answer[3] = 0x80;
    answer[7] = 1;
    answer.extend_from_slice(&[
        0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0x0e, 0x10, 0, 4, 93, 184, 216, 34,
    ]);
    parse(&answer, 0x1234) == Ok((alloc::vec![Ipv4([93, 184, 216, 34])], 3600))
        && q.len() == 12 + 13 + 4
        && query(1, "bad..name").is_none()
}
