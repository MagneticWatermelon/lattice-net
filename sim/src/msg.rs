//! Game messages carried in lattice-net channels. Each starts with a tag byte.
//!
//! ```text
//! C->S unreliable  Input    tag | newest_seq:4 | n:1 | n × (move_x:1 move_y:1 yaw:2 buttons:1)   newest first
//! S->C reliable    Welcome  tag | entity:2 | spawn:2×f32 | anchor:2×f32 | radius:f32
//! S->C unreliable  Snapshot tag | server_tick:4 | ack_seq:4 | buffered:1 | wait:2 | pace:2 | level:1 | client_level:1 | own pos:2×f32 vel:2×f32
//! S->C unreliable  Near     see delta.rs: deltas against acked baselines, one per tick, tagged
//! S->C unreliable  Entities tag | server_tick:4 | tier:1 | n:1 | n × blob         (mid and far; one or more per tier per tick)
//! far blob  (11 B) := entity:2 | cell:1 | 8 B bitpacked far-tier state (see bitpack.rs); used for mid and far
//! cell := cx | cy<<4, the entity's own 512 m cell, so a blob is the same for every recipient
//! ```
//!
//! Inputs are sent 3× redundantly, so one lost packet never starves the server.
//! The snapshot's own state is full-precision f32: the bot compares it bit-exactly
//! with its prediction for `ack_seq`. Entity messages carry one tier each and
//! never exceed one packet (there's no fragmentation yet), so a tier with more
//! blobs than fit is split over several messages. The first one is sized to
//! fill what's left of the current packet (`PacketFill`), so every packet but
//! a client's last leaves the server nearly full: GSO pads them to equal size.

use lattice_net::bitpack::{dequantize, dequantize_angle, quantize, BitReader, BitWriter};
use lattice_net::wire::{DecodeError, Reader, Writer};
use lattice_net::Config;

use crate::interest::Tier;
use crate::movement::{Input, MoveState, WORLD_SIZE};

pub const MSG_INPUT: u8 = 1;
pub const MSG_WELCOME: u8 = 2;
pub const MSG_SNAPSHOT: u8 = 3;
pub const MSG_ENTITIES: u8 = 4;

pub const INPUT_REDUNDANCY: usize = 3;
/// Mid- and far-tier blob.
pub const FAR_BLOB: usize = 11;
pub const SNAPSHOT_LEN: usize = 1 + 4 + 4 + 1 + 2 + 2 + 1 + 1 + 16;
pub const ENTITIES_HEADER: usize = 1 + 4 + 1 + 1;

/// `SnapshotHeader::wait` when `ack_seq` was consumed by a stand-in, not a real input.
pub const WAIT_STAND_IN: u16 = u16::MAX;
const BLOB_CELL: f32 = 512.0;
const BLOB_CELLS: u32 = (WORLD_SIZE / BLOB_CELL) as u32;
const _: () = assert!(BLOB_CELLS <= 16, "cell index must fit in 4 bits per axis");

pub type Blob = [u8; FAR_BLOB];
/// Bytes per blob in an Entities message (mid and far; near has its own message).
pub fn blob_size(_tier: Tier) -> usize {
    FAR_BLOB
}

/// Most blobs of `tier` one Entities message of at most `max_message` bytes holds.
pub fn blobs_per_message(tier: Tier, max_message: usize) -> usize {
    ((max_message - ENTITIES_HEADER) / blob_size(tier)).min(u8::MAX as usize)
}

/// Predicts how the transport packs one client's unreliable messages (in
/// order, greedily, each packet holding `body` bytes), so a tier's blobs can be
/// split to fill the current packet instead of leaving it part-empty.
/// Reliable messages go first in the real packing; they are rare here, and
/// when present only cost some fill, never correctness.
#[derive(Debug, Clone, Copy)]
pub struct PacketFill {
    body: usize,
    used: usize,
}

impl PacketFill {
    pub fn new(body: usize) -> Self {
        Self { body, used: 0 }
    }

    /// Account for an unreliable message of `len` bytes.
    pub fn push(&mut self, len: usize) {
        let w = Config::unreliable_wire_size(len);
        self.used = if self.used + w > self.body { w } else { self.used + w };
    }

    /// Blobs in the next Entities message, given `left` to write: as many as
    /// fit in the current packet, or a full message (`per`) if none do.
    pub fn next_chunk(&self, left: usize, size: usize, per: usize) -> usize {
        // framing: kind(1) + varlen(2, worst case)
        let fit = (self.body - self.used).saturating_sub(3 + ENTITIES_HEADER) / size;
        left.min(per).min(if fit > 0 { fit } else { per })
    }

    /// Bytes of `n` blobs written as Entities messages from here, and the fill after.
    pub fn entities(mut self, n: usize, size: usize, per: usize) -> (usize, Self) {
        let (mut left, mut bytes) = (n, 0);
        while left > 0 {
            let k = self.next_chunk(left, size, per);
            let len = ENTITIES_HEADER + k * size;
            self.push(len);
            bytes += len;
            left -= k;
        }
        (bytes, self)
    }
}

pub fn encode_inputs(newest_seq: u32, newest_first: &[Input]) -> Vec<u8> {
    let mut w = Writer::with_capacity(6 + newest_first.len() * 5);
    w.u8(MSG_INPUT);
    w.u32(newest_seq);
    w.u8(newest_first.len() as u8);
    for i in newest_first {
        w.u8(i.move_x as u8);
        w.u8(i.move_y as u8);
        w.u16(i.yaw);
        w.u8(i.buttons);
    }
    w.into_inner()
}

/// Calls `f(seq, input)` for each input in the batch, newest first.
pub fn decode_inputs(data: &[u8], mut f: impl FnMut(u32, Input)) -> Result<(), DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_INPUT {
        return Err(DecodeError::Invalid);
    }
    let newest = r.u32()?;
    let n = r.u8()? as u32;
    if n > newest {
        return Err(DecodeError::Invalid); // seq 0 is never a real input
    }
    for k in 0..n {
        let input = Input { move_x: r.u8()? as i8, move_y: r.u8()? as i8, yaw: r.u16()?, buttons: r.u8()? };
        f(newest - k, input);
    }
    r.finish()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Welcome {
    pub entity: u16,
    pub spawn: [f32; 2],
    /// The bot wanders within `radius` of `anchor` (scenario hotspot/blob center).
    pub anchor: [f32; 2],
    pub radius: f32,
}

pub fn encode_welcome(m: &Welcome) -> Vec<u8> {
    let mut w = Writer::with_capacity(23);
    w.u8(MSG_WELCOME);
    w.u16(m.entity);
    for v in [m.spawn[0], m.spawn[1], m.anchor[0], m.anchor[1], m.radius] {
        w.u32(v.to_bits());
    }
    w.into_inner()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapshotHeader {
    pub server_tick: u32,
    /// Newest input seq the server has consumed (real or stand-in), 0 if none yet.
    pub ack_seq: u32,
    /// Real inputs the server had queued for this client when the tick began.
    /// The client speeds its input clock up or down to keep this at 2
    /// (the input due now plus one spare).
    pub buffered: u8,
    /// How long input `ack_seq` waited on the server, from its datagram
    /// arriving to being applied, in 0.1 ms. `WAIT_STAND_IN` if it never
    /// arrived in time. The client adds half the RTT to get input -> applied.
    pub wait: u16,
    /// Game-seconds per wall-second, per mille: the rate at which the server
    /// consumes inputs, relative to 30 per second. Clients pace their inputs by it.
    pub pace: u16,
    /// Server-wide degradation level (0 = normal), and this client's own
    /// bandwidth level.
    pub level: u8,
    pub client_level: u8,
    pub own: MoveState,
}

pub fn write_snapshot(w: &mut Writer, h: &SnapshotHeader) {
    w.u8(MSG_SNAPSHOT);
    w.u32(h.server_tick);
    w.u32(h.ack_seq);
    w.u8(h.buffered);
    w.u16(h.wait);
    w.u16(h.pace);
    w.u8(h.level);
    w.u8(h.client_level);
    for v in [h.own.pos[0], h.own.pos[1], h.own.vel[0], h.own.vel[1]] {
        w.u32(v.to_bits());
    }
}

/// Starts an Entities message; append exactly `count` blobs of `tier` after it.
pub fn write_entities_header(w: &mut Writer, server_tick: u32, tier: Tier, count: u8) {
    w.u8(MSG_ENTITIES);
    w.u32(server_tick);
    w.u8(tier as u8);
    w.u8(count);
}

#[derive(Debug)]
pub enum ServerMsg<'a> {
    Welcome(Welcome),
    Snapshot(SnapshotHeader),
    /// `blobs` holds whole blobs of `blob_size(tier)` bytes each.
    Entities { server_tick: u32, tier: Tier, blobs: &'a [u8] },
}

pub fn decode_server_msg(data: &[u8]) -> Result<ServerMsg<'_>, DecodeError> {
    let mut r = Reader::new(data);
    match r.u8()? {
        MSG_WELCOME => {
            let entity = r.u16()?;
            let spawn = [read_f32(&mut r)?, read_f32(&mut r)?];
            let anchor = [read_f32(&mut r)?, read_f32(&mut r)?];
            let radius = read_f32(&mut r)?;
            r.finish()?;
            Ok(ServerMsg::Welcome(Welcome { entity, spawn, anchor, radius }))
        }
        MSG_SNAPSHOT => {
            let server_tick = r.u32()?;
            let ack_seq = r.u32()?;
            let buffered = r.u8()?;
            let wait = r.u16()?;
            let (pace, level, client_level) = (r.u16()?, r.u8()?, r.u8()?);
            let own = MoveState { pos: [read_f32(&mut r)?, read_f32(&mut r)?], vel: [read_f32(&mut r)?, read_f32(&mut r)?] };
            r.finish()?;
            Ok(ServerMsg::Snapshot(SnapshotHeader { server_tick, ack_seq, buffered, wait, pace, level, client_level, own }))
        }
        MSG_ENTITIES => {
            let server_tick = r.u32()?;
            let tier = Tier::from_u8(r.u8()?).filter(|&t| t != Tier::Near).ok_or(DecodeError::Invalid)?;
            let count = r.u8()? as usize;
            let blobs = r.take(count * blob_size(tier))?;
            r.finish()?;
            Ok(ServerMsg::Entities { server_tick, tier, blobs })
        }
        _ => Err(DecodeError::Invalid),
    }
}

fn read_f32(r: &mut Reader<'_>) -> Result<f32, DecodeError> {
    r.u32().map(f32::from_bits)
}

/// Far-tier encoding: position relative to the entity's own 512 m cell, so the
/// blob is identical for every recipient and is serialized once per tick.
pub fn encode_blob(entity: u16, pos: [f32; 2], yaw: u16) -> Blob {
    let (cx, cy) = blob_cell(pos);
    let mut bw = BitWriter::new();
    bw.write(quantize(pos[0] - cx as f32 * BLOB_CELL, 0.0, BLOB_CELL, 15), 15);
    bw.write(quantize(pos[1] - cy as f32 * BLOB_CELL, 0.0, BLOB_CELL, 15), 15);
    bw.write(0, 12); // altitude: flat world for now
    bw.write(yaw as u32 >> 7, 9);
    bw.write(32, 6); // pitch: level
    bw.write(0, 3); // stance / seat flags
    bw.write(15, 4); // health bucket: full
    let bits = bw.finish();

    let mut b = [0u8; FAR_BLOB];
    b[..2].copy_from_slice(&entity.to_le_bytes());
    b[2] = (cx | cy << 4) as u8;
    b[3..].copy_from_slice(&bits);
    b
}

fn blob_cell(pos: [f32; 2]) -> (u32, u32) {
    let cell = |v: f32| ((v / BLOB_CELL) as u32).min(BLOB_CELLS - 1);
    (cell(pos[0]), cell(pos[1]))
}

/// Returns (entity, pos, yaw radians).
pub fn decode_blob(b: &[u8]) -> Result<(u16, [f32; 2], f32), DecodeError> {
    if b.len() != FAR_BLOB {
        return Err(DecodeError::Invalid);
    }
    let entity = u16::from_le_bytes([b[0], b[1]]);
    let (cx, cy) = ((b[2] & 0xF) as f32, (b[2] >> 4) as f32);
    let mut r = BitReader::new(&b[3..]);
    let x = cx * BLOB_CELL + dequantize(r.read(15)?, 0.0, BLOB_CELL, 15);
    let y = cy * BLOB_CELL + dequantize(r.read(15)?, 0.0, BLOB_CELL, 15);
    let _z = r.read(12)?;
    let yaw = dequantize_angle(r.read(9)?, 9);
    Ok((entity, [x, y], yaw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_roundtrip() {
        let ins = [
            Input { move_x: -127, move_y: 5, yaw: 40000, buttons: 1 },
            Input { move_x: 3, move_y: 127, yaw: 1, buttons: 0 },
        ];
        let bytes = encode_inputs(10, &ins);
        let mut got = Vec::new();
        decode_inputs(&bytes, |s, i| got.push((s, i))).unwrap();
        assert_eq!(got, vec![(10, ins[0]), (9, ins[1])]);
        // a batch reaching back past seq 1 is malformed
        assert!(decode_inputs(&encode_inputs(1, &ins), |_, _| {}).is_err());
    }

    #[test]
    fn welcome_snapshot_and_entities_roundtrip() {
        let w = Welcome { entity: 7, spawn: [1.5, 2.5], anchor: [4096.0, 4096.0], radius: 200.0 };
        assert!(matches!(decode_server_msg(&encode_welcome(&w)), Ok(ServerMsg::Welcome(x)) if x == w));

        let h = SnapshotHeader {
            server_tick: 99,
            ack_seq: 42,
            buffered: 2,
            wait: 333,
            pace: 800,
            level: 7,
            client_level: 1,
            own: MoveState { pos: [1.0, 2.0], vel: [-3.0, 0.125] },
        };
        let mut wr = Writer::default();
        write_snapshot(&mut wr, &h);
        assert_eq!(wr.len(), SNAPSHOT_LEN);
        assert!(matches!(decode_server_msg(wr.as_slice()), Ok(ServerMsg::Snapshot(x)) if x == h));

        let mut wr = Writer::default();
        write_entities_header(&mut wr, 99, Tier::Mid, 2);
        wr.bytes(&encode_blob(1, [10.0, 20.0], 0));
        wr.bytes(&encode_blob(2, [30.0, 40.0], 0));
        assert_eq!(wr.len(), ENTITIES_HEADER + 2 * FAR_BLOB);
        let Ok(ServerMsg::Entities { server_tick: 99, tier: Tier::Mid, blobs }) = decode_server_msg(wr.as_slice()) else {
            panic!()
        };
        assert_eq!(blobs.len(), 2 * FAR_BLOB);
        assert!(decode_server_msg(&wr.as_slice()[..wr.len() - 1]).is_err(), "truncated");
        assert_eq!(blobs_per_message(Tier::Far, 1176), 106);
        let mut near = Writer::default();
        write_entities_header(&mut near, 99, Tier::Near, 0);
        assert!(decode_server_msg(near.as_slice()).is_err(), "near has its own message");
    }

    #[test]
    fn blob_precision_across_cells() {
        for &(x, y) in &[(0.0, 0.0), (511.99, 512.0), (4096.3, 7000.7), (WORLD_SIZE, WORLD_SIZE)] {
            let yaw = 12345u16;
            let (id, p, a) = decode_blob(&encode_blob(9, [x, y], yaw)).unwrap();
            assert_eq!(id, 9);
            assert!((p[0] - x).abs() < 0.02 && (p[1] - y).abs() < 0.02, "{x},{y} -> {p:?}");
            let want = yaw as f32 / 65536.0 * std::f32::consts::TAU;
            assert!((a - want).abs() < 0.013, "{a} vs {want}");
        }
    }
}
