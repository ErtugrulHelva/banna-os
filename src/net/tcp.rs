//! TCP (RFC 9293): üçlü el sıkışma, kayan pencere, sırasız alım, RFC 6298 yeniden
//! iletim zamanlayıcısı, hızlı yeniden iletim ve NewReno tıkanıklık denetimi
//! (RFC 5681/6582), sıfır pencere yoklaması, TIME-WAIT.
//!
//! Bu modül saf durum makinesidir: zamanı parametre olarak alır, gönderilecek
//! segmentleri `output` kuyruğuna koyar. IP katmanı ve kilitler `net` modülündedir.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use super::wire::{Ipv4, TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN, TcpHeader};

pub type SocketId = u32;

const OUR_MSS: u16 = 1460;
const DEFAULT_MSS: u16 = 536;
const SEND_BUFFER: usize = 256 * 1024;
/// Alma tamponu: pencere ölçeklemeyle 1 MB (64 KB'lık pencere, 90 ms uzaklıktaki bir
/// sunucudan bağlantı başına en fazla ~0,7 MB/sn indirmeye izin veriyordu).
const RECEIVE_BUFFER: usize = 1024 * 1024;
/// Bizim pencere ölçekleme kaydırmamız: 65535 << 5 ≈ 2 MB ≥ RECEIVE_BUFFER.
const OUR_WSCALE: u8 = 5;
const OUT_OF_ORDER_LIMIT: usize = 1024 * 1024;
const INITIAL_RTO_MS: u64 = 1000;
const MIN_RTO_MS: u64 = 200;
const MAX_RTO_MS: u64 = 60_000;
const MAX_RETRIES: u32 = 10;
const MAX_SYN_RETRIES: u32 = 5;
const TIME_WAIT_MS: u64 = 10_000;
const BACKLOG: usize = 32;
const EPHEMERAL_FIRST: u16 = 49152;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Closed => "CLOSED",
            State::Listen => "LISTEN",
            State::SynSent => "SYN-SENT",
            State::SynReceived => "SYN-RECEIVED",
            State::Established => "ESTABLISHED",
            State::FinWait1 => "FIN-WAIT-1",
            State::FinWait2 => "FIN-WAIT-2",
            State::CloseWait => "CLOSE-WAIT",
            State::Closing => "CLOSING",
            State::LastAck => "LAST-ACK",
            State::TimeWait => "TIME-WAIT",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    Refused,
    Reset,
    TimedOut,
    Closed,
    NoSocket,
    AddressInUse,
    WouldBlock,
}

fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn le(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) <= 0
}

pub struct Outgoing {
    pub destination: Ipv4,
    pub header: TcpHeader,
    pub payload: Vec<u8>,
}

struct Tcb {
    state: State,
    local_port: u16,
    remote: Ipv4,
    remote_port: u16,
    parent: Option<SocketId>,
    user_closed: bool,
    error: Option<Error>,

    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    /// Şimdiye kadar gönderilen en yüksek sıra; go-back-N sonrası ACK doğrulaması için.
    snd_max: u32,
    snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    /// `snd_una`'dan başlayan veri: onaylanmamış + henüz gönderilmemiş.
    send_buffer: VecDeque<u8>,
    fin_queued: bool,
    fin_sent: bool,
    fin_seq: u32,
    mss: u32,

    rcv_nxt: u32,
    receive_buffer: VecDeque<u8>,
    out_of_order: BTreeMap<u32, Vec<u8>>,
    out_of_order_bytes: usize,
    fin_received: bool,
    ack_pending: bool,
    advertised_window: u32,
    /// Pencere ölçekleme (iki taraf da SYN'de önerdiyse): karşının pencere alanı
    /// `snd_shift` kadar, bizim duyurduğumuz `rcv_shift` kadar kaydırılır.
    snd_shift: u8,
    rcv_shift: u8,

    rto_ms: u64,
    srtt_ms: Option<u64>,
    rttvar_ms: u64,
    rtt_probe: Option<(u32, u64)>,
    retransmit_at: Option<u64>,
    retries: u32,
    persist_at: Option<u64>,
    time_wait_until: u64,

    cwnd: u32,
    ssthresh: u32,
    duplicate_acks: u32,
    recover: u32,
    in_recovery: bool,

    pub segments_out: u64,
    pub segments_in: u64,
    pub retransmits: u64,
}

impl Tcb {
    fn new(local_port: u16, remote: Ipv4, remote_port: u16, iss: u32, state: State) -> Self {
        Self {
            state,
            local_port,
            remote,
            remote_port,
            parent: None,
            user_closed: false,
            error: None,
            iss,
            snd_una: iss,
            snd_nxt: iss,
            snd_max: iss,
            snd_wnd: DEFAULT_MSS as u32,
            snd_wl1: 0,
            snd_wl2: 0,
            send_buffer: VecDeque::new(),
            fin_queued: false,
            fin_sent: false,
            fin_seq: 0,
            mss: DEFAULT_MSS as u32,
            rcv_nxt: 0,
            receive_buffer: VecDeque::new(),
            out_of_order: BTreeMap::new(),
            out_of_order_bytes: 0,
            fin_received: false,
            ack_pending: false,
            advertised_window: u16::MAX as u32,
            snd_shift: 0,
            rcv_shift: 0,
            rto_ms: INITIAL_RTO_MS,
            srtt_ms: None,
            rttvar_ms: 0,
            rtt_probe: None,
            retransmit_at: None,
            retries: 0,
            persist_at: None,
            time_wait_until: 0,
            cwnd: 0,
            ssthresh: u32::MAX,
            duplicate_acks: 0,
            recover: iss,
            in_recovery: false,
            segments_out: 0,
            segments_in: 0,
            retransmits: 0,
        }
    }

    fn receive_window(&self) -> u32 {
        let free = (RECEIVE_BUFFER - self.receive_buffer.len()) as u32;
        // Ölçekleme yoksa pencere alanı 16 bit.
        free.min((u16::MAX as u32) << self.rcv_shift)
    }

    /// SYN'deki ölçekleme önerisini kaydeder (karşı önermediyse ölçekleme kapalı kalır).
    fn negotiate_scale(&mut self, offered: Option<u8>) {
        if let Some(shift) = offered {
            self.snd_shift = shift.min(14);
            self.rcv_shift = OUR_WSCALE;
        }
    }

    fn synchronized(&self) -> bool {
        !matches!(
            self.state,
            State::Closed | State::Listen | State::SynSent | State::SynReceived
        )
    }

    /// Gönderilmiş ama onaylanmamış bayt sayısı (SYN/FIN dahil).
    fn in_flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.snd_una)
    }

    /// `snd_nxt`'e karşılık gelen gönderim tamponu konumu.
    fn sent_data(&self) -> usize {
        let mut flight = self.in_flight() as usize;
        if matches!(self.state, State::SynSent | State::SynReceived) {
            flight = flight.saturating_sub(1);
        }
        flight.min(self.send_buffer.len())
    }

    /// FIN'in (yeniden) gönderilmesi gerekiyor mu: tüm veri yolda ve `snd_nxt` FIN'i geçmemiş.
    fn fin_due(&self) -> bool {
        self.fin_queued
            && self.sent_data() == self.send_buffer.len()
            && self.snd_nxt == self.snd_una.wrapping_add(self.send_buffer.len() as u32)
            && !self.fin_acked()
    }

    /// Onay bekleyen en yüksek sıraya kadar olan bayt sayısı (go-back-N sırasında
    /// `in_flight`'tan büyük olabilir).
    fn outstanding(&self) -> u32 {
        self.snd_max.wrapping_sub(self.snd_una)
    }

    fn advance_nxt(&mut self, by: u32) {
        self.snd_nxt = self.snd_nxt.wrapping_add(by);
        if lt(self.snd_max, self.snd_nxt) {
            self.snd_max = self.snd_nxt;
        }
    }

    fn fin_acked(&self) -> bool {
        self.fin_sent && le(self.fin_seq.wrapping_add(1), self.snd_una)
    }

    fn update_rtt(&mut self, sample: u64) {
        match self.srtt_ms {
            None => {
                self.srtt_ms = Some(sample);
                self.rttvar_ms = sample / 2;
            }
            Some(srtt) => {
                let delta = srtt.abs_diff(sample);
                self.rttvar_ms = (3 * self.rttvar_ms + delta) / 4;
                self.srtt_ms = Some((7 * srtt + sample) / 8);
            }
        }
        let srtt = self.srtt_ms.unwrap_or(sample);
        self.rto_ms = (srtt + (4 * self.rttvar_ms).max(10)).clamp(MIN_RTO_MS, MAX_RTO_MS);
    }

    fn set_error(&mut self, error: Error) {
        self.error = Some(error);
        self.state = State::Closed;
        self.retransmit_at = None;
        self.persist_at = None;
    }
}

struct Listener {
    port: u16,
    backlog: VecDeque<SocketId>,
}

#[derive(Clone, Debug)]
pub struct SocketInfo {
    pub id: SocketId,
    pub state: State,
    pub local_port: u16,
    pub remote: Ipv4,
    pub remote_port: u16,
    pub send_queue: usize,
    pub receive_queue: usize,
    pub rto_ms: u64,
    pub srtt_ms: Option<u64>,
    pub cwnd: u32,
    pub retransmits: u64,
}

pub struct Tcp {
    sockets: BTreeMap<SocketId, Tcb>,
    listeners: BTreeMap<SocketId, Listener>,
    next_id: SocketId,
    next_port: u16,
    pub output: Vec<Outgoing>,
    pub resets_sent: u64,
}

pub enum Received {
    Data(Vec<u8>),
    End,
}

impl Tcp {
    pub fn new(seed: u32) -> Self {
        Self {
            sockets: BTreeMap::new(),
            listeners: BTreeMap::new(),
            next_id: 1,
            next_port: EPHEMERAL_FIRST + (seed % 8000) as u16,
            output: Vec::new(),
            resets_sent: 0,
        }
    }

    fn allocate_id(&mut self) -> SocketId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn port_in_use(&self, port: u16) -> bool {
        self.listeners.values().any(|l| l.port == port)
            || self
                .sockets
                .values()
                .any(|t| t.local_port == port && t.parent.is_none())
    }

    fn allocate_port(&mut self) -> Option<u16> {
        for _ in 0..16384 {
            self.next_port = if self.next_port < EPHEMERAL_FIRST || self.next_port == u16::MAX {
                EPHEMERAL_FIRST
            } else {
                self.next_port + 1
            };
            if !self.port_in_use(self.next_port) {
                return Some(self.next_port);
            }
        }
        None
    }

    // ---- kullanıcı arayüzü ----

    pub fn connect(
        &mut self,
        now: u64,
        remote: Ipv4,
        port: u16,
        iss: u32,
    ) -> Result<SocketId, Error> {
        let local = self.allocate_port().ok_or(Error::AddressInUse)?;
        let id = self.allocate_id();
        let mut tcb = Tcb::new(local, remote, port, iss, State::SynSent);
        tcb.advance_nxt(1);
        tcb.retransmit_at = Some(now + tcb.rto_ms);
        tcb.rtt_probe = Some((tcb.snd_nxt, now));
        self.emit(&mut tcb, iss, TCP_SYN, Some(OUR_MSS), &[]);
        self.sockets.insert(id, tcb);
        Ok(id)
    }

    pub fn listen(&mut self, port: u16) -> Result<SocketId, Error> {
        if port == 0 || self.listeners.values().any(|l| l.port == port) {
            return Err(Error::AddressInUse);
        }
        let id = self.allocate_id();
        self.listeners.insert(
            id,
            Listener {
                port,
                backlog: VecDeque::new(),
            },
        );
        Ok(id)
    }

    pub fn accept(&mut self, listener: SocketId) -> Result<SocketId, Error> {
        let l = self.listeners.get_mut(&listener).ok_or(Error::NoSocket)?;
        let id = l.backlog.pop_front().ok_or(Error::WouldBlock)?;
        if let Some(t) = self.sockets.get_mut(&id) {
            t.parent = None;
        }
        Ok(id)
    }

    pub fn has_pending(&self, listener: SocketId) -> bool {
        self.listeners
            .get(&listener)
            .is_some_and(|l| !l.backlog.is_empty())
    }

    pub fn state(&self, id: SocketId) -> Option<State> {
        if self.listeners.contains_key(&id) {
            return Some(State::Listen);
        }
        self.sockets.get(&id).map(|t| t.state)
    }

    pub fn error(&self, id: SocketId) -> Option<Error> {
        self.sockets.get(&id).and_then(|t| t.error)
    }

    /// Veriyi gönderim tamponuna ekler; eklenebilen bayt sayısını döndürür.
    pub fn send(&mut self, now: u64, id: SocketId, data: &[u8]) -> Result<usize, Error> {
        let tcb = self.sockets.get_mut(&id).ok_or(Error::NoSocket)?;
        if let Some(e) = tcb.error {
            return Err(e);
        }
        match tcb.state {
            State::Established | State::CloseWait => {}
            State::SynSent | State::SynReceived => return Err(Error::WouldBlock),
            _ => return Err(Error::Closed),
        }
        if tcb.fin_queued {
            return Err(Error::Closed);
        }
        let room = SEND_BUFFER - tcb.send_buffer.len();
        if room == 0 {
            return Err(Error::WouldBlock);
        }
        let n = room.min(data.len());
        tcb.send_buffer.extend(&data[..n]);
        self.transmit(now, id);
        Ok(n)
    }

    pub fn can_send(&self, id: SocketId) -> bool {
        self.sockets.get(&id).is_none_or(|t| {
            t.error.is_some()
                || !matches!(t.state, State::SynSent | State::SynReceived)
                    && t.send_buffer.len() < SEND_BUFFER
        })
    }

    /// Okunabilecek veri, bağlantı sonu ya da hata varsa `true`.
    pub fn readable(&self, id: SocketId) -> bool {
        self.sockets.get(&id).is_none_or(|t| {
            !t.receive_buffer.is_empty()
                || t.fin_received
                || t.error.is_some()
                || t.state == State::Closed
        })
    }

    pub fn receive(&mut self, now: u64, id: SocketId, max: usize) -> Result<Received, Error> {
        let tcb = self.sockets.get_mut(&id).ok_or(Error::NoSocket)?;
        if !tcb.receive_buffer.is_empty() {
            let n = max.min(tcb.receive_buffer.len());
            let data: Vec<u8> = tcb.receive_buffer.drain(..n).collect();
            // Pencere anlamlı ölçüde açıldıysa karşıya bildir (RFC 1122 SWS önlemi).
            let window = tcb.receive_window();
            if window.saturating_sub(tcb.advertised_window)
                >= (RECEIVE_BUFFER as u32 / 2).min(tcb.mss * 2)
                || (tcb.advertised_window < tcb.mss && window >= tcb.mss)
            {
                tcb.ack_pending = true;
                self.transmit(now, id);
            }
            return Ok(Received::Data(data));
        }
        if let Some(e) = tcb.error {
            return Err(e);
        }
        if tcb.fin_received {
            return Ok(Received::End);
        }
        match tcb.state {
            State::Closed => Err(Error::Closed),
            _ => Err(Error::WouldBlock),
        }
    }

    /// Düzgün kapanış: kalan veri gönderildikten sonra FIN yollanır. Soket,
    /// bağlantı tamamen kapanınca (ya da TIME-WAIT bitince) kendiliğinden silinir.
    pub fn close(&mut self, now: u64, id: SocketId) {
        if let Some(l) = self.listeners.remove(&id) {
            for child in l.backlog {
                self.abort(child);
            }
            let orphans: Vec<SocketId> = self
                .sockets
                .iter()
                .filter(|(_, t)| t.parent == Some(id))
                .map(|(&c, _)| c)
                .collect();
            for child in orphans {
                self.abort(child);
            }
            return;
        }
        let Some(tcb) = self.sockets.get_mut(&id) else {
            return;
        };
        tcb.user_closed = true;
        match tcb.state {
            State::Closed | State::SynSent => {
                self.sockets.remove(&id);
            }
            State::SynReceived | State::Established | State::CloseWait => {
                if !tcb.receive_buffer.is_empty() {
                    // Okunmamış veriyle kapatma: RFC 2525'e göre RST.
                    self.abort(id);
                    return;
                }
                tcb.fin_queued = true;
                self.transmit(now, id);
            }
            _ => {}
        }
    }

    /// Bağlantıyı RST ile hemen düşürür.
    pub fn abort(&mut self, id: SocketId) {
        let Some(mut tcb) = self.sockets.remove(&id) else {
            return;
        };
        if matches!(
            tcb.state,
            State::SynReceived
                | State::Established
                | State::FinWait1
                | State::FinWait2
                | State::CloseWait
        ) {
            let seq = tcb.snd_nxt;
            self.emit(&mut tcb, seq, TCP_RST | TCP_ACK, None, &[]);
            self.resets_sent += 1;
        }
    }

    pub fn sockets(&self) -> Vec<SocketInfo> {
        let mut list: Vec<SocketInfo> = self
            .listeners
            .iter()
            .map(|(&id, l)| SocketInfo {
                id,
                state: State::Listen,
                local_port: l.port,
                remote: Ipv4::UNSPECIFIED,
                remote_port: 0,
                send_queue: 0,
                receive_queue: l.backlog.len(),
                rto_ms: 0,
                srtt_ms: None,
                cwnd: 0,
                retransmits: 0,
            })
            .collect();
        list.extend(self.sockets.iter().map(|(&id, t)| SocketInfo {
            id,
            state: t.state,
            local_port: t.local_port,
            remote: t.remote,
            remote_port: t.remote_port,
            send_queue: t.send_buffer.len(),
            receive_queue: t.receive_buffer.len(),
            rto_ms: t.rto_ms,
            srtt_ms: t.srtt_ms,
            cwnd: t.cwnd,
            retransmits: t.retransmits,
        }));
        list
    }

    // ---- segment üretimi ----

    fn emit(&mut self, tcb: &mut Tcb, seq: u32, flags: u8, mss: Option<u16>, payload: &[u8]) {
        let syn = flags & TCP_SYN != 0;
        // SYN'lerdeki pencere ölçeklenmez (RFC 7323 §2.2).
        let shift = if syn { 0 } else { tcb.rcv_shift };
        let field = if flags & TCP_RST != 0 {
            0
        } else {
            (tcb.receive_window() >> shift).min(u16::MAX as u32)
        };
        let window = field << shift;
        // Bağlantı açarken her zaman önerilir; SYN+ACK'te yalnızca karşı önerdiyse.
        let wscale = match (syn, flags & TCP_ACK != 0) {
            (true, false) => Some(OUR_WSCALE),
            (true, true) if tcb.rcv_shift > 0 => Some(OUR_WSCALE),
            _ => None,
        };
        if flags & TCP_ACK != 0 {
            tcb.ack_pending = false;
            tcb.advertised_window = window;
        }
        tcb.segments_out += 1;
        self.output.push(Outgoing {
            destination: tcb.remote,
            header: TcpHeader {
                source_port: tcb.local_port,
                destination_port: tcb.remote_port,
                seq,
                ack: if flags & TCP_ACK != 0 { tcb.rcv_nxt } else { 0 },
                flags,
                window: field as u16,
                mss,
                wscale,
            },
            payload: payload.to_vec(),
        });
    }

    fn send_reset_for(&mut self, source: Ipv4, header: &TcpHeader, payload_len: usize) {
        if header.flags & TCP_RST != 0 {
            return;
        }
        let (seq, ack, flags) = if header.flags & TCP_ACK != 0 {
            (header.ack, 0, TCP_RST)
        } else {
            let mut len = payload_len as u32;
            if header.flags & TCP_SYN != 0 {
                len += 1;
            }
            if header.flags & TCP_FIN != 0 {
                len += 1;
            }
            (0, header.seq.wrapping_add(len), TCP_RST | TCP_ACK)
        };
        self.resets_sent += 1;
        self.output.push(Outgoing {
            destination: source,
            header: TcpHeader {
                source_port: header.destination_port,
                destination_port: header.source_port,
                seq,
                ack,
                flags,
                window: 0,
                mss: None,
                wscale: None,
            },
            payload: Vec::new(),
        });
    }

    /// Pencerenin ve tıkanıklık penceresinin izin verdiği kadar veri ve gerekirse
    /// FIN/ACK gönderir.
    fn transmit(&mut self, now: u64, id: SocketId) {
        let Some(mut tcb) = self.sockets.remove(&id) else {
            return;
        };
        self.transmit_tcb(now, &mut tcb);
        self.sockets.insert(id, tcb);
    }

    fn transmit_tcb(&mut self, now: u64, tcb: &mut Tcb) {
        let can_send_data = matches!(
            tcb.state,
            State::Established
                | State::CloseWait
                | State::FinWait1
                | State::LastAck
                | State::Closing
        );
        let mut sent_any = false;
        if can_send_data {
            loop {
                let offset = tcb.sent_data();
                let unsent = tcb.send_buffer.len() - offset;
                if unsent == 0 {
                    break;
                }
                let window = tcb.snd_wnd.min(tcb.cwnd.max(tcb.mss));
                let flight = tcb.in_flight();
                if flight >= window {
                    if tcb.snd_wnd == 0 && flight == 0 && tcb.persist_at.is_none() {
                        tcb.persist_at = Some(now + tcb.rto_ms);
                    }
                    break;
                }
                let len = (unsent as u32).min(tcb.mss).min(window - flight) as usize;
                // Silly window önlemi: küçük segmenti yalnızca kuyrukta başka veri yoksa yolla.
                if len < tcb.mss as usize && len < unsent && flight > 0 {
                    break;
                }
                let payload: Vec<u8> = tcb
                    .send_buffer
                    .range(offset..offset + len)
                    .copied()
                    .collect();
                let seq = tcb.snd_nxt;
                let flags = TCP_ACK | if len == unsent { TCP_PSH } else { 0 };
                self.emit(tcb, seq, flags, None, &payload);
                let fresh = !lt(seq, tcb.snd_max);
                tcb.advance_nxt(len as u32);
                if tcb.rtt_probe.is_none() && fresh {
                    tcb.rtt_probe = Some((tcb.snd_nxt, now));
                }
                if tcb.retransmit_at.is_none() {
                    tcb.retransmit_at = Some(now + tcb.rto_ms);
                }
                tcb.persist_at = None;
                sent_any = true;
            }
            if tcb.fin_due() {
                let seq = tcb.snd_nxt;
                self.emit(tcb, seq, TCP_FIN | TCP_ACK, None, &[]);
                tcb.fin_sent = true;
                tcb.fin_seq = seq;
                tcb.advance_nxt(1);
                if tcb.retransmit_at.is_none() {
                    tcb.retransmit_at = Some(now + tcb.rto_ms);
                }
                tcb.state = match tcb.state {
                    State::Established => State::FinWait1,
                    State::CloseWait => State::LastAck,
                    other => other,
                };
                sent_any = true;
            }
        }
        if tcb.ack_pending && !sent_any && tcb.state != State::Closed {
            let seq = tcb.snd_nxt;
            self.emit(tcb, seq, TCP_ACK, None, &[]);
        }
    }

    /// `snd_una`'daki ilk segmenti yeniden yollar (hızlı yeniden iletim / RTO).
    fn retransmit_first(&mut self, tcb: &mut Tcb) {
        tcb.retransmits += 1;
        tcb.rtt_probe = None;
        match tcb.state {
            State::SynSent => {
                let iss = tcb.iss;
                self.emit(tcb, iss, TCP_SYN, Some(OUR_MSS), &[]);
                return;
            }
            State::SynReceived => {
                let iss = tcb.iss;
                self.emit(tcb, iss, TCP_SYN | TCP_ACK, Some(OUR_MSS), &[]);
                return;
            }
            _ => {}
        }
        let len = tcb.send_buffer.len().min(tcb.mss as usize);
        if len > 0 {
            let payload: Vec<u8> = tcb.send_buffer.range(..len).copied().collect();
            let seq = tcb.snd_una;
            self.emit(tcb, seq, TCP_ACK | TCP_PSH, None, &payload);
        } else if tcb.fin_sent {
            let seq = tcb.snd_una;
            self.emit(tcb, seq, TCP_FIN | TCP_ACK, None, &[]);
        }
    }

    // ---- gelen segmentler ----

    pub fn input(&mut self, now: u64, source: Ipv4, header: &TcpHeader, payload: &[u8]) {
        let key = self.sockets.iter().find_map(|(&id, t)| {
            (t.local_port == header.destination_port
                && t.remote == source
                && t.remote_port == header.source_port
                && t.state != State::Closed)
                .then_some(id)
        });
        if let Some(id) = key {
            let mut tcb = self.sockets.remove(&id).expect("socket vanished");
            tcb.segments_in += 1;
            let keep = self.segment_arrives(now, id, &mut tcb, header, payload);
            if keep {
                self.sockets.insert(id, tcb);
            }
            return;
        }
        let listener = self
            .listeners
            .iter()
            .find_map(|(&id, l)| (l.port == header.destination_port).then_some(id));
        match listener {
            Some(lid) => self.listen_input(now, lid, source, header, payload),
            None => self.send_reset_for(source, header, payload.len()),
        }
    }

    fn listen_input(
        &mut self,
        now: u64,
        lid: SocketId,
        source: Ipv4,
        h: &TcpHeader,
        payload: &[u8],
    ) {
        if h.flags & TCP_RST != 0 {
            return;
        }
        if h.flags & TCP_ACK != 0 {
            self.send_reset_for(source, h, payload.len());
            return;
        }
        if h.flags & TCP_SYN == 0 {
            return;
        }
        let half_open = self
            .sockets
            .values()
            .filter(|t| t.parent == Some(lid))
            .count();
        if half_open >= BACKLOG {
            return;
        }
        let port = self.listeners[&lid].port;
        let iss = super::random_u32();
        let mut tcb = Tcb::new(port, source, h.source_port, iss, State::SynReceived);
        tcb.parent = Some(lid);
        tcb.rcv_nxt = h.seq.wrapping_add(1);
        tcb.advance_nxt(1);
        tcb.snd_wnd = h.window as u32;
        tcb.negotiate_scale(h.wscale);
        tcb.snd_wl1 = h.seq;
        tcb.mss = h.mss.unwrap_or(DEFAULT_MSS).clamp(64, OUR_MSS) as u32;
        tcb.cwnd = initial_cwnd(tcb.mss);
        tcb.retransmit_at = Some(now + tcb.rto_ms);
        self.emit(&mut tcb, iss, TCP_SYN | TCP_ACK, Some(OUR_MSS), &[]);
        let id = self.allocate_id();
        self.sockets.insert(id, tcb);
    }

    /// RFC 9293 §3.10.7. Soket tutulacaksa `true`.
    fn segment_arrives(
        &mut self,
        now: u64,
        id: SocketId,
        tcb: &mut Tcb,
        h: &TcpHeader,
        payload: &[u8],
    ) -> bool {
        if tcb.state == State::SynSent {
            return self.syn_sent_input(now, tcb, h);
        }

        // 1. Sıra numarası kabul edilebilir mi?
        let mut seg_len = payload.len() as u32;
        if h.flags & TCP_SYN != 0 {
            seg_len += 1;
        }
        if h.flags & TCP_FIN != 0 {
            seg_len += 1;
        }
        let window = tcb.receive_window();
        let in_window =
            |seq: u32| le(tcb.rcv_nxt, seq) && lt(seq, tcb.rcv_nxt.wrapping_add(window));
        let acceptable = match (seg_len, window) {
            (0, 0) => h.seq == tcb.rcv_nxt,
            (0, _) => in_window(h.seq),
            (_, 0) => false,
            (len, _) => in_window(h.seq) || in_window(h.seq.wrapping_add(len - 1)),
        };
        if !acceptable {
            if h.flags & TCP_RST == 0 {
                tcb.ack_pending = true;
                self.transmit_tcb(now, tcb);
            }
            return true;
        }

        // 2. RST
        if h.flags & TCP_RST != 0 {
            if h.seq != tcb.rcv_nxt {
                // RFC 5961 meydan okuma ACK'i.
                tcb.ack_pending = true;
                self.transmit_tcb(now, tcb);
                return true;
            }
            if tcb.state == State::SynReceived && tcb.parent.is_some() {
                return false;
            }
            tcb.set_error(Error::Reset);
            return !tcb.user_closed;
        }

        // 3. Senkronize durumda SYN: meydan okuma ACK'i.
        if h.flags & TCP_SYN != 0 {
            tcb.ack_pending = true;
            self.transmit_tcb(now, tcb);
            return true;
        }

        // 4. ACK
        if h.flags & TCP_ACK == 0 {
            return true;
        }
        if tcb.state == State::SynReceived {
            if lt(tcb.snd_una, h.ack) && le(h.ack, tcb.snd_max) {
                tcb.state = State::Established;
                tcb.snd_una = h.ack;
                tcb.snd_wnd = (h.window as u32) << tcb.snd_shift;
                tcb.snd_wl1 = h.seq;
                tcb.snd_wl2 = h.ack;
                tcb.retransmit_at = None;
                tcb.retries = 0;
                if let Some(parent) = tcb.parent
                    && let Some(l) = self.listeners.get_mut(&parent)
                {
                    if l.backlog.len() < BACKLOG {
                        l.backlog.push_back(id);
                    } else {
                        self.send_reset_for(tcb.remote, h, payload.len());
                        return false;
                    }
                }
            } else {
                self.send_reset_for(tcb.remote, h, payload.len());
                return true;
            }
        }
        if lt(tcb.snd_max, h.ack) {
            tcb.ack_pending = true;
            self.transmit_tcb(now, tcb);
            return true;
        }
        self.process_ack(now, tcb, h, payload.is_empty());

        match tcb.state {
            State::FinWait1 if tcb.fin_acked() => tcb.state = State::FinWait2,
            State::Closing if tcb.fin_acked() => {
                tcb.state = State::TimeWait;
                tcb.time_wait_until = now + TIME_WAIT_MS;
            }
            State::LastAck if tcb.fin_acked() => return false,
            _ => {}
        }

        // 5. Veri
        if !payload.is_empty()
            && matches!(
                tcb.state,
                State::Established | State::FinWait1 | State::FinWait2
            )
        {
            self.accept_data(tcb, h.seq, payload);
        }

        // 6. FIN (yalnızca sıradaysa)
        if h.flags & TCP_FIN != 0
            && h.seq.wrapping_add(payload.len() as u32) == tcb.rcv_nxt
            && !tcb.fin_received
        {
            tcb.fin_received = true;
            tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(1);
            tcb.ack_pending = true;
            match tcb.state {
                State::SynReceived | State::Established => tcb.state = State::CloseWait,
                State::FinWait1 => {
                    if tcb.fin_acked() {
                        tcb.state = State::TimeWait;
                        tcb.time_wait_until = now + TIME_WAIT_MS;
                    } else {
                        tcb.state = State::Closing;
                    }
                }
                State::FinWait2 => {
                    tcb.state = State::TimeWait;
                    tcb.time_wait_until = now + TIME_WAIT_MS;
                }
                State::TimeWait => tcb.time_wait_until = now + TIME_WAIT_MS,
                _ => {}
            }
        } else if h.flags & TCP_FIN != 0 && tcb.state == State::TimeWait {
            tcb.ack_pending = true;
            tcb.time_wait_until = now + TIME_WAIT_MS;
        }

        self.transmit_tcb(now, tcb);
        true
    }

    fn syn_sent_input(&mut self, now: u64, tcb: &mut Tcb, h: &TcpHeader) -> bool {
        let ack_ok = h.flags & TCP_ACK != 0;
        if ack_ok && (le(h.ack, tcb.iss) || lt(tcb.snd_max, h.ack)) {
            if h.flags & TCP_RST == 0 {
                self.send_reset_for(tcb.remote, h, 0);
            }
            return true;
        }
        if h.flags & TCP_RST != 0 {
            if ack_ok {
                tcb.set_error(Error::Refused);
            }
            return true;
        }
        if h.flags & TCP_SYN == 0 {
            return true;
        }
        tcb.rcv_nxt = h.seq.wrapping_add(1);
        tcb.mss = h.mss.unwrap_or(DEFAULT_MSS).clamp(64, OUR_MSS) as u32;
        tcb.cwnd = initial_cwnd(tcb.mss);
        tcb.snd_wnd = h.window as u32;
        tcb.negotiate_scale(h.wscale);
        tcb.snd_wl1 = h.seq;
        tcb.snd_wl2 = h.ack;
        tcb.ack_pending = true;
        if ack_ok {
            if let Some((_, sent)) = tcb.rtt_probe.take()
                && tcb.retransmits == 0
            {
                tcb.update_rtt(now.saturating_sub(sent).max(1));
            }
            tcb.snd_una = h.ack;
            tcb.state = State::Established;
            tcb.retransmit_at = None;
            tcb.retries = 0;
            self.transmit_tcb(now, tcb);
        } else {
            // Eşzamanlı açılış.
            tcb.state = State::SynReceived;
            let iss = tcb.iss;
            self.emit(tcb, iss, TCP_SYN | TCP_ACK, Some(OUR_MSS), &[]);
        }
        true
    }

    fn process_ack(&mut self, now: u64, tcb: &mut Tcb, h: &TcpHeader, pure_ack: bool) {
        let ack = h.ack;
        if lt(tcb.snd_una, ack) {
            let acked = ack.wrapping_sub(tcb.snd_una);
            let mut data_acked = acked as usize;
            if tcb.fin_sent && le(tcb.fin_seq.wrapping_add(1), ack) {
                data_acked -= 1;
            }
            let data_acked = data_acked.min(tcb.send_buffer.len());
            tcb.send_buffer.drain(..data_acked);
            tcb.snd_una = ack;
            if lt(tcb.snd_nxt, ack) {
                tcb.snd_nxt = ack;
            }

            if let Some((seq, sent)) = tcb.rtt_probe
                && le(seq, ack)
            {
                tcb.update_rtt(now.saturating_sub(sent).max(1));
                tcb.rtt_probe = None;
            }

            if tcb.in_recovery {
                if le(tcb.recover, ack) {
                    tcb.in_recovery = false;
                    tcb.cwnd = tcb.ssthresh.min(tcb.in_flight().max(tcb.mss) + tcb.mss);
                } else {
                    // Kısmi ACK (RFC 6582): bir sonraki boşluğu hemen doldur.
                    self.retransmit_first(tcb);
                    tcb.cwnd = tcb.cwnd.saturating_sub(acked).saturating_add(tcb.mss);
                }
            } else if tcb.cwnd < tcb.ssthresh {
                tcb.cwnd = tcb.cwnd.saturating_add(acked.min(tcb.mss));
            } else {
                tcb.cwnd = tcb
                    .cwnd
                    .saturating_add((tcb.mss * tcb.mss / tcb.cwnd.max(1)).max(1));
            }
            tcb.duplicate_acks = 0;
            tcb.retries = 0;
            tcb.retransmit_at = (tcb.outstanding() > 0).then(|| now + tcb.rto_ms);
        } else if ack == tcb.snd_una
            && pure_ack
            && tcb.outstanding() > 0
            && (h.window as u32) << tcb.snd_shift == tcb.snd_wnd
        {
            tcb.duplicate_acks += 1;
            if tcb.duplicate_acks == 3 && !tcb.in_recovery {
                tcb.ssthresh = (tcb.outstanding() / 2).max(2 * tcb.mss);
                tcb.recover = tcb.snd_max;
                tcb.in_recovery = true;
                self.retransmit_first(tcb);
                tcb.cwnd = tcb.ssthresh + 3 * tcb.mss;
            } else if tcb.duplicate_acks > 3 && tcb.in_recovery {
                tcb.cwnd = tcb.cwnd.saturating_add(tcb.mss);
            }
        }

        if lt(tcb.snd_wl1, h.seq) || (tcb.snd_wl1 == h.seq && le(tcb.snd_wl2, ack)) {
            tcb.snd_wnd = (h.window as u32) << tcb.snd_shift;
            tcb.snd_wl1 = h.seq;
            tcb.snd_wl2 = ack;
            if tcb.snd_wnd > 0 {
                tcb.persist_at = None;
            }
        }
    }

    fn accept_data(&mut self, tcb: &mut Tcb, seq: u32, payload: &[u8]) {
        let mut seq = seq;
        let mut data = payload;
        if lt(seq, tcb.rcv_nxt) {
            let skip = tcb.rcv_nxt.wrapping_sub(seq) as usize;
            if skip >= data.len() {
                tcb.ack_pending = true;
                return;
            }
            data = &data[skip..];
            seq = tcb.rcv_nxt;
        }
        let room = tcb.receive_window() as usize;
        let offset = seq.wrapping_sub(tcb.rcv_nxt) as usize;
        if offset >= room {
            tcb.ack_pending = true;
            return;
        }
        let data = &data[..data.len().min(room - offset)];
        tcb.ack_pending = true;
        if seq != tcb.rcv_nxt {
            if tcb.out_of_order_bytes + data.len() <= OUT_OF_ORDER_LIMIT
                && !tcb.out_of_order.contains_key(&seq)
            {
                tcb.out_of_order_bytes += data.len();
                tcb.out_of_order.insert(seq, data.to_vec());
            }
            return;
        }
        tcb.receive_buffer.extend(data);
        tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(data.len() as u32);
        // Sırasız gelenlerden artık sıraya girenleri ekle.
        loop {
            let next = tcb
                .out_of_order
                .iter()
                .find(|(s, d)| {
                    le(**s, tcb.rcv_nxt) && lt(tcb.rcv_nxt, s.wrapping_add(d.len() as u32))
                })
                .map(|(s, _)| *s);
            let stale: Vec<u32> = tcb
                .out_of_order
                .iter()
                .filter(|(s, d)| le(s.wrapping_add(d.len() as u32), tcb.rcv_nxt))
                .map(|(s, _)| *s)
                .collect();
            for s in stale {
                if let Some(d) = tcb.out_of_order.remove(&s) {
                    tcb.out_of_order_bytes -= d.len();
                }
            }
            let Some(s) = next else { break };
            let Some(d) = tcb.out_of_order.remove(&s) else {
                break;
            };
            tcb.out_of_order_bytes -= d.len();
            let skip = tcb.rcv_nxt.wrapping_sub(s) as usize;
            let room = tcb.receive_window() as usize;
            let part = &d[skip..d.len().min(skip + room)];
            tcb.receive_buffer.extend(part);
            tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(part.len() as u32);
        }
    }

    // ---- zamanlayıcılar ----

    /// Yeniden iletim, sıfır pencere yoklaması ve TIME-WAIT sürelerini işler.
    /// Bir soketin durumu değiştiyse `true` (bekleyenler uyandırılmalı).
    pub fn poll(&mut self, now: u64) -> bool {
        let mut changed = false;
        let ids: Vec<SocketId> = self.sockets.keys().copied().collect();
        for id in ids {
            let Some(mut tcb) = self.sockets.remove(&id) else {
                continue;
            };
            let mut keep = true;
            if tcb.state == State::TimeWait && now >= tcb.time_wait_until {
                tcb.state = State::Closed;
                changed = true;
            }
            if let Some(at) = tcb.retransmit_at
                && now >= at
            {
                tcb.retries += 1;
                let limit = if matches!(tcb.state, State::SynSent | State::SynReceived) {
                    MAX_SYN_RETRIES
                } else {
                    MAX_RETRIES
                };
                if tcb.retries > limit {
                    if tcb.parent.is_some() && tcb.state == State::SynReceived {
                        keep = false;
                    } else {
                        let seq = tcb.snd_nxt;
                        self.emit(&mut tcb, seq, TCP_RST | TCP_ACK, None, &[]);
                        tcb.set_error(Error::TimedOut);
                    }
                    changed = true;
                } else {
                    if tcb.synchronized() {
                        // Go-back-N: tıkanıklık penceresi 1 MSS'ye iner, onaylanmamış
                        // her şey `snd_una`'dan itibaren yeniden gönderilir.
                        tcb.ssthresh = (tcb.outstanding() / 2).max(2 * tcb.mss);
                        tcb.cwnd = tcb.mss;
                        tcb.in_recovery = false;
                        tcb.duplicate_acks = 0;
                        tcb.rtt_probe = None;
                        tcb.retransmits += 1;
                        tcb.snd_nxt = tcb.snd_una;
                    } else {
                        self.retransmit_first(&mut tcb);
                    }
                    tcb.rto_ms = (tcb.rto_ms * 2).min(MAX_RTO_MS);
                    tcb.retransmit_at = Some(now + tcb.rto_ms);
                }
            }
            if let Some(at) = tcb.persist_at
                && now >= at
                && tcb.state != State::Closed
            {
                // Sıfır pencere yoklaması: pencere dışındaki ilk bayt.
                let offset = tcb.sent_data();
                if let Some(&byte) = tcb.send_buffer.get(offset) {
                    let seq = tcb.snd_nxt;
                    self.emit(&mut tcb, seq, TCP_ACK, None, &[byte]);
                }
                tcb.rto_ms = (tcb.rto_ms * 2).min(MAX_RTO_MS);
                tcb.persist_at = Some(now + tcb.rto_ms);
            }
            if keep && tcb.state != State::Closed {
                self.transmit_tcb(now, &mut tcb);
            }
            if tcb.state == State::Closed && (tcb.user_closed || tcb.parent.is_some()) {
                keep = false;
            }
            if keep {
                self.sockets.insert(id, tcb);
            } else {
                changed = true;
            }
        }
        changed
    }
}

fn initial_cwnd(mss: u32) -> u32 {
    // RFC 6928: min(10*MSS, max(2*MSS, 14600))
    (10 * mss).min((2 * mss).max(14600))
}

/// İki uç arasında paket alışverişini taklit eder: el sıkışma, kayıplı aktarım,
/// sırasız teslim, kapanış.
pub fn self_test() -> bool {
    let a_ip = Ipv4([10, 0, 0, 1]);
    let b_ip = Ipv4([10, 0, 0, 2]);
    let mut a = Tcp::new(1);
    let mut b = Tcp::new(2);
    let mut now = 0u64;
    let Ok(listener) = b.listen(80) else {
        return false;
    };
    let Ok(client) = a.connect(now, b_ip, 80, 0xffff_ff00) else {
        return false;
    };

    let mut dropped = 0;
    let mut counter = 0u32;
    // Her 7. veri segmentini düşürür, her 5.'yi bir tur geciktirir.
    let mut deliver = |a: &mut Tcp, b: &mut Tcp, now: u64, held: &mut Vec<(bool, Outgoing)>| {
        let mut batch: Vec<(bool, Outgoing)> = core::mem::take(held);
        batch.extend(a.output.drain(..).map(|o| (true, o)));
        batch.extend(b.output.drain(..).map(|o| (false, o)));
        for (from_a, o) in batch {
            counter += 1;
            if !o.payload.is_empty() && counter % 7 == 0 && dropped < 40 {
                dropped += 1;
                continue;
            }
            if !o.payload.is_empty()
                && counter % 5 == 0
                && !held.iter().any(|(_, h)| h.header == o.header)
            {
                held.push((from_a, o));
                continue;
            }
            if from_a {
                b.input(now, a_ip, &o.header, &o.payload);
            } else {
                a.input(now, b_ip, &o.header, &o.payload);
            }
        }
    };

    let mut held = Vec::new();
    // Pencere ölçekleme: karşının duyurduğu pencere 16 bitlik alanı aşmalı.
    let mut widest = 0u32;
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let mut sent = 0;
    let mut server = None;
    let mut received = Vec::new();
    let mut echoed = Vec::new();
    for _ in 0..20_000 {
        now += 5;
        if server.is_none() {
            server = b.accept(listener).ok();
        }
        if a.state(client) == Some(State::Established) && sent < data.len() {
            if let Ok(n) = a.send(now, client, &data[sent..(sent + 3000).min(data.len())]) {
                sent += n;
            }
            if sent == data.len() {
                a.close(now, client);
            }
        }
        if let Some(s) = server {
            while let Ok(Received::Data(d)) = b.receive(now, s, 4096) {
                received.extend_from_slice(&d);
            }
            if matches!(b.receive(now, s, 1), Ok(Received::End)) && echoed.is_empty() {
                echoed.push(1);
                let _ = b.send(now, s, b"bye");
                b.close(now, s);
            }
        }
        if let Some(t) = a.sockets.get(&client) {
            widest = widest.max(t.snd_wnd);
        }
        deliver(&mut a, &mut b, now, &mut held);
        a.poll(now);
        b.poll(now);
        if a.state(client).is_none() && b.sockets.is_empty() {
            break;
        }
    }
    let refused = {
        let mut c = Tcp::new(3);
        let id = c.connect(0, b_ip, 81, 1).ok();
        let syn = c.output.pop();
        if let Some(syn) = syn {
            b.input(0, a_ip, &syn.header, &syn.payload);
        }
        let rst = b.output.pop();
        if let (Some(id), Some(rst)) = (id, rst) {
            c.input(0, b_ip, &rst.header, &rst.payload);
            c.error(id) == Some(Error::Refused)
        } else {
            false
        }
    };
    // Karşı ölçekleme önermezse (eski yığınlar) iki yön de ölçeksiz kalmalı.
    let unscaled = {
        let mut c = Tcp::new(4);
        let mut d = Tcp::new(5);
        let ok = d.listen(90).is_ok();
        let id = c.connect(0, b_ip, 90, 7).ok();
        let syn = c.output.pop();
        if let (true, Some(id), Some(mut syn)) = (ok, id, syn) {
            syn.header.wscale = None;
            d.input(0, a_ip, &syn.header, &syn.payload);
            let syn_ack = d.output.pop();
            let plain = syn_ack.as_ref().is_some_and(|o| o.header.wscale.is_none());
            if let Some(o) = syn_ack {
                let mut h = o.header;
                h.wscale = None;
                c.input(0, b_ip, &h, &o.payload);
            }
            plain
                && c.sockets
                    .get(&id)
                    .is_some_and(|t| t.snd_shift == 0 && t.rcv_shift == 0)
        } else {
            false
        }
    };
    received == data
        && dropped > 0
        && refused
        && b.sockets.is_empty()
        && widest > u16::MAX as u32
        && unscaled
}
