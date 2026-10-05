//! Game messages carried in lattice-net channels. Each starts with a tag byte.
//!
//! ```text
//! C->S unreliable  Input    tag | newest_seq:4 | n:1 | [mid_lag:1] | n × (move_x:1 move_y:1 yaw:2 pitch:2 buttons:1 [render:2]
//!                           [shot: frac:1 yaw:2 pitch:2 render:2])
//!                           newest first. n's top bit: render times follow. `render` is the client's
//!                           near render step when the input was made (1/64 steps, wrapping; see
//!                           RENDER_UNITS); mid and far entities were drawn `mid_lag` (1/8 steps) earlier.
//!                           BUTTON_FIRE: a shot follows (weapon::Shot), at most one per input
//! S->C reliable    Welcome  tag | entity:2 | spawn:2×f32 | anchor:2×f32 | radius:f32 | world_seed:8
//! S->C unreliable  Snapshot tag | server_tick:4 | step:4 | ack_seq:4 | buffered:1 | wait:2 | pace:2 | level:1 | client_level:1
//!                           | own pos:2×f32 vel:2×f32 z:f32 vz:f32 grounded:1 | pushes:1 | health:1 | life:1
//! S->C unreliable  Near     see delta.rs: deltas against acked baselines, one per tick, tagged
//! S->C unreliable  Entities tag | server_tick:4 | tier:1 | n:1 | n × blob         (mid and far; one or more per tier per tick)
//! far blob  (11 B) := entity:2 | cell:1 | 8 B bitpacked far-tier state (see bitpack.rs); used for mid and far
//!   x:15 y:15 (~16 mm in the cell) | altitude:12 (16 cm) | yaw:9 | pitch:6 | flags:3 (airborne, dead) | health:4 (1/15ths)
//! cell := cx | cy<<4, the entity's own 512 m cell, so a blob is the same for every recipient
//!
//! Time: `server_tick` counts ticks; `step` counts 1/30 s movement steps (game
//! time). They advance together at 30 Hz, but a 20 Hz tick is 1 or 2 steps,
//! so clients render on steps. Every state a tick sends is at its `step`.
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

use crate::tier::Tier;
use crate::faction::MAX_HEALTH;
use crate::movement::{Input, MoveState, BUTTON_FIRE, WORLD_SIZE};
use crate::weapon::Shot;

pub const MSG_INPUT: u8 = 1;
pub const MSG_WELCOME: u8 = 2;
pub const MSG_SNAPSHOT: u8 = 3;
pub const MSG_ENTITIES: u8 = 4;

pub const INPUT_REDUNDANCY: usize = 3;
/// Mid- and far-tier blob.
pub const FAR_BLOB: usize = 11;
pub const SNAPSHOT_LEN: usize = 1 + 4 + 4 + 4 + 1 + 2 + 2 + 1 + 1 + 24 + 1 + 1 + 1 + 1;
/// Render times in inputs are in 1/`RENDER_UNITS` steps, as a wrapping u16:
/// unambiguous within ±512 steps (±17 s) of the server's step.
pub const RENDER_UNITS: f64 = 64.0;
/// `RenderTime::mid_lag` is in 1/`MID_LAG_UNITS` steps (up to ~1 s).
pub const MID_LAG_UNITS: f64 = 8.0;
/// The longest render delays a client may use, in steps: near 133 ms, mid
/// and far 200 ms (the client core's defaults). The server trims a shot's
/// claimed render time to what a client within these could have seen: an
/// older claim is a "backtrack" cheat, not lag.
pub const MAX_NEAR_DELAY: f64 = 4.0;
pub const MAX_MID_DELAY: f64 = 6.0;
const RENDER_FLAG: u8 = 0x80;
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

/// The wire form of a render step (see `RENDER_UNITS`).
pub fn render_units(step: f64) -> u16 {
    (step * RENDER_UNITS).round().rem_euclid(65536.0) as u16
}

/// How far a render time (from `render_units`) lies behind `step`, in steps;
/// negative if it's ahead (a client can't render the future, so it's bogus).
pub fn render_age(step: u32, render: u16) -> f64 {
    let now = (step as u64 * RENDER_UNITS as u64) as u16;
    now.wrapping_sub(render) as i16 as f64 / RENDER_UNITS
}

/// What the player saw when it made an input: near entities at render step
/// `near` (`render_units`), mid and far ones `mid_lag` / `MID_LAG_UNITS`
/// steps before that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTime {
    pub near: u16,
    pub mid_lag: u8,
}

impl RenderTime {
    /// How far behind `step` the near and mid render steps were (`render_age`).
    pub fn ages(&self, step: u32) -> (f64, f64) {
        let near = render_age(step, self.near);
        (near, near + self.mid_lag as f64 / MID_LAG_UNITS)
    }
}

/// An input as a batch carries it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputEntry {
    pub input: Input,
    /// Near render step when it was made (`render_units`).
    pub render: Option<u16>,
    pub shot: Option<Shot>,
}

/// Each input with its near render step (`render_units`) and shot, and the
/// batch's mid lag. Render times are sent only when every input in the batch
/// has one.
pub fn encode_inputs(newest_seq: u32, newest_first: &[InputEntry], mid_lag: u8) -> Vec<u8> {
    let timed = newest_first.iter().all(|e| e.render.is_some());
    let mut w = Writer::with_capacity(7 + newest_first.len() * 16);
    w.u8(MSG_INPUT);
    w.u32(newest_seq);
    w.u8(newest_first.len() as u8 | if timed { RENDER_FLAG } else { 0 });
    if timed {
        w.u8(mid_lag);
    }
    for e in newest_first {
        let i = &e.input;
        w.u8(i.move_x as u8);
        w.u8(i.move_y as u8);
        w.u16(i.yaw);
        w.u16(i.pitch as u16);
        w.u8(i.buttons & !BUTTON_FIRE | if e.shot.is_some() { BUTTON_FIRE } else { 0 });
        if timed {
            w.u16(e.render.unwrap());
        }
        if let Some(s) = e.shot {
            w.u8(s.frac);
            w.u16(s.yaw);
            w.u16(s.pitch as u16);
            w.u16(s.render);
        }
    }
    w.into_inner()
}

/// Calls `f(seq, input, render, shot)` for each input in the batch, newest
/// first. A shot's render time shares the batch's mid lag.
pub fn decode_inputs(data: &[u8], mut f: impl FnMut(u32, Input, Option<RenderTime>, Option<Shot>)) -> Result<(), DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_INPUT {
        return Err(DecodeError::Invalid);
    }
    let newest = r.u32()?;
    let n = r.u8()?;
    let (timed, n) = (n & RENDER_FLAG != 0, (n & !RENDER_FLAG) as u32);
    if n > newest {
        return Err(DecodeError::Invalid); // seq 0 is never a real input
    }
    let mid_lag = if timed { r.u8()? } else { 0 };
    for k in 0..n {
        let mut input =
            Input { move_x: r.u8()? as i8, move_y: r.u8()? as i8, yaw: r.u16()?, pitch: r.u16()? as i16, buttons: r.u8()? };
        let render = if timed { Some(RenderTime { near: r.u16()?, mid_lag }) } else { None };
        let shot = if input.buttons & BUTTON_FIRE != 0 {
            input.buttons &= !BUTTON_FIRE;
            Some(Shot { frac: r.u8()?, yaw: r.u16()?, pitch: r.u16()? as i16, render: r.u16()? })
        } else {
            None
        };
        f(newest - k, input, render, shot);
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
    /// The client builds the same world from it (`World::shared`).
    pub world_seed: u64,
}

pub fn encode_welcome(m: &Welcome) -> Vec<u8> {
    let mut w = Writer::with_capacity(31);
    w.u8(MSG_WELCOME);
    w.u16(m.entity);
    for v in [m.spawn[0], m.spawn[1], m.anchor[0], m.anchor[1], m.radius] {
        w.u32(v.to_bits());
    }
    w.u64(m.world_seed);
    w.into_inner()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapshotHeader {
    pub server_tick: u32,
    /// Game time of this tick's states, in 1/30 s movement steps.
    pub step: u32,
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
    /// Counts (wrapping) the ticks the server pushed this player apart from
    /// a crowd. A prediction miss when it changed is a push, not a bug.
    pub pushes: u8,
    /// Own health; 0 is dead, and a dead player's inputs move nothing
    /// (`movement::dead_input`).
    pub health: u8,
    /// Counts (wrapping) deaths and respawns. When it changes, the client
    /// rebases and replays under the new rule; a respawn also teleports.
    pub life: u8,
}

pub fn write_snapshot(w: &mut Writer, h: &SnapshotHeader) {
    w.u8(MSG_SNAPSHOT);
    w.u32(h.server_tick);
    w.u32(h.step);
    w.u32(h.ack_seq);
    w.u8(h.buffered);
    w.u16(h.wait);
    w.u16(h.pace);
    w.u8(h.level);
    w.u8(h.client_level);
    for v in [h.own.pos[0], h.own.pos[1], h.own.vel[0], h.own.vel[1], h.own.z, h.own.vz] {
        w.u32(v.to_bits());
    }
    w.u8(h.own.grounded as u8);
    w.u8(h.pushes);
    w.u8(h.health);
    w.u8(h.life);
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
            let world_seed = r.u64()?;
            r.finish()?;
            Ok(ServerMsg::Welcome(Welcome { entity, spawn, anchor, radius, world_seed }))
        }
        MSG_SNAPSHOT => {
            let server_tick = r.u32()?;
            let step = r.u32()?;
            let ack_seq = r.u32()?;
            let buffered = r.u8()?;
            let wait = r.u16()?;
            let (pace, level, client_level) = (r.u16()?, r.u8()?, r.u8()?);
            let pos = [read_f32(&mut r)?, read_f32(&mut r)?];
            let vel = [read_f32(&mut r)?, read_f32(&mut r)?];
            let (z, vz) = (read_f32(&mut r)?, read_f32(&mut r)?);
            let grounded = match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::Invalid),
            };
            let own = MoveState { pos, vel, z, vz, grounded };
            let (pushes, health, life) = (r.u8()?, r.u8()?, r.u8()?);
            r.finish()?;
            Ok(ServerMsg::Snapshot(SnapshotHeader {
                server_tick,
                step,
                ack_seq,
                buffered,
                wait,
                pace,
                level,
                client_level,
                own,
                pushes,
                health,
                life,
            }))
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

/// Heights in blobs: 12 bits over 0-655 m (16 cm), like the near tier's range.
const BLOB_Z_MAX: f32 = 655.35;
/// Blob flags: in the air (jumping or falling); dead.
const BLOB_AIRBORNE: u32 = 1;
const BLOB_DEAD: u32 = 2;

/// A mid/far entity as a blob carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlobState {
    pub entity: u16,
    pub pos: [f32; 2],
    /// Height of the feet.
    pub z: f32,
    /// Radians.
    pub yaw: f32,
    pub pitch: f32,
    pub airborne: bool,
    /// Health to 1/15th (rounded up: a living player never reads 0).
    pub health: u8,
    pub dead: bool,
}

/// Far-tier encoding: position relative to the entity's own 512 m cell, so the
/// blob is identical for every recipient and is serialized once per tick.
pub fn encode_blob(entity: u16, s: &MoveState, yaw: u16, pitch: i16, health: u8) -> Blob {
    let pos = s.pos;
    let (cx, cy) = blob_cell(pos);
    let mut bw = BitWriter::new();
    bw.write(quantize(pos[0] - cx as f32 * BLOB_CELL, 0.0, BLOB_CELL, 15), 15);
    bw.write(quantize(pos[1] - cy as f32 * BLOB_CELL, 0.0, BLOB_CELL, 15), 15);
    bw.write(quantize(s.z, 0.0, BLOB_Z_MAX, 12), 12);
    bw.write(yaw as u32 >> 7, 9);
    bw.write((pitch as i32 + 32768) as u32 >> 10, 6);
    let dead = if health == 0 { BLOB_DEAD } else { 0 };
    bw.write(if s.grounded { 0 } else { BLOB_AIRBORNE } | dead, 3); // stance / seat flags
    bw.write(health_bucket(health), 4);
    let bits = bw.finish();

    let mut b = [0u8; FAR_BLOB];
    b[..2].copy_from_slice(&entity.to_le_bytes());
    b[2] = (cx | cy << 4) as u8;
    b[3..].copy_from_slice(&bits);
    b
}

/// Health in 15ths of `MAX_HEALTH`, rounded up so only 0 reads 0.
fn health_bucket(health: u8) -> u32 {
    (health.min(MAX_HEALTH) as u32 * 15).div_ceil(MAX_HEALTH as u32)
}

fn blob_cell(pos: [f32; 2]) -> (u32, u32) {
    let cell = |v: f32| ((v / BLOB_CELL) as u32).min(BLOB_CELLS - 1);
    (cell(pos[0]), cell(pos[1]))
}

pub fn decode_blob(b: &[u8]) -> Result<BlobState, DecodeError> {
    if b.len() != FAR_BLOB {
        return Err(DecodeError::Invalid);
    }
    let entity = u16::from_le_bytes([b[0], b[1]]);
    let (cx, cy) = ((b[2] & 0xF) as f32, (b[2] >> 4) as f32);
    let mut r = BitReader::new(&b[3..]);
    let x = cx * BLOB_CELL + dequantize(r.read(15)?, 0.0, BLOB_CELL, 15);
    let y = cy * BLOB_CELL + dequantize(r.read(15)?, 0.0, BLOB_CELL, 15);
    let z = dequantize(r.read(12)?, 0.0, BLOB_Z_MAX, 12);
    let yaw = dequantize_angle(r.read(9)?, 9);
    // Pitch is the top 6 bits of the input's i16 (-pi/2..pi/2 maps to its range).
    let pitch = ((r.read(6)? << 10) as f32 + 512.0 - 32768.0) / 32768.0 * std::f32::consts::FRAC_PI_2;
    let flags = r.read(3)?;
    let health = (r.read(4)? * MAX_HEALTH as u32 / 15) as u8;
    Ok(BlobState { entity, pos: [x, y], z, yaw, pitch, airborne: flags & BLOB_AIRBORNE != 0, health, dead: flags & BLOB_DEAD != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_roundtrip() {
        let entry = |input, render| InputEntry { input, render, shot: None };
        let shot = Shot { frac: 200, yaw: 1234, pitch: -500, render: 4321 };
        let ins = [
            InputEntry { shot: Some(shot), ..entry(Input { move_x: -127, move_y: 5, yaw: 40000, pitch: -32767, buttons: 3 }, Some(65535)) },
            entry(Input { move_x: 3, move_y: 127, yaw: 1, pitch: 1200, buttons: 0 }, Some(7)),
        ];
        let bytes = encode_inputs(10, &ins, 32);
        assert_eq!(bytes.len(), 7 + 2 * 9 + 7, "a shot is 7 B");
        let mut got = Vec::new();
        decode_inputs(&bytes, |s, i, r, sh| got.push((s, i, r, sh))).unwrap();
        let rt = |near| Some(RenderTime { near, mid_lag: 32 });
        assert_eq!(got, vec![(10, ins[0].input, rt(65535), Some(shot)), (9, ins[1].input, rt(7), None)]);
        assert_eq!(rt(render_units(1000.0)).unwrap().ages(1002), (2.0, 6.0), "mid: 4 steps further back");
        // Without a render time on every input, none is sent.
        let untimed = [InputEntry { shot: None, ..ins[0] }, entry(ins[1].input, None)];
        let bytes = encode_inputs(10, &untimed, 32);
        assert_eq!(bytes.len(), 6 + 2 * 7);
        got.clear();
        decode_inputs(&bytes, |s, i, r, sh| got.push((s, i, r, sh))).unwrap();
        assert_eq!(got, vec![(10, ins[0].input, None, None), (9, ins[1].input, None, None)]);
        // a batch reaching back past seq 1 is malformed
        assert!(decode_inputs(&encode_inputs(1, &ins, 0), |_, _, _, _| {}).is_err());
    }

    #[test]
    fn render_times_wrap_and_measure_age() {
        assert_eq!(render_age(1000, render_units(997.5)), 2.5);
        // Across the u16 wrap (1024 steps of 1/64).
        assert_eq!(render_age(1030, render_units(1020.25)), 9.75);
        assert_eq!(render_age(5, render_units(5.0)), 0.0);
        assert!(render_age(100, render_units(101.0)) < 0.0, "ahead of the server");
    }

    #[test]
    fn welcome_snapshot_and_entities_roundtrip() {
        let w = Welcome { entity: 7, spawn: [1.5, 2.5], anchor: [4096.0, 4096.0], radius: 200.0, world_seed: u64::MAX - 3 };
        assert!(matches!(decode_server_msg(&encode_welcome(&w)), Ok(ServerMsg::Welcome(x)) if x == w));

        let h = SnapshotHeader {
            server_tick: 99,
            step: 120,
            ack_seq: 42,
            buffered: 2,
            wait: 333,
            pace: 800,
            level: 7,
            client_level: 1,
            own: MoveState { pos: [1.0, 2.0], vel: [-3.0, 0.125], z: 87.25, vz: -1.5, grounded: false },
            pushes: 9,
            health: 37,
            life: 255,
        };
        let mut wr = Writer::default();
        write_snapshot(&mut wr, &h);
        assert_eq!(wr.len(), SNAPSHOT_LEN);
        assert!(matches!(decode_server_msg(wr.as_slice()), Ok(ServerMsg::Snapshot(x)) if x == h));

        let mut wr = Writer::default();
        write_entities_header(&mut wr, 99, Tier::Mid, 2);
        let at = |x, y| MoveState { pos: [x, y], ..Default::default() };
        wr.bytes(&encode_blob(1, &at(10.0, 20.0), 0, 0, 100));
        wr.bytes(&encode_blob(2, &at(30.0, 40.0), 0, 0, 100));
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
            for (z, grounded, pitch) in [(0.0, true, 0i16), (163.27, false, -16384), (655.0, true, 32767)] {
                let s = MoveState { pos: [x, y], z, grounded, ..Default::default() };
                let b = decode_blob(&encode_blob(9, &s, yaw, pitch, 100)).unwrap();
                assert_eq!(b.entity, 9);
                let p = b.pos;
                assert!((p[0] - x).abs() < 0.02 && (p[1] - y).abs() < 0.02, "{x},{y} -> {p:?}");
                let want = yaw as f32 / 65536.0 * std::f32::consts::TAU;
                assert!((b.yaw - want).abs() < 0.013, "{} vs {want}", b.yaw);
                assert!((b.z - z).abs() <= 0.08, "height to 16 cm: {z} -> {}", b.z);
                let want = pitch as f32 / 32768.0 * std::f32::consts::FRAC_PI_2;
                assert!((b.pitch - want).abs() < 0.03, "pitch {want} -> {}", b.pitch);
                assert_eq!(b.airborne, !grounded);
                assert_eq!((b.health, b.dead), (100, false));
            }
        }
    }

    #[test]
    fn blobs_carry_health_and_death() {
        let s = MoveState::default();
        let at = |h| decode_blob(&encode_blob(4, &s, 0, 0, h)).unwrap();
        assert_eq!((at(0).health, at(0).dead), (0, true));
        assert_eq!((at(1).health, at(1).dead), (6, false), "alive never reads 0");
        assert_eq!(at(50).health, 53);
        assert_eq!((at(100).health, at(100).dead), (100, false));
    }
}
