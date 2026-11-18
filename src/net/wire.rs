//! Ethernet, ARP, IPv4, ICMP ve UDP başlıklarının ayrıştırılması ve oluşturulması.

use alloc::vec::Vec;
use core::fmt;

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;
pub const BROADCAST_MAC: [u8; 6] = [0xff; 6];

#[derive(Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct Ipv4(pub [u8; 4]);

impl Ipv4 {
    pub const UNSPECIFIED: Self = Self([0, 0, 0, 0]);
    pub const BROADCAST: Self = Self([255, 255, 255, 255]);

    pub fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    pub fn is_unspecified(self) -> bool {
        self == Self::UNSPECIFIED
    }

    pub fn same_subnet(self, other: Self, mask: Self) -> bool {
        self.to_u32() & mask.to_u32() == other.to_u32() & mask.to_u32()
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut out = [0u8; 4];
        let mut parts = text.split('.');
        for byte in out.iter_mut() {
            *byte = parts.next()?.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(out))
    }

    pub fn prefix_len(self) -> u32 {
        self.to_u32().count_ones()
    }
}

impl fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl fmt::Debug for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub struct Mac<'a>(pub &'a [u8; 6]);

impl fmt::Display for Mac<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let m = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

pub fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

pub fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// İnternet sağlama toplamı (RFC 1071).
pub fn checksum(chunks: &[&[u8]]) -> u16 {
    let mut sum = 0u64;
    let mut odd: Option<u8> = None;
    for chunk in chunks {
        let mut data: &[u8] = chunk;
        if let Some(high) = odd
            && let Some((&low, rest)) = data.split_first()
        {
            sum += u16::from_be_bytes([high, low]) as u64;
            data = rest;
            odd = None;
        }
        let (words, rest) = data.as_chunks::<8>();
        for w in words {
            let v = u64::from_be_bytes(*w);
            sum += (v >> 48) + ((v >> 32) & 0xffff) + ((v >> 16) & 0xffff) + (v & 0xffff);
        }
        let (pairs, last) = rest.as_chunks::<2>();
        for p in pairs {
            sum += u16::from_be_bytes(*p) as u64;
        }
        if let [last] = last {
            odd = Some(*last);
        }
    }
    if let Some(high) = odd {
        sum += (high as u64) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

pub struct EthernetFrame<'a> {
    pub destination: [u8; 6],
    pub ethertype: u16,
    pub payload: &'a [u8],
}

pub fn parse_ethernet(frame: &[u8]) -> Option<EthernetFrame<'_>> {
    if frame.len() < 14 {
        return None;
    }
    Some(EthernetFrame {
        destination: frame[0..6].try_into().ok()?,
        ethertype: be16(frame, 12)?,
        payload: &frame[14..],
    })
}

pub fn ethernet(destination: [u8; 6], source: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len().max(46));
    frame.extend_from_slice(&destination);
    frame.extend_from_slice(&source);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    while frame.len() < 60 {
        frame.push(0);
    }
    frame
}

pub struct Arp {
    pub operation: u16,
    pub sender_mac: [u8; 6],
    pub sender_ip: Ipv4,
    pub target_ip: Ipv4,
}

pub fn parse_arp(p: &[u8]) -> Option<Arp> {
    if p.len() < 28 || be16(p, 0)? != 1 || be16(p, 2)? != ETHERTYPE_IPV4 || p[4] != 6 || p[5] != 4 {
        return None;
    }
    Some(Arp {
        operation: be16(p, 6)?,
        sender_mac: p[8..14].try_into().ok()?,
        sender_ip: Ipv4(p[14..18].try_into().ok()?),
        target_ip: Ipv4(p[24..28].try_into().ok()?),
    })
}

pub fn arp(
    operation: u16,
    sender_mac: [u8; 6],
    sender_ip: Ipv4,
    target_mac: [u8; 6],
    target_ip: Ipv4,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(28);
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    p.push(6);
    p.push(4);
    p.extend_from_slice(&operation.to_be_bytes());
    p.extend_from_slice(&sender_mac);
    p.extend_from_slice(&sender_ip.0);
    p.extend_from_slice(&target_mac);
    p.extend_from_slice(&target_ip.0);
    p
}

pub struct Ipv4Packet<'a> {
    pub source: Ipv4,
    pub destination: Ipv4,
    pub protocol: u8,
    pub payload: &'a [u8],
}

pub fn parse_ipv4(p: &[u8]) -> Option<Ipv4Packet<'_>> {
    if p.len() < 20 || p[0] >> 4 != 4 {
        return None;
    }
    let header = ((p[0] & 0x0f) as usize) * 4;
    let total = be16(p, 2)? as usize;
    if header < 20 || total < header || total > p.len() || checksum(&[&p[..header]]) != 0 {
        return None;
    }
    let fragment = be16(p, 6)?;
    if fragment & 0x3fff != 0 {
        return None;
    }
    Some(Ipv4Packet {
        source: Ipv4(p[12..16].try_into().ok()?),
        destination: Ipv4(p[16..20].try_into().ok()?),
        protocol: p[9],
        payload: &p[header..total],
    })
}

pub fn ipv4(source: Ipv4, destination: Ipv4, protocol: u8, id: u16, payload: &[u8]) -> Vec<u8> {
    let total = 20 + payload.len();
    let mut p = Vec::with_capacity(total);
    p.extend_from_slice(&[0x45, 0]);
    p.extend_from_slice(&(total as u16).to_be_bytes());
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0x4000u16.to_be_bytes());
    p.push(64);
    p.push(protocol);
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(&source.0);
    p.extend_from_slice(&destination.0);
    let sum = checksum(&[&p[..20]]);
    p[10..12].copy_from_slice(&sum.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

pub struct UdpDatagram<'a> {
    pub source_port: u16,
    pub destination_port: u16,
    pub payload: &'a [u8],
}

pub fn parse_udp(p: &[u8]) -> Option<UdpDatagram<'_>> {
    if p.len() < 8 {
        return None;
    }
    let length = be16(p, 4)? as usize;
    if length < 8 || length > p.len() {
        return None;
    }
    Some(UdpDatagram {
        source_port: be16(p, 0)?,
        destination_port: be16(p, 2)?,
        payload: &p[8..length],
    })
}

pub fn udp(
    source: Ipv4,
    destination: Ipv4,
    source_port: u16,
    destination_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let length = (8 + payload.len()) as u16;
    let mut p = Vec::with_capacity(length as usize);
    p.extend_from_slice(&source_port.to_be_bytes());
    p.extend_from_slice(&destination_port.to_be_bytes());
    p.extend_from_slice(&length.to_be_bytes());
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(payload);
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&source.0);
    pseudo[4..8].copy_from_slice(&destination.0);
    pseudo[9] = PROTO_UDP;
    pseudo[10..12].copy_from_slice(&length.to_be_bytes());
    let sum = match checksum(&[&pseudo, &p]) {
        0 => 0xffff,
        s => s,
    };
    p[6..8].copy_from_slice(&sum.to_be_bytes());
    p
}

pub fn icmp_echo(kind: u8, id: u16, sequence: u16, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + data.len());
    p.extend_from_slice(&[kind, 0, 0, 0]);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&sequence.to_be_bytes());
    p.extend_from_slice(data);
    let sum = checksum(&[&p]);
    p[2..4].copy_from_slice(&sum.to_be_bytes());
    p
}

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpHeader {
    pub source_port: u16,
    pub destination_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub mss: Option<u16>,
    /// Pencere ölçekleme kaydırması (RFC 7323, yalnızca SYN'lerde).
    pub wscale: Option<u8>,
}

fn pseudo_header(source: Ipv4, destination: Ipv4, protocol: u8, length: usize) -> [u8; 12] {
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&source.0);
    pseudo[4..8].copy_from_slice(&destination.0);
    pseudo[9] = protocol;
    pseudo[10..12].copy_from_slice(&(length as u16).to_be_bytes());
    pseudo
}

/// TCP segmentini çözer; sağlama toplamı (sahte başlık dahil) tutmazsa `None`.
pub fn parse_tcp(source: Ipv4, destination: Ipv4, p: &[u8]) -> Option<(TcpHeader, &[u8])> {
    if p.len() < 20 {
        return None;
    }
    let offset = ((p[12] >> 4) as usize) * 4;
    if offset < 20 || offset > p.len() {
        return None;
    }
    let pseudo = pseudo_header(source, destination, PROTO_TCP, p.len());
    if checksum(&[&pseudo, p]) != 0 {
        return None;
    }
    let mut mss = None;
    let mut wscale = None;
    let mut i = 20;
    while i < offset {
        match p[i] {
            0 => break,
            1 => i += 1,
            kind => {
                let len = *p.get(i + 1)? as usize;
                if len < 2 || i + len > offset {
                    break;
                }
                if kind == 2 && len == 4 {
                    mss = be16(p, i + 2);
                }
                if kind == 3 && len == 3 {
                    wscale = p.get(i + 2).copied();
                }
                i += len;
            }
        }
    }
    Some((
        TcpHeader {
            source_port: be16(p, 0)?,
            destination_port: be16(p, 2)?,
            seq: be32(p, 4)?,
            ack: be32(p, 8)?,
            flags: p[13] & 0x3f,
            window: be16(p, 14)?,
            mss,
            wscale,
        },
        &p[offset..],
    ))
}

pub fn tcp(source: Ipv4, destination: Ipv4, header: &TcpHeader, payload: &[u8]) -> Vec<u8> {
    let options =
        if header.mss.is_some() { 4 } else { 0 } + if header.wscale.is_some() { 4 } else { 0 };
    let offset = 20 + options;
    let mut p = Vec::with_capacity(offset + payload.len());
    p.extend_from_slice(&header.source_port.to_be_bytes());
    p.extend_from_slice(&header.destination_port.to_be_bytes());
    p.extend_from_slice(&header.seq.to_be_bytes());
    p.extend_from_slice(&header.ack.to_be_bytes());
    p.push(((offset / 4) as u8) << 4);
    p.push(header.flags);
    p.extend_from_slice(&header.window.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0]);
    if let Some(mss) = header.mss {
        p.extend_from_slice(&[2, 4]);
        p.extend_from_slice(&mss.to_be_bytes());
    }
    if let Some(shift) = header.wscale {
        // NOP + pencere ölçekleme (tür 3, uzunluk 3): 4 bayta hizalı.
        p.extend_from_slice(&[1, 3, 3, shift]);
    }
    p.extend_from_slice(payload);
    let pseudo = pseudo_header(source, destination, PROTO_TCP, p.len());
    let sum = checksum(&[&pseudo, &p]);
    p[16..18].copy_from_slice(&sum.to_be_bytes());
    p
}
