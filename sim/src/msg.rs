//! Game messages carried in lattice-net channels. Each starts with a tag byte.
//!
//! ```text
//! C->S unreliable  Input    tag | newest_seq:4 | n:1 | n × (move_x:1 move_y:1 yaw:2 buttons:1)   newest first
//! S->C reliable    Welcome  tag | entity:2 | spawn:2×f32 | anchor:2×f32 | radius:f32
//! S->C unreliable  Snapshot tag | server_tick:4 | ack_seq:4 | buffered:1 | wait:2 | own pos:2×f32 vel:2×f32 | n:1 | n × blob:11
//! blob := entity:2 | cell:1 (cx | cy<<4, 512 m cells) | 8 B bitpacked far-tier state (see bitpack.rs)
//! ```
//!
//! Inputs are sent 3× redundantly, so one lost packet never starves the server.
//! The snapshot's own state is full-precision f32: the bot compares it bit-exactly
//! with its prediction for `ack_seq`.

use lattice_net::bitpack::{dequantize, dequantize_angle, quantize, BitReader, BitWriter};
use lattice_net::wire::{DecodeError, Reader, Writer};

use crate::movement::{Input, MoveState, WORLD_SIZE};

pub const MSG_INPUT: u8 = 1;
pub const MSG_WELCOME: u8 = 2;
pub const MSG_SNAPSHOT: u8 = 3;

pub const INPUT_REDUNDANCY: usize = 3;
pub const ENTITY_BLOB: usize = 11;
pub const SNAPSHOT_HEADER: usize = 1 + 4 + 4 + 1 + 2 + 16 + 1;
/// `SnapshotHeader::wait` when `ack_seq` was consumed by a stand-in, not a real input.
pub const WAIT_STAND_IN: u16 = u16::MAX;
const BLOB_CELL: f32 = 512.0;
const BLOB_CELLS: u32 = (WORLD_SIZE / BLOB_CELL) as u32;
const _: () = assert!(BLOB_CELLS <= 16, "cell index must fit in 4 bits per axis");

pub type Blob = [u8; ENTITY_BLOB];

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
    /// The client speeds its input clock up or down to keep this at 2-3
    /// (the input due now plus 1-2 spare).
    pub buffered: u8,
    /// How long input `ack_seq` waited on the server, from its datagram
    /// arriving to being applied, in 0.1 ms. `WAIT_STAND_IN` if it never
    /// arrived in time. The client adds half the RTT to get input -> applied.
    pub wait: u16,
    pub own: MoveState,
    pub count: u8,
}

pub fn write_snapshot_header(w: &mut Writer, h: &SnapshotHeader) {
    w.u8(MSG_SNAPSHOT);
    w.u32(h.server_tick);
    w.u32(h.ack_seq);
    w.u8(h.buffered);
    w.u16(h.wait);
    for v in [h.own.pos[0], h.own.pos[1], h.own.vel[0], h.own.vel[1]] {
        w.u32(v.to_bits());
    }
    w.u8(h.count);
}

#[derive(Debug)]
pub enum ServerMsg<'a> {
    Welcome(Welcome),
    Snapshot(SnapshotHeader, &'a [u8]),
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
            let own = MoveState { pos: [read_f32(&mut r)?, read_f32(&mut r)?], vel: [read_f32(&mut r)?, read_f32(&mut r)?] };
            let count = r.u8()?;
            let blobs = r.take(count as usize * ENTITY_BLOB)?;
            r.finish()?;
            Ok(ServerMsg::Snapshot(SnapshotHeader { server_tick, ack_seq, buffered, wait, own, count }, blobs))
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
    let cell = |v: f32| ((v / BLOB_CELL) as u32).min(BLOB_CELLS - 1);
    let (cx, cy) = (cell(pos[0]), cell(pos[1]));
    let mut bw = BitWriter::new();
    bw.write(quantize(pos[0] - cx as f32 * BLOB_CELL, 0.0, BLOB_CELL, 15), 15);
    bw.write(quantize(pos[1] - cy as f32 * BLOB_CELL, 0.0, BLOB_CELL, 15), 15);
    bw.write(0, 12); // altitude: flat world for now
    bw.write(yaw as u32 >> 7, 9);
    bw.write(32, 6); // pitch: level
    bw.write(0, 3); // stance / seat flags
    bw.write(15, 4); // health bucket: full
    let bits = bw.finish();

    let mut b = [0u8; ENTITY_BLOB];
    b[..2].copy_from_slice(&entity.to_le_bytes());
    b[2] = (cx | cy << 4) as u8;
    b[3..].copy_from_slice(&bits);
    b
}

/// Returns (entity, pos, yaw radians).
pub fn decode_blob(b: &[u8]) -> Result<(u16, [f32; 2], f32), DecodeError> {
    if b.len() != ENTITY_BLOB {
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
    fn welcome_and_snapshot_roundtrip() {
        let w = Welcome { entity: 7, spawn: [1.5, 2.5], anchor: [4096.0, 4096.0], radius: 200.0 };
        assert!(matches!(decode_server_msg(&encode_welcome(&w)), Ok(ServerMsg::Welcome(x)) if x == w));

        let h = SnapshotHeader {
            server_tick: 99,
            ack_seq: 42,
            buffered: 2,
            wait: 333,
            own: MoveState { pos: [1.0, 2.0], vel: [-3.0, 0.125] },
            count: 2,
        };
        let mut wr = Writer::default();
        write_snapshot_header(&mut wr, &h);
        assert_eq!(wr.len(), SNAPSHOT_HEADER);
        wr.bytes(&encode_blob(1, [10.0, 20.0], 0));
        wr.bytes(&encode_blob(2, [30.0, 40.0], 0));
        let ServerMsg::Snapshot(got, blobs) = decode_server_msg(wr.as_slice()).unwrap() else { panic!() };
        assert_eq!(got, h);
        assert_eq!(blobs.len(), 2 * ENTITY_BLOB);
        assert!(decode_server_msg(&wr.as_slice()[..wr.len() - 1]).is_err());
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
