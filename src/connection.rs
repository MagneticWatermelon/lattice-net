//! A connected peer: packet sequencing, acks, RTT/loss estimation and channels.
//! Used by both `Server` (one per client) and `Client`.

use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

use crate::channel::{self, PacketIds, ReliableReceiver, ReliableSender, KIND_PADDING, KIND_RELIABLE, KIND_UNRELIABLE};
use crate::packet::{self, AckHeader, Packet, ACK_DELAY_UNIT_US, PAYLOAD_OVERHEAD};
use crate::seq::SequenceBuffer;
use crate::wire::{DecodeError, Reader, Writer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Latest-wins. Anything that doesn't fit in this flush's packets is dropped.
    /// Arrives possibly out of order: tag state with a tick number.
    Unreliable,
    /// Ordered, exactly-once, resent until acked.
    Reliable,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Mixed into every packet's CRC. Bump it when the protocol changes.
    pub protocol_id: u64,
    /// Drop the connection after this long with no valid packets.
    pub timeout: Duration,
    /// Send an empty packet (carrying acks) if nothing was sent for this long.
    pub keepalive_interval: Duration,
    pub handshake_resend_interval: Duration,
    pub max_packet_size: usize,
    /// Upper bound on packets one `flush` may emit for one connection.
    pub max_packets_per_flush: usize,
    /// Server: most new connections accepted per tick (between `update` calls),
    /// server-wide, split evenly across shards (rounded up). A client over the
    /// budget is simply not answered; it resends its challenge response every
    /// `handshake_resend_interval` and its cookie stays valid for 10-20 s, so a
    /// mass join is spread over several ticks instead of stalling one.
    /// Each accept allocates the connection's windows (~130 KB). 0 = no limit.
    pub max_accepts_per_tick: usize,
    /// Pad every packet of a flush but the last to `max_packet_size`, so the
    /// flush can go out as one GSO send (`UDP_SEGMENT` needs equal-size
    /// segments). Receivers need no setting: padding always parses.
    pub pad_packets: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // LATTICE2: padding messages. LATTICE1: 256-message reliable windows.
            protocol_id: u64::from_le_bytes(*b"LATTICE2"),
            timeout: Duration::from_secs(5),
            keepalive_interval: Duration::from_millis(100),
            handshake_resend_interval: Duration::from_millis(100),
            max_packet_size: packet::MAX_PACKET_SIZE,
            max_packets_per_flush: 4,
            max_accepts_per_tick: 256,
            pad_packets: false,
        }
    }
}

impl Config {
    /// Largest single message: must fit in one packet (no fragmentation yet).
    pub fn max_message_size(&self) -> usize {
        // worst-case message header: kind(1) + id(2) + varlen(2)
        self.max_packet_size - PAYLOAD_OVERHEAD - 5
    }

    /// Bytes of messages one packet holds.
    pub fn packet_body_size(&self) -> usize {
        self.max_packet_size - PAYLOAD_OVERHEAD
    }

    /// Bytes an unreliable message of `len` takes in a packet, framing included.
    /// With `packet_body_size`, lets a caller predict how its messages pack.
    pub fn unreliable_wire_size(len: usize) -> usize {
        channel::wire_size(false, len)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub packets_acked: u64,
    /// Sent packets never acked within `LOSS_LAG` packets.
    pub packets_lost: u64,
    pub duplicate_packets: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Unreliable messages that didn't fit in the flush they were queued for.
    pub unreliable_dropped: u64,
    /// Bytes of padding sent (`Config::pad_packets`), included in `bytes_sent`.
    pub padding_bytes: u64,
    /// Smoothed network RTT in ms (EWMA, alpha 0.1). The peer's ack delay is
    /// subtracted, so its tick rate doesn't count. What's left includes the gap
    /// between a datagram arriving and the `now` passed to `receive`, so pass
    /// arrival timestamps where you have them.
    pub rtt_ms: f32,
    /// Smoothed packet loss 0..1 (EWMA, alpha 0.05).
    pub loss: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    NotConnected,
    UnknownClient,
    MessageTooLarge { size: usize, max: usize },
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::NotConnected => write!(f, "not connected"),
            SendError::UnknownClient => write!(f, "unknown client"),
            SendError::MessageTooLarge { size, max } => write!(f, "message of {size} B exceeds max {max} B"),
        }
    }
}
impl std::error::Error for SendError {}

/// Sent packets tracked for acks. Must exceed `LOSS_LAG`, and covers ~3 s at the
/// ~90 packets/s a client gets once snapshots span several packets.
const SENT_BUFFER: usize = 256;
/// Received packets remembered: only feeds the 33-packet ack field and
/// duplicate detection, so a packet older than this is dropped as a duplicate.
const RECV_BUFFER: usize = 128;
/// A sent packet still unacked after this many newer packets counts as lost.
const LOSS_LAG: u16 = 128;
const _: () = assert!(SENT_BUFFER > LOSS_LAG as usize);
const DEFAULT_RTT_MS: f32 = 100.0;

struct SentPacket {
    time: Instant,
    acked: bool,
    reliable: PacketIds,
    tags: PacketTags,
}

/// Most tagged unreliable messages one packet carries (their tags are stored
/// inline with the sent packet, so this bounds its size).
pub const MAX_TAGS_PER_PACKET: usize = 8;
/// Acked tags kept for the application; beyond this the oldest are dropped.
const MAX_ACKED_TAGS: usize = 4096;

#[derive(Debug, Clone, Copy, Default)]
struct PacketTags {
    len: u8,
    tags: [u32; MAX_TAGS_PER_PACKET],
}

impl PacketTags {
    fn is_full(&self) -> bool {
        self.len as usize == MAX_TAGS_PER_PACKET
    }

    fn push(&mut self, tag: u32) {
        self.tags[self.len as usize] = tag;
        self.len += 1;
    }

    fn as_slice(&self) -> &[u32] {
        &self.tags[..self.len as usize]
    }
}

pub struct Connection {
    cfg: Config,
    session: u32,
    local_seq: u16,
    sent: SequenceBuffer<SentPacket>,
    /// Arrival time of each received packet, for `ack_delay`.
    received: SequenceBuffer<Instant>,
    reliable_tx: ReliableSender,
    reliable_rx: ReliableReceiver,
    /// Queued unreliable messages, with the application's tag if it asked to
    /// hear about their delivery.
    unreliable_tx: VecDeque<(Vec<u8>, Option<u32>)>,
    /// Tags of delivered (acked) messages, until `take_acked`.
    acked_tags: Vec<u32>,
    inbox: VecDeque<(Channel, Vec<u8>)>,
    last_recv: Instant,
    last_send: Option<Instant>,
    rtt_samples: u64,
    stats: Stats,
}

impl Connection {
    pub(crate) fn new(cfg: Config, session: u32, now: Instant) -> Self {
        Self {
            cfg,
            session,
            local_seq: 0,
            sent: SequenceBuffer::new(SENT_BUFFER),
            received: SequenceBuffer::new(RECV_BUFFER),
            reliable_tx: ReliableSender::new(),
            reliable_rx: ReliableReceiver::new(),
            unreliable_tx: VecDeque::new(),
            acked_tags: Vec::new(),
            inbox: VecDeque::new(),
            last_recv: now,
            last_send: None,
            rtt_samples: 0,
            stats: Stats::default(),
        }
    }

    /// Back to the state of `Connection::new` for a new peer, keeping every
    /// allocation, so a server can recycle connections instead of allocating
    /// (and page-faulting) fresh windows on each accept.
    pub(crate) fn reset(&mut self, session: u32, now: Instant) {
        // Destructured so that a new field can't be forgotten here.
        let Connection {
            cfg: _,
            session: sess,
            local_seq,
            sent,
            received,
            reliable_tx,
            reliable_rx,
            unreliable_tx,
            acked_tags,
            inbox,
            last_recv,
            last_send,
            rtt_samples,
            stats,
        } = self;
        *sess = session;
        *local_seq = 0;
        sent.clear();
        received.clear();
        reliable_tx.reset();
        reliable_rx.reset();
        unreliable_tx.clear();
        acked_tags.clear();
        inbox.clear();
        *last_recv = now;
        *last_send = None;
        *rtt_samples = 0;
        *stats = Stats::default();
    }

    pub fn send(&mut self, channel: Channel, data: Vec<u8>) -> Result<(), SendError> {
        let max = self.cfg.max_message_size();
        if data.len() > max {
            return Err(SendError::MessageTooLarge { size: data.len(), max });
        }
        match channel {
            Channel::Reliable => self.reliable_tx.push(data),
            Channel::Unreliable => self.unreliable_tx.push_back((data, None)),
        }
        Ok(())
    }

    /// An unreliable message whose delivery the application wants to hear
    /// about: once a packet carrying it is acked, `tag` shows up in
    /// `take_acked`. Loss is never reported; a tag just doesn't come back.
    /// The transport doesn't interpret tags.
    pub fn send_tagged(&mut self, data: Vec<u8>, tag: u32) -> Result<(), SendError> {
        let max = self.cfg.max_message_size();
        if data.len() > max {
            return Err(SendError::MessageTooLarge { size: data.len(), max });
        }
        self.unreliable_tx.push_back((data, Some(tag)));
        Ok(())
    }

    /// Moves the tags of messages acked since the last call into `out`.
    pub fn take_acked(&mut self, out: &mut Vec<u32>) {
        out.append(&mut self.acked_tags);
    }

    pub fn recv(&mut self) -> Option<(Channel, Vec<u8>)> {
        self.inbox.pop_front()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn rtt(&self) -> Duration {
        Duration::from_secs_f32(self.rtt_ms() / 1000.0)
    }

    /// Reliable messages not yet acked (in flight + backlog).
    pub fn reliable_pending(&self) -> usize {
        self.reliable_tx.pending()
    }

    pub(crate) fn session(&self) -> u32 {
        self.session
    }

    pub(crate) fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_recv) > self.cfg.timeout
    }

    fn rtt_ms(&self) -> f32 {
        if self.rtt_samples == 0 {
            DEFAULT_RTT_MS
        } else {
            self.stats.rtt_ms
        }
    }

    fn resend_interval(&self) -> Duration {
        Duration::from_secs_f32((self.rtt_ms() * 1.25).max(20.0) / 1000.0)
    }

    /// Handle a decoded, session-checked payload packet.
    pub(crate) fn on_payload(&mut self, h: AckHeader, body: &[u8], now: Instant) -> Result<(), DecodeError> {
        // Parse fully before committing: never ack a packet we couldn't process,
        // or the sender would consider its reliable messages delivered.
        let mut msgs: Vec<(Option<u16>, &[u8])> = Vec::new();
        let mut r = Reader::new(body);
        while r.remaining() > 0 {
            let id = match r.u8()? {
                KIND_RELIABLE => Some(r.u16()?),
                KIND_UNRELIABLE => None,
                KIND_PADDING if r.rest().iter().all(|&b| b == 0) => break,
                _ => return Err(DecodeError::Invalid),
            };
            let len = r.varlen()?;
            msgs.push((id, r.take(len)?));
        }

        if self.received.exists(h.seq) || !self.received.insert(h.seq, now) {
            self.stats.duplicate_packets += 1; // duplicate, or too old to track
            return Ok(());
        }
        self.last_recv = now;
        self.stats.packets_received += 1;
        self.stats.bytes_received += (PAYLOAD_OVERHEAD + body.len()) as u64;

        self.process_acks(h, now);

        for (id, data) in msgs {
            match id {
                Some(id) => self.reliable_rx.on_message(id, data),
                None => self.inbox.push_back((Channel::Unreliable, data.to_vec())),
            }
        }
        self.reliable_rx.deliver(&mut self.inbox);
        Ok(())
    }

    fn process_acks(&mut self, h: AckHeader, now: Instant) {
        let (ack, bits) = (h.ack, h.ack_bits);
        for i in 0..=32u16 {
            let acked = i == 0 || bits & (1 << (i - 1)) != 0;
            if !acked {
                continue;
            }
            let Some(sp) = self.sent.get_mut(ack.wrapping_sub(i)) else { continue };
            if sp.acked {
                continue;
            }
            sp.acked = true;
            let ids = std::mem::take(&mut sp.reliable);
            let tags = std::mem::take(&mut sp.tags);
            self.acked_tags.extend_from_slice(tags.as_slice());
            if self.acked_tags.len() > MAX_ACKED_TAGS {
                let excess = self.acked_tags.len() - MAX_ACKED_TAGS;
                self.acked_tags.drain(..excess);
            }
            self.stats.packets_acked += 1;

            // Only the newest ack carries the peer's hold time (as in QUIC). A packet
            // first acked through ack_bits was held for an unknown extra time: skip it.
            if i == 0 {
                let hold = Duration::from_micros(h.ack_delay as u64 * ACK_DELAY_UNIT_US);
                let sample = now.saturating_duration_since(sp.time).saturating_sub(hold).as_secs_f32() * 1000.0;
                self.rtt_samples += 1;
                self.stats.rtt_ms = if self.rtt_samples == 1 {
                    sample
                } else {
                    self.stats.rtt_ms + (sample - self.stats.rtt_ms) * 0.1
                };
            }
            if !ids.is_empty() {
                self.reliable_tx.on_acked(ids.as_slice());
            }
        }
    }

    fn ack_fields(&self, seq: u16, now: Instant) -> AckHeader {
        // Before anything is received this acks 65535. Harmless: we'd have to
        // send 65536 packets with zero replies first, and we time out long before that.
        let ack = self.received.sequence().wrapping_sub(1);
        let mut ack_bits = 0u32;
        for i in 0..32u16 {
            if self.received.exists(ack.wrapping_sub(i + 1)) {
                ack_bits |= 1 << i;
            }
        }
        let held = self.received.get(ack).map_or(Duration::ZERO, |&at| now.saturating_duration_since(at));
        let ack_delay = (held.as_micros() as u64 / ACK_DELAY_UNIT_US).min(u16::MAX as u64) as u16;
        AckHeader { seq, ack, ack_bits, ack_delay }
    }

    /// Frame one packet body, optionally padded to `max_packet_size`.
    fn emit(&mut self, mut body: Writer, ids: PacketIds, tags: PacketTags, pad: bool, now: Instant, out: &mut Vec<Vec<u8>>) {
        let budget = self.cfg.packet_body_size();
        if pad && body.len() < budget {
            self.stats.padding_bytes += (budget - body.len()) as u64;
            body.u8(KIND_PADDING);
            body.pad_to(budget);
        }
        let seq = self.local_seq;

        // Each sequence is judged exactly once, LOSS_LAG packets after it was sent.
        if let Some(old) = self.sent.get(seq.wrapping_sub(LOSS_LAG)) {
            let lost = !old.acked;
            self.stats.packets_lost += lost as u64;
            self.stats.loss += (lost as u8 as f32 - self.stats.loss) * 0.05;
        }

        let header = self.ack_fields(seq, now);
        let pkt = packet::encode(
            self.cfg.protocol_id,
            &Packet::Payload {
                session: self.session,
                header,
                body: body.as_slice(),
            },
        );
        self.sent.insert(seq, SentPacket { time: now, acked: false, reliable: ids, tags });
        self.local_seq = seq.wrapping_add(1);
        self.last_send = Some(now);
        self.stats.packets_sent += 1;
        self.stats.bytes_sent += pkt.len() as u64;
        out.push(pkt);
    }

    /// Build this tick's packets: due reliable messages first, then queued
    /// unreliable ones. Emits a keepalive if nothing else was sent recently.
    /// With `pad_packets`, all but the last are padded to `max_packet_size`.
    pub(crate) fn flush(&mut self, now: Instant, out: &mut Vec<Vec<u8>>) {
        let budget = self.cfg.packet_body_size();
        let resend = self.resend_interval();
        let keepalive_due =
            self.last_send.is_none_or(|t| now.saturating_duration_since(t) >= self.cfg.keepalive_interval);

        // Each body is held back until the next one exists, so only the last
        // one goes out unpadded.
        let mut pending: Option<(Writer, PacketIds, PacketTags)> = None;
        let mut produced = 0;
        while produced < self.cfg.max_packets_per_flush {
            let mut body = Writer::with_capacity(budget);
            let mut ids = PacketIds::default();
            let mut tags = PacketTags::default();
            self.reliable_tx.write(&mut body, budget, now, resend, &mut ids);

            while let Some((front, tag)) = self.unreliable_tx.front() {
                if body.len() + channel::wire_size(false, front.len()) > budget || (tag.is_some() && tags.is_full()) {
                    break;
                }
                let (msg, tag) = self.unreliable_tx.pop_front().unwrap();
                if let Some(t) = tag {
                    tags.push(t);
                }
                channel::write_message(&mut body, None, &msg);
            }

            if body.is_empty() && !(produced == 0 && keepalive_due) {
                break;
            }
            if let Some((prev, ids, tags)) = pending.replace((body, ids, tags)) {
                self.emit(prev, ids, tags, self.cfg.pad_packets, now, out);
            }
            produced += 1;
        }
        if let Some((last, ids, tags)) = pending {
            self.emit(last, ids, tags, false, now, out);
        }

        self.stats.unreliable_dropped += self.unreliable_tx.len() as u64;
        self.unreliable_tx.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(bytes: &[u8]) -> (AckHeader, Vec<u8>) {
        match packet::decode(Config::default().protocol_id, bytes).unwrap() {
            Packet::Payload { header, body, .. } => (header, body.to_vec()),
            p => panic!("not a payload: {p:?}"),
        }
    }

    #[test]
    fn padding_fills_all_but_the_last_packet_and_parses_away() {
        let t0 = Instant::now();
        let cfg = Config { max_packets_per_flush: 8, pad_packets: true, ..Config::default() };
        let (mut a, mut b) = (Connection::new(cfg.clone(), 1, t0), Connection::new(cfg, 1, t0));
        let msgs: Vec<Vec<u8>> = (0..5).map(|i| vec![i as u8 + 1; 500]).collect();
        for m in &msgs {
            a.send(Channel::Unreliable, m.clone()).unwrap();
        }
        let mut out = Vec::new();
        a.flush(t0, &mut out);
        // Two 500 B messages per packet: 3 packets, the first two padded.
        let sizes: Vec<usize> = out.iter().map(Vec::len).collect();
        assert_eq!(sizes[..2], [packet::MAX_PACKET_SIZE; 2]);
        assert!(sizes[2] < packet::MAX_PACKET_SIZE);
        // Each padded body: two 503 B messages (kind, varlen, data) in 1181 B.
        assert_eq!(a.stats().padding_bytes, 2 * (1181 - 2 * 503));
        for p in &out {
            let (h, body) = payload(p);
            b.on_payload(h, &body, t0).unwrap();
        }
        let got: Vec<Vec<u8>> = std::iter::from_fn(|| b.recv()).map(|(_, m)| m).collect();
        assert_eq!(got, msgs);

        // A single packet is never padded.
        a.send(Channel::Unreliable, vec![9; 10]).unwrap();
        out.clear();
        a.flush(t0, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].len() < 40);
    }

    #[test]
    fn padding_must_be_zeros() {
        let t0 = Instant::now();
        let mut b = Connection::new(Config::default(), 1, t0);
        let h = AckHeader { seq: 0, ack: 0, ack_bits: 0, ack_delay: 0 };
        assert!(b.on_payload(h, &[KIND_PADDING, 0, 0, 0], t0).is_ok());
        let h = AckHeader { seq: 1, ..h };
        assert_eq!(b.on_payload(h, &[KIND_PADDING, 0, 7, 0], t0), Err(DecodeError::Invalid));
    }

    #[test]
    fn tags_come_back_when_their_packet_is_acked_and_never_when_lost() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let cfg = Config { max_packets_per_flush: 8, ..Config::default() };
        let (mut a, mut b) = (Connection::new(cfg.clone(), 1, t0), Connection::new(cfg, 1, t0));
        let mut out = Vec::new();

        // 12 small tagged messages: at most 8 tags per packet, so two packets.
        for tag in 0..12 {
            a.send_tagged(vec![tag as u8; 10], tag).unwrap();
        }
        a.flush(ms(0), &mut out);
        assert_eq!(out.len(), 2);
        // The first packet is lost; the second arrives and b acks it.
        let (h, body) = payload(&out[1]);
        b.on_payload(h, &body, ms(5)).unwrap();
        let mut back = Vec::new();
        b.flush(ms(10), &mut back);
        let (h, body) = payload(&back[0]);
        a.on_payload(h, &body, ms(15)).unwrap();

        let mut acked = Vec::new();
        a.take_acked(&mut acked);
        assert_eq!(acked, (8..12).collect::<Vec<u32>>(), "only the delivered packet's tags");
        a.take_acked(&mut acked);
        assert_eq!(acked.len(), 4, "each tag is reported once");
    }

    #[test]
    fn rtt_excludes_the_peers_hold_time() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let (mut a, mut b) = (Connection::new(Config::default(), 1, t0), Connection::new(Config::default(), 1, t0));
        let mut out = Vec::new();

        // A sends at 0; 10 ms of network; B holds it 30 ms (its tick), then replies;
        // 10 ms back. The network RTT is 20 ms, though the ack took 50.
        a.flush(ms(0), &mut out);
        let (h, body) = payload(&out.pop().unwrap());
        b.on_payload(h, &body, ms(10)).unwrap();
        b.flush(ms(40), &mut out);
        let (h, body) = payload(&out.pop().unwrap());
        assert_eq!(h.ack_delay, 3000, "30 ms in 10 us units");
        a.on_payload(h, &body, ms(50)).unwrap();
        assert!((a.stats().rtt_ms - 20.0).abs() < 0.01, "rtt {}", a.stats().rtt_ms);
    }
}
