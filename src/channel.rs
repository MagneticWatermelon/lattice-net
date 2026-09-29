//! Message channels carried inside payload packets.
//!
//! Reliable-ordered works *on top of packet acks*: each sent packet remembers
//! which reliable message ids it carried. When that packet is acked, those
//! messages are done. Unacked messages are resent after ~1.25x RTT. The receiver
//! buffers out-of-order messages and delivers them strictly in order, exactly once.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::connection::Channel;
use crate::seq::{seq_lt, SequenceBuffer};
use crate::wire::Writer;

/// Max reliable messages in flight (sent but unacked). Also the receive window:
/// both ends must agree, so changing it changes the protocol.
pub(crate) const RELIABLE_WINDOW: usize = 256;
/// A sent packet stays tracked for `SENT_BUFFER` (256) packets, during which at
/// most 32 * 256 = 8192 new message ids are issued, far below 65536. So an ack
/// for an old packet can never refer to a message id that has since been reused.
pub(crate) const MAX_RELIABLE_PER_PACKET: usize = 32;

pub(crate) const KIND_UNRELIABLE: u8 = 0;
pub(crate) const KIND_RELIABLE: u8 = 1;
/// The rest of the packet is zero padding (see `Config::pad_packets`).
pub(crate) const KIND_PADDING: u8 = 2;

#[inline]
pub(crate) fn wire_size(reliable: bool, len: usize) -> usize {
    1 + if reliable { 2 } else { 0 } + if len < 0x80 { 1 } else { 2 } + len
}

#[inline]
pub(crate) fn write_message(w: &mut Writer, reliable_id: Option<u16>, data: &[u8]) {
    match reliable_id {
        Some(id) => {
            w.u8(KIND_RELIABLE);
            w.u16(id);
        }
        None => w.u8(KIND_UNRELIABLE),
    }
    w.varlen(data.len());
    w.bytes(data);
}

/// The reliable message ids one packet carried, inline: no allocation per sent packet.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PacketIds {
    len: u8,
    ids: [u16; MAX_RELIABLE_PER_PACKET],
}

impl PacketIds {
    pub fn push(&mut self, id: u16) {
        self.ids[self.len as usize] = id;
        self.len += 1;
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.ids[..self.len as usize]
    }
}

struct Pending {
    data: Vec<u8>,
    last_sent: Option<Instant>,
}

pub(crate) struct ReliableSender {
    window: SequenceBuffer<Pending>,
    next_id: u16,
    oldest_unacked: u16,
    /// Messages waiting for window space.
    backlog: VecDeque<Vec<u8>>,
}

impl ReliableSender {
    pub fn new() -> Self {
        Self {
            window: SequenceBuffer::lazy(RELIABLE_WINDOW),
            next_id: 0,
            oldest_unacked: 0,
            backlog: VecDeque::new(),
        }
    }

    /// Back to a fresh sender, keeping allocations (connection reuse).
    pub fn reset(&mut self) {
        self.window.clear();
        self.next_id = 0;
        self.oldest_unacked = 0;
        self.backlog.clear();
    }

    fn in_flight(&self) -> usize {
        self.next_id.wrapping_sub(self.oldest_unacked) as usize
    }

    pub fn pending(&self) -> usize {
        self.in_flight() + self.backlog.len()
    }

    pub fn push(&mut self, data: Vec<u8>) {
        self.backlog.push_back(data);
        self.fill_window();
    }

    fn fill_window(&mut self) {
        while self.in_flight() < RELIABLE_WINDOW {
            let Some(data) = self.backlog.pop_front() else { break };
            self.window.insert(self.next_id, Pending { data, last_sent: None });
            self.next_id = self.next_id.wrapping_add(1);
        }
    }

    /// Writes never-sent and due-for-resend messages, oldest first, into `w`.
    pub fn write(
        &mut self,
        w: &mut Writer,
        budget: usize,
        now: Instant,
        resend_after: Duration,
        ids: &mut PacketIds,
    ) {
        let mut id = self.oldest_unacked;
        while id != self.next_id && ids.len() < MAX_RELIABLE_PER_PACKET && budget - w.len() >= 5 {
            if let Some(p) = self.window.get_mut(id) {
                let due = p.last_sent.is_none_or(|t| now.saturating_duration_since(t) >= resend_after);
                if due && w.len() + wire_size(true, p.data.len()) <= budget {
                    write_message(w, Some(id), &p.data);
                    p.last_sent = Some(now);
                    ids.push(id);
                }
            }
            id = id.wrapping_add(1);
        }
    }

    pub fn on_acked(&mut self, ids: &[u16]) {
        for &id in ids {
            self.window.remove(id);
        }
        while self.oldest_unacked != self.next_id && !self.window.exists(self.oldest_unacked) {
            self.oldest_unacked = self.oldest_unacked.wrapping_add(1);
        }
        self.fill_window();
    }
}

pub(crate) struct ReliableReceiver {
    buffer: SequenceBuffer<Vec<u8>>,
    next_id: u16,
}

impl ReliableReceiver {
    pub fn new() -> Self {
        Self { buffer: SequenceBuffer::lazy(RELIABLE_WINDOW), next_id: 0 }
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.next_id = 0;
    }

    pub fn on_message(&mut self, id: u16, data: &[u8]) {
        if seq_lt(id, self.next_id) {
            return; // already delivered (resend whose ack was lost)
        }
        if id.wrapping_sub(self.next_id) as usize >= RELIABLE_WINDOW {
            return; // outside window: sender is misbehaving
        }
        if !self.buffer.exists(id) {
            self.buffer.insert(id, data.to_vec());
        }
    }

    pub fn deliver(&mut self, out: &mut VecDeque<(Channel, Vec<u8>)>) {
        while let Some(data) = self.buffer.remove(self.next_id) {
            out.push_back((Channel::Reliable, data));
            self.next_id = self.next_id.wrapping_add(1);
        }
    }
}
