//! Packet framing.
//!
//! Every datagram:
//! ```text
//! +--------+------+----------------------------------------------+
//! | crc32  | type | type-specific fields                         |
//! | 4 B    | 1 B  |                                              |
//! +--------+------+----------------------------------------------+
//! crc32 = CRC32(protocol_id_le ++ bytes[4..])   (protocol id is never sent)
//! ```
//!
//! Payload (the hot path, sent every tick):
//! ```text
//! crc32:4 | type:1 | session:4 | seq:2 | ack:2 | ack_bits:4 | ack_delay:2 | messages...   (19 B overhead)
//! message := kind:1 [reliable_id:2 if kind==reliable] len:varlen(1-2) bytes
//! ```
//!
//! Handshake:
//! ```text
//! C->S ConnectionRequest { salt }            padded to 256 B
//! S->C Challenge         { salt, cookie }    21 B  (server keeps NO state)
//! C->S ChallengeResponse { salt, cookie }    padded to 256 B
//! S->C Accepted          { salt, client_id } (server allocates the slot now)
//! ```
//! `cookie = keyed_hash(server_secret, client_addr, salt, time_bucket)`, so only a
//! client that actually receives packets at its claimed address can finish the handshake.

use crate::wire::{crc32, DecodeError, Reader, Writer};

pub const MAX_PACKET_SIZE: usize = 1200;
/// Client->server handshake packets are padded to this size, and the server's
/// replies are much smaller, so the handshake can't be abused for reflection/amplification.
pub const HANDSHAKE_PADDED_SIZE: usize = 256;
/// crc32(4) + type(1) + session(4) + seq(2) + ack(2) + ack_bits(4) + ack_delay(2)
pub const PAYLOAD_OVERHEAD: usize = 19;
/// `AckHeader::ack_delay` unit, in microseconds.
pub const ACK_DELAY_UNIT_US: u64 = 10;

const T_REQUEST: u8 = 1;
const T_CHALLENGE: u8 = 2;
const T_RESPONSE: u8 = 3;
const T_ACCEPTED: u8 = 4;
const T_DENIED: u8 = 5;
const T_PAYLOAD: u8 = 6;
const T_DISCONNECT: u8 = 7;

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

#[derive(Debug, PartialEq, Eq)]
pub enum Packet<'a> {
    ConnectionRequest { client_salt: u64 },
    Challenge { client_salt: u64, cookie: u64 },
    ChallengeResponse { client_salt: u64, cookie: u64 },
    Accepted { client_salt: u64, client_id: u32 },
    Denied { client_salt: u64, reason: DenyReason },
    Payload { session: u32, header: AckHeader, body: &'a [u8] },
    Disconnect { session: u32 },
}

/// Both sides derive the per-connection session tag from the cookie. It's carried
/// in every payload, so an off-path attacker spoofing the client's IP also has to
/// guess 32 bits. (Real protection comes with encryption; see README.)
pub fn session_from_cookie(cookie: u64) -> u32 {
    (cookie ^ (cookie >> 32)) as u32
}

pub fn encode(protocol_id: u64, packet: &Packet<'_>) -> Vec<u8> {
    let cap = match packet {
        Packet::Payload { body, .. } => PAYLOAD_OVERHEAD + body.len(),
        _ => HANDSHAKE_PADDED_SIZE,
    };
    let mut w = Writer::with_capacity(cap);
    w.u32(0); // crc placeholder
    match *packet {
        Packet::ConnectionRequest { client_salt } => {
            w.u8(T_REQUEST);
            w.u64(client_salt);
            w.pad_to(HANDSHAKE_PADDED_SIZE);
        }
        Packet::Challenge { client_salt, cookie } => {
            w.u8(T_CHALLENGE);
            w.u64(client_salt);
            w.u64(cookie);
        }
        Packet::ChallengeResponse { client_salt, cookie } => {
            w.u8(T_RESPONSE);
            w.u64(client_salt);
            w.u64(cookie);
            w.pad_to(HANDSHAKE_PADDED_SIZE);
        }
        Packet::Accepted { client_salt, client_id } => {
            w.u8(T_ACCEPTED);
            w.u64(client_salt);
            w.u32(client_id);
        }
        Packet::Denied { client_salt, reason } => {
            w.u8(T_DENIED);
            w.u64(client_salt);
            w.u8(reason as u8);
        }
        Packet::Payload { session, header, body } => {
            w.u8(T_PAYLOAD);
            w.u32(session);
            w.u16(header.seq);
            w.u16(header.ack);
            w.u32(header.ack_bits);
            w.u16(header.ack_delay);
            w.bytes(body);
        }
        Packet::Disconnect { session } => {
            w.u8(T_DISCONNECT);
            w.u32(session);
        }
    }
    let mut buf = w.into_inner();
    debug_assert!(buf.len() <= MAX_PACKET_SIZE);
    let crc = crc32(&protocol_id.to_le_bytes(), &buf[4..]);
    buf[..4].copy_from_slice(&crc.to_le_bytes());
    buf
}

pub fn decode(protocol_id: u64, data: &[u8]) -> Result<Packet<'_>, DecodeError> {
    if data.len() < 5 || data.len() > MAX_PACKET_SIZE {
        return Err(DecodeError::Invalid);
    }
    let crc = u32::from_le_bytes(data[..4].try_into().unwrap());
    if crc != crc32(&protocol_id.to_le_bytes(), &data[4..]) {
        return Err(DecodeError::BadChecksum);
    }
    let padded = data.len() == HANDSHAKE_PADDED_SIZE;
    let mut r = Reader::new(&data[4..]);
    let packet = match r.u8()? {
        T_REQUEST if padded => Packet::ConnectionRequest { client_salt: r.u64()? },
        T_RESPONSE if padded => Packet::ChallengeResponse {
            client_salt: r.u64()?,
            cookie: r.u64()?,
        },
        T_CHALLENGE => {
            let p = Packet::Challenge { client_salt: r.u64()?, cookie: r.u64()? };
            r.finish()?;
            p
        }
        T_ACCEPTED => {
            let p = Packet::Accepted { client_salt: r.u64()?, client_id: r.u32()? };
            r.finish()?;
            p
        }
        T_DENIED => {
            let client_salt = r.u64()?;
            let reason = match r.u8()? {
                1 => DenyReason::ServerFull,
                _ => return Err(DecodeError::Invalid),
            };
            r.finish()?;
            Packet::Denied { client_salt, reason }
        }
        T_PAYLOAD => {
            let session = r.u32()?;
            let header = AckHeader { seq: r.u16()?, ack: r.u16()?, ack_bits: r.u32()?, ack_delay: r.u16()? };
            Packet::Payload { session, header, body: r.rest() }
        }
        T_DISCONNECT => {
            let p = Packet::Disconnect { session: r.u32()? };
            r.finish()?;
            p
        }
        // includes unpadded requests/responses
        _ => return Err(DecodeError::Invalid),
    };
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    const PID: u64 = 0xABCD;

    #[test]
    fn roundtrip_all() {
        let body = [1u8, 2, 3];
        let pkts = [
            Packet::ConnectionRequest { client_salt: 7 },
            Packet::Challenge { client_salt: 7, cookie: 99 },
            Packet::ChallengeResponse { client_salt: 7, cookie: 99 },
            Packet::Accepted { client_salt: 7, client_id: 3 },
            Packet::Denied { client_salt: 7, reason: DenyReason::ServerFull },
            Packet::Payload {
                session: 5,
                header: AckHeader { seq: 1, ack: 2, ack_bits: 0xF0F0, ack_delay: 777 },
                body: &body,
            },
            Packet::Disconnect { session: 5 },
        ];
        for p in &pkts {
            let bytes = encode(PID, p);
            assert_eq!(&decode(PID, &bytes).unwrap(), p);
        }
    }

    #[test]
    fn handshake_is_not_an_amplifier() {
        let req = encode(PID, &Packet::ConnectionRequest { client_salt: 1 });
        let chal = encode(PID, &Packet::Challenge { client_salt: 1, cookie: 2 });
        assert!(req.len() > chal.len() * 10);
    }

    #[test]
    fn rejects_corruption_wrong_protocol_and_unpadded() {
        let mut bytes = encode(PID, &Packet::Disconnect { session: 1 });
        assert_eq!(decode(PID + 1, &bytes), Err(DecodeError::BadChecksum));
        bytes[6] ^= 0x40;
        assert_eq!(decode(PID, &bytes), Err(DecodeError::BadChecksum));

        // an unpadded ConnectionRequest must be rejected even with a valid crc
        let mut w = Writer::default();
        w.u32(0);
        w.u8(T_REQUEST);
        w.u64(1);
        let mut b = w.into_inner();
        let crc = crc32(&PID.to_le_bytes(), &b[4..]);
        b[..4].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(decode(PID, &b), Err(DecodeError::Invalid));
    }
}
