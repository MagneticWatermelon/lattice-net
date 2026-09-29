//! Packet sealing: ChaCha20-Poly1305 (IETF, 96-bit nonces), one key per
//! direction per connection, both from the connect token (`token.rs`).
//!
//! A sealed packet is `prefix | ciphertext | tag:16`. The prefix (type, seq,
//! ...) stays readable; it and the protocol id are the associated data, so
//! neither can be altered, and a peer on another protocol version fails the
//! tag instead of a checksum.
//!
//! Nonce = `domain:4 | counter:8`. A (key, nonce) pair must never seal two
//! different messages, so each kind of sealed packet has its own domain and a
//! counter that is unique within it (see the `DOMAIN_*` constants).

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce, Tag};

use crate::token::Key;

pub const TAG_BYTES: usize = 16;
/// Largest prefix `seal`/`open` accept (the handshake response's).
const MAX_PREFIX: usize = 192;

/// Payload and disconnect packets. Counter: the sender's 64-bit packet
/// sequence, which only ever increases.
pub(crate) const DOMAIN_PACKET: u32 = 0;
/// The client's handshake response. Counter: the server's cookie, so a resend
/// with the same cookie seals the same (empty) message under the same nonce.
pub(crate) const DOMAIN_RESPONSE: u32 = 1;
/// The server's Accepted. Counter: the client's salt; resends repeat the same
/// message (salt, client id).
pub(crate) const DOMAIN_ACCEPTED: u32 = 2;

fn nonce(domain: u32, counter: u64) -> Nonce {
    let mut n = [0; 12];
    n[..4].copy_from_slice(&domain.to_le_bytes());
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n.into()
}

fn associated_data<'a>(buf: &'a mut [u8; 8 + MAX_PREFIX], protocol_id: u64, prefix: &[u8]) -> &'a [u8] {
    assert!(prefix.len() <= MAX_PREFIX);
    buf[..8].copy_from_slice(&protocol_id.to_le_bytes());
    buf[8..8 + prefix.len()].copy_from_slice(prefix);
    &buf[..8 + prefix.len()]
}

#[derive(Clone)]
pub(crate) struct Cipher(ChaCha20Poly1305);

impl Cipher {
    pub fn new(key: &Key) -> Self {
        Self(ChaCha20Poly1305::new(key.into()))
    }

    /// Encrypts `packet[prefix..]` in place and appends the tag.
    pub fn seal(&self, protocol_id: u64, domain: u32, counter: u64, packet: &mut Vec<u8>, prefix: usize) {
        let mut ad = [0; 8 + MAX_PREFIX];
        let (head, msg) = packet.split_at_mut(prefix);
        let ad = associated_data(&mut ad, protocol_id, head);
        let tag = self
            .0
            .encrypt_in_place_detached(&nonce(domain, counter), ad, msg)
            .expect("packets are far below the AEAD's size limit");
        packet.extend_from_slice(&tag);
    }

    /// Authenticates `packet` (sealed with `prefix` bytes of prefix) and
    /// decrypts its message into `out`. Returns the message length, or `None`
    /// if the packet is too short or wasn't sealed by the peer for this nonce.
    pub fn open(
        &self,
        protocol_id: u64,
        domain: u32,
        counter: u64,
        packet: &[u8],
        prefix: usize,
        out: &mut [u8],
    ) -> Option<usize> {
        let len = packet.len().checked_sub(prefix + TAG_BYTES)?;
        let out = out.get_mut(..len)?;
        let (head, rest) = packet.split_at(prefix);
        let (msg, tag) = rest.split_at(len);
        out.copy_from_slice(msg);
        let mut ad = [0; 8 + MAX_PREFIX];
        let ad = associated_data(&mut ad, protocol_id, head);
        self.0
            .decrypt_in_place_detached(&nonce(domain, counter), ad, out, Tag::from_slice(tag))
            .ok()?;
        Some(len)
    }
}

/// The full sequence closest to `top` (the newest one authenticated so far)
/// whose low 16 bits are `low`, as QUIC decodes packet numbers. Packets arrive
/// at most a few hundred apart, far inside the ±32,768 this resolves.
pub(crate) fn expand_seq(top: u64, low: u16) -> u64 {
    const WIN: u64 = 1 << 16;
    const HALF: u64 = WIN / 2;
    let cand = (top & !(WIN - 1)) | low as u64;
    if cand + HALF <= top && cand <= u64::MAX - WIN {
        cand + WIN
    } else if cand > top + HALF && cand >= WIN {
        cand - WIN
    } else {
        cand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_then_open_and_any_change_fails() {
        let (a, b) = (Cipher::new(&[1; 32]), Cipher::new(&[2; 32]));
        let mut pkt = vec![6, 0x34, 0x12];
        pkt.extend_from_slice(b"hello");
        a.seal(9, DOMAIN_PACKET, 0x1234, &mut pkt, 3);
        assert_eq!(pkt.len(), 3 + 5 + TAG_BYTES);
        assert_ne!(&pkt[3..8], b"hello");
        let mut out = [0; 64];
        assert_eq!(a.open(9, DOMAIN_PACKET, 0x1234, &pkt, 3, &mut out), Some(5));
        assert_eq!(&out[..5], b"hello");

        assert_eq!(b.open(9, DOMAIN_PACKET, 0x1234, &pkt, 3, &mut out), None, "other key");
        assert_eq!(a.open(8, DOMAIN_PACKET, 0x1234, &pkt, 3, &mut out), None, "other protocol");
        assert_eq!(a.open(9, DOMAIN_PACKET, 0x1235, &pkt, 3, &mut out), None, "other counter");
        assert_eq!(a.open(9, DOMAIN_RESPONSE, 0x1234, &pkt, 3, &mut out), None, "other domain");
        for i in 0..pkt.len() {
            let mut bad = pkt.clone();
            bad[i] ^= 0x80;
            assert_eq!(a.open(9, DOMAIN_PACKET, 0x1234, &bad, 3, &mut out), None, "flipped byte {i}");
        }
        assert_eq!(a.open(9, DOMAIN_PACKET, 0x1234, &pkt[..10], 3, &mut out), None, "truncated");
        assert_eq!(a.open(9, DOMAIN_PACKET, 0x1234, &pkt, 3, &mut out[..2]), None, "no room");
    }

    #[test]
    fn expand_seq_follows_the_counter_across_wraps() {
        assert_eq!(expand_seq(0, 0), 0);
        assert_eq!(expand_seq(0, 5), 5);
        assert_eq!(expand_seq(65_530, 3), 65_539, "just past a wrap");
        assert_eq!(expand_seq(65_539, 65_530), 65_530, "reordered from before it");
        assert_eq!(expand_seq(10 * 65_536 + 100, 90), 10 * 65_536 + 90);
        assert_eq!(expand_seq(10 * 65_536 + 100, 40_000), 9 * 65_536 + 40_000, "more than half a window back");
        // Walk a counter through 5 wraps, receiving every 7th packet.
        let mut top = 0;
        for seq in (0..5 * 65_536u64).step_by(7) {
            let got = expand_seq(top, seq as u16);
            assert_eq!(got, seq);
            top = top.max(got);
        }
    }
}
