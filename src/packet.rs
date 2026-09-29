//! Packet framing.
//!
//! Every datagram starts with a type byte. There's no checksum: payload,
//! disconnect and accept packets are sealed (ChaCha20-Poly1305, `crypto.rs`),
//! whose tag authenticates them, protocol id included; the client's request and
//! response carry a connect token the server authenticates (`token.rs`).
//!
//! Payload and disconnect (the hot path):
//! ```text
//! type:1 | seq:2 | sealed( ack:2 | ack_bits:4 | ack_delay:2 | messages... ) | tag:16   (27 B overhead)
//! type:1 | seq:2 | tag:16                                                              (disconnect)
//! message := kind:1 [reliable_id:2 if kind==reliable] len:varlen(1-2) bytes
//! ```
//! `seq` is the low 16 bits of the sender's 64-bit packet counter, which is
//! the nonce; the receiver rebuilds the rest (`crypto::expand_seq`).
//!
//! Handshake:
//! ```text
//! C->S Request   { server_id:8 expires:8 private:144 salt:8 }                    padded to 256 B
//! S->C Challenge { salt:8 cookie:8 }                                             17 B  (server keeps NO state)
//! C->S Response  { server_id:8 expires:8 private:144 salt:8 cookie:8 tag:16 }    padded to 256 B
//! S->C Accepted  { salt:8 sealed(client_id:4) tag:16 }                           29 B  (server allocates the slot now)
//! S->C Denied    { salt:8 reason:1 }
//! ```
//! - The token's private part only opens with the key the server shares with
//!   the login service, and tells the server the connection's keys.
//! - `cookie = keyed_hash(server_secret, client_addr, salt, time_bucket)`, so only
//!   a client that receives packets at its claimed address can finish.
//! - The response's tag (client->server key, empty message) proves the sender
//!   holds the token's keys: a token copied off the wire can't be used.
//! - Requests and responses are padded bigger than any reply, so the handshake
//!   can't be used for reflection or amplification.

use crate::crypto::{Cipher, DOMAIN_ACCEPTED, DOMAIN_RESPONSE, TAG_BYTES};
use crate::token::{ConnectToken, PRIVATE_TOKEN_BYTES};
use crate::wire::{DecodeError, Reader, Writer};

pub const MAX_PACKET_SIZE: usize = 1200;
/// Client->server handshake packets are padded to this size, and the server's
/// replies are much smaller, so the handshake can't be abused for reflection/amplification.
pub const HANDSHAKE_PADDED_SIZE: usize = 256;
/// type(1) + seq(2) + ack(2) + ack_bits(4) + ack_delay(2) + tag(16)
pub const PAYLOAD_OVERHEAD: usize = 27;
/// Clear bytes before a payload's or disconnect's sealed part: type and seq.
pub(crate) const SEALED_PREFIX: usize = 3;
/// `AckHeader::ack_delay` unit, in microseconds.
pub const ACK_DELAY_UNIT_US: u64 = 10;

pub(crate) const T_REQUEST: u8 = 1;
pub(crate) const T_CHALLENGE: u8 = 2;
pub(crate) const T_RESPONSE: u8 = 3;
pub(crate) const T_ACCEPTED: u8 = 4;
pub(crate) const T_DENIED: u8 = 5;
pub(crate) const T_PAYLOAD: u8 = 6;
pub(crate) const T_DISCONNECT: u8 = 7;

/// Request and response share this layout up to the salt.
const TOKEN_FIELDS: usize = 8 + 8 + PRIVATE_TOKEN_BYTES;
/// Everything in a response before its tag.
const RESPONSE_PREFIX: usize = 1 + TOKEN_FIELDS + 8 + 8;
const CHALLENGE_SIZE: usize = 1 + 8 + 8;
const DENIED_SIZE: usize = 1 + 8 + 1;
const ACCEPTED_PREFIX: usize = 1 + 8;
const ACCEPTED_SIZE: usize = ACCEPTED_PREFIX + 4 + TAG_BYTES;
const _: () = assert!(RESPONSE_PREFIX + TAG_BYTES <= HANDSHAKE_PADDED_SIZE);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DenyReason {
    ServerFull = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckHeader {
    pub seq: u16,
    /// Most recent packet sequence received from the peer.
    pub ack: u16,
    /// Bit i set => packet `ack - 1 - i` was also received.
    pub ack_bits: u32,
    /// How long packet `ack` waited at this end before this packet carried its
    /// ack, in `ACK_DELAY_UNIT_US` (saturating, ~655 ms). The peer subtracts it
    /// from its RTT sample, so the RTT measures the network, not our tick rate.
    pub ack_delay: u16,
}

/// The token fields a request or response carries in the clear.
#[derive(Debug, PartialEq, Eq)]
pub struct TokenRef<'a> {
    pub server_id: u64,
    pub expires: u64,
    pub private: &'a [u8; PRIVATE_TOKEN_BYTES],
}

/// A handshake packet, parsed but (for Response and Accepted) not yet authenticated.
#[derive(Debug, PartialEq, Eq)]
pub enum Handshake<'a> {
    Request { token: TokenRef<'a>, salt: u64 },
    Challenge { salt: u64, cookie: u64 },
    /// Authenticate with `verify_response` once the token has given the key.
    Response { token: TokenRef<'a>, salt: u64, cookie: u64 },
    /// Open with `open_accepted`.
    Accepted { salt: u64 },
    Denied { salt: u64, reason: DenyReason },
}

fn write_token(w: &mut Writer, t: &ConnectToken) {
    w.u64(t.server_id);
    w.u64(t.expires);
    w.bytes(&t.private);
}

fn read_token<'a>(r: &mut Reader<'a>) -> Result<TokenRef<'a>, DecodeError> {
    Ok(TokenRef { server_id: r.u64()?, expires: r.u64()?, private: r.take(PRIVATE_TOKEN_BYTES)?.try_into().unwrap() })
}

pub(crate) fn encode_request(token: &ConnectToken, salt: u64) -> Vec<u8> {
    let mut w = Writer::with_capacity(HANDSHAKE_PADDED_SIZE);
    w.u8(T_REQUEST);
    write_token(&mut w, token);
    w.u64(salt);
    w.pad_to(HANDSHAKE_PADDED_SIZE);
    w.into_inner()
}

pub(crate) fn encode_challenge(salt: u64, cookie: u64) -> Vec<u8> {
    let mut w = Writer::with_capacity(CHALLENGE_SIZE);
    w.u8(T_CHALLENGE);
    w.u64(salt);
    w.u64(cookie);
    w.into_inner()
}

/// Sealed with the client->server key: proves the sender holds the token's keys.
pub(crate) fn encode_response(protocol_id: u64, token: &ConnectToken, salt: u64, cookie: u64) -> Vec<u8> {
    let mut w = Writer::with_capacity(HANDSHAKE_PADDED_SIZE);
    w.u8(T_RESPONSE);
    write_token(&mut w, token);
    w.u64(salt);
    w.u64(cookie);
    let mut pkt = w.into_inner();
    let prefix = pkt.len();
    Cipher::new(&token.client_to_server_key).seal(protocol_id, DOMAIN_RESPONSE, cookie, &mut pkt, prefix);
    pkt.resize(HANDSHAKE_PADDED_SIZE, 0);
    pkt
}

/// Checks a response's tag against the client->server key from its token.
pub(crate) fn verify_response(protocol_id: u64, c2s: &Cipher, data: &[u8], cookie: u64) -> bool {
    let Some(sealed) = data.get(..RESPONSE_PREFIX + TAG_BYTES) else { return false };
    c2s.open(protocol_id, DOMAIN_RESPONSE, cookie, sealed, RESPONSE_PREFIX, &mut []) == Some(0)
}

/// Sealed with the server->client key, so a client only believes its server.
pub(crate) fn encode_accepted(protocol_id: u64, s2c: &Cipher, salt: u64, client_id: u32) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(ACCEPTED_SIZE);
    pkt.push(T_ACCEPTED);
    pkt.extend_from_slice(&salt.to_le_bytes());
    pkt.extend_from_slice(&client_id.to_le_bytes());
    s2c.seal(protocol_id, DOMAIN_ACCEPTED, salt, &mut pkt, ACCEPTED_PREFIX);
    pkt
}

/// The client id in an Accepted, if it was sealed by our server for our salt.
pub(crate) fn open_accepted(protocol_id: u64, s2c: &Cipher, data: &[u8], salt: u64) -> Option<u32> {
    let mut id = [0; 4];
    (s2c.open(protocol_id, DOMAIN_ACCEPTED, salt, data, ACCEPTED_PREFIX, &mut id)? == 4).then(|| u32::from_le_bytes(id))
}

pub(crate) fn encode_denied(salt: u64, reason: DenyReason) -> Vec<u8> {
    let mut w = Writer::with_capacity(DENIED_SIZE);
    w.u8(T_DENIED);
    w.u64(salt);
    w.u8(reason as u8);
    w.into_inner()
}

/// Parses a handshake packet. Every type has an exact size, so a payload or
/// junk of another size never parses as one.
pub fn decode_handshake(data: &[u8]) -> Result<Handshake<'_>, DecodeError> {
    let (&ty, _) = data.split_first().ok_or(DecodeError::Eof)?;
    let size_ok = match ty {
        T_REQUEST | T_RESPONSE => data.len() == HANDSHAKE_PADDED_SIZE,
        T_CHALLENGE => data.len() == CHALLENGE_SIZE,
        T_ACCEPTED => data.len() == ACCEPTED_SIZE,
        T_DENIED => data.len() == DENIED_SIZE,
        _ => false,
    };
    if !size_ok {
        return Err(DecodeError::Invalid);
    }
    let mut r = Reader::new(&data[1..]);
    Ok(match ty {
        T_REQUEST => Handshake::Request { token: read_token(&mut r)?, salt: r.u64()? },
        T_RESPONSE => Handshake::Response { token: read_token(&mut r)?, salt: r.u64()?, cookie: r.u64()? },
        T_CHALLENGE => Handshake::Challenge { salt: r.u64()?, cookie: r.u64()? },
        T_ACCEPTED => Handshake::Accepted { salt: r.u64()? },
        _ => Handshake::Denied {
            salt: r.u64()?,
            reason: match r.u8()? {
                1 => DenyReason::ServerFull,
                _ => return Err(DecodeError::Invalid),
            },
        },
    })
}

/// A payload's clear prefix and plaintext header, ready for its messages; seal
/// it with `Cipher::seal(.., SEALED_PREFIX)`.
pub(crate) fn begin_payload(h: &AckHeader, body_len: usize) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(PAYLOAD_OVERHEAD + body_len);
    pkt.push(T_PAYLOAD);
    pkt.extend_from_slice(&h.seq.to_le_bytes());
    pkt.extend_from_slice(&h.ack.to_le_bytes());
    pkt.extend_from_slice(&h.ack_bits.to_le_bytes());
    pkt.extend_from_slice(&h.ack_delay.to_le_bytes());
    pkt
}

/// Splits an opened payload into its ack header and messages.
pub(crate) fn read_payload(seq: u16, plain: &[u8]) -> Result<(AckHeader, &[u8]), DecodeError> {
    let mut r = Reader::new(plain);
    let header = AckHeader { seq, ack: r.u16()?, ack_bits: r.u32()?, ack_delay: r.u16()? };
    Ok((header, r.rest()))
}

/// Type and 16-bit seq of a payload or disconnect, before it's opened.
pub(crate) fn sealed_prefix(data: &[u8]) -> Option<(u8, u16)> {
    match data {
        [ty @ (T_PAYLOAD | T_DISCONNECT), a, b, ..] if data.len() <= MAX_PACKET_SIZE => {
            Some((*ty, u16::from_le_bytes([*a, *b])))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::USER_DATA_BYTES;

    const PID: u64 = 0xABCD;

    fn token() -> ConnectToken {
        ConnectToken::mint(&[5; 32], PID, 1, u64::MAX, 7, &[0; USER_DATA_BYTES])
    }

    #[test]
    fn handshake_roundtrip_and_sizes() {
        let t = token();
        let req = encode_request(&t, 11);
        assert_eq!(req.len(), HANDSHAKE_PADDED_SIZE);
        let tref = TokenRef { server_id: 1, expires: u64::MAX, private: &t.private };
        assert_eq!(decode_handshake(&req), Ok(Handshake::Request { token: tref, salt: 11 }));
        assert_eq!(decode_handshake(&encode_challenge(11, 99)), Ok(Handshake::Challenge { salt: 11, cookie: 99 }));
        assert_eq!(
            decode_handshake(&encode_denied(11, DenyReason::ServerFull)),
            Ok(Handshake::Denied { salt: 11, reason: DenyReason::ServerFull })
        );

        let resp = encode_response(PID, &t, 11, 99);
        assert_eq!(resp.len(), HANDSHAKE_PADDED_SIZE);
        assert!(matches!(decode_handshake(&resp), Ok(Handshake::Response { salt: 11, cookie: 99, .. })));
        let c2s = Cipher::new(&t.client_to_server_key);
        assert!(verify_response(PID, &c2s, &resp, 99));

        let s2c = Cipher::new(&t.server_to_client_key);
        let acc = encode_accepted(PID, &s2c, 11, 42);
        assert_eq!(decode_handshake(&acc), Ok(Handshake::Accepted { salt: 11 }));
        assert_eq!(open_accepted(PID, &s2c, &acc, 11), Some(42));
    }

    #[test]
    fn handshake_is_not_an_amplifier() {
        let t = token();
        let req = encode_request(&t, 1).len();
        for reply in [encode_challenge(1, 2).len(), encode_denied(1, DenyReason::ServerFull).len()] {
            assert!(req > reply * 8, "{req} vs {reply}");
        }
        let accepted = encode_accepted(PID, &Cipher::new(&[1; 32]), 1, 2).len();
        assert!(encode_response(PID, &t, 1, 2).len() > accepted * 8);
    }

    #[test]
    fn responses_prove_the_keys_and_accepts_prove_the_server() {
        let t = token();
        let resp = encode_response(PID, &t, 11, 99);
        let c2s = Cipher::new(&t.client_to_server_key);
        assert!(!verify_response(PID, &c2s, &resp, 98), "another cookie");
        assert!(!verify_response(PID + 1, &c2s, &resp, 99), "another protocol");
        // Someone with the token bytes but not its keys.
        let mut forged = t.clone();
        forged.client_to_server_key = [0; 32];
        assert!(!verify_response(PID, &c2s, &encode_response(PID, &forged, 11, 99), 99));
        let mut bad = resp.clone();
        bad[20] ^= 1; // inside the private token
        assert!(!verify_response(PID, &c2s, &bad, 99));

        let s2c = Cipher::new(&t.server_to_client_key);
        let acc = encode_accepted(PID, &s2c, 11, 42);
        assert_eq!(open_accepted(PID, &s2c, &acc, 12), None, "another salt");
        assert_eq!(open_accepted(PID, &Cipher::new(&[0; 32]), &acc, 11), None, "another key");
    }

    #[test]
    fn junk_and_wrong_sizes_are_rejected() {
        assert_eq!(decode_handshake(&[]), Err(DecodeError::Eof));
        assert_eq!(decode_handshake(&[T_REQUEST; 100]), Err(DecodeError::Invalid), "unpadded request");
        assert_eq!(decode_handshake(&[T_CHALLENGE; 18]), Err(DecodeError::Invalid));
        assert_eq!(decode_handshake(&[T_PAYLOAD; 40]), Err(DecodeError::Invalid), "not a handshake");
        assert_eq!(decode_handshake(&[99; 17]), Err(DecodeError::Invalid));
        let mut denied = encode_denied(1, DenyReason::ServerFull);
        denied[9] = 7;
        assert_eq!(decode_handshake(&denied), Err(DecodeError::Invalid), "unknown reason");
        assert_eq!(sealed_prefix(&[T_PAYLOAD, 1]), None);
        assert_eq!(sealed_prefix(&[T_DISCONNECT, 1, 2]), Some((T_DISCONNECT, 0x0201)));
        assert_eq!(sealed_prefix(&[T_CHALLENGE, 1, 2]), None);
    }
}
