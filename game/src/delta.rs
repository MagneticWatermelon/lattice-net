//! Near-tier delta encoding (M2b).
//!
//! The server keeps every entity's quantized near state for the last
//! `NEAR_HISTORY` ticks, shared by all clients. Each near message is tagged
//! with its tick, and the transport reports which tags were acked. Once a
//! client has acked a tick, the entities that message carried can be sent as
//! deltas against that state (their *baseline*). The client keeps the same
//! 32-tick history per entity, so a baseline is always one it has.
//!
//! ```text
//! Near message := tag | server_tick:4 | n:1 | bitstream, entities in ascending id order:
//!   id gap        varbits [4, 8, 12, 16]            (id - previous id)
//!   has_base      1
//!   if has_base:
//!     base_age    5                                  (1..=31 ticks back)
//!     dx, dy      zigzag varbits [0, 8, 12, 21]      (20-bit world position, ~8 mm)
//!     dvx, dvy    zigzag varbits [0, 4, 7, 11]       (10-bit velocity)
//!     yaw         changed:1 [+ 12]
//!     rest        changed:1 [+ z 12, pitch 8, flags 3, health 7]
//!   else: the full state, 102 bits
//! ```
//!
//! A moving entity typically costs ~40 bits against a 2-3 tick old baseline,
//! against 15 bytes for a full near blob.

use std::collections::HashMap;

use lattice_net::bitpack::{dequantize, dequantize_angle, quantize, BitReader, BitWriter};
use lattice_net::wire::{DecodeError, Writer};

use crate::movement::{MoveState, WORLD_SIZE};

pub const MSG_NEAR: u8 = 5;
pub const NEAR_HEADER: usize = 1 + 4 + 1;
/// Ticks of near states both sides keep; a baseline must be younger.
pub const NEAR_HISTORY: usize = 32;
pub const MAX_BASE_AGE: u32 = NEAR_HISTORY as u32 - 1;

const CELL: f32 = 512.0;
const CELLS: u32 = (WORLD_SIZE / CELL) as u32;
const MAX_SPEED: f32 = 20.0;

const ID_GAP: [u32; 4] = [4, 8, 12, 16];
const POS: [u32; 4] = [0, 8, 12, 21];
const VEL: [u32; 4] = [0, 4, 7, 11];
/// Height steps of 1 cm: 0 on the flat, ~5 bits a tick on a slope or in a jump.
const Z: [u32; 4] = [0, 6, 10, 17];
/// Heights are 1 cm steps over 0-655 m.
const Z_MAX: f32 = 655.35;
/// `NearQ::flags` bit: in the air (jumping or falling).
pub const FLAG_AIRBORNE: u8 = 1;
/// `NearQ::flags` bit: dead (health 0), waiting to respawn.
pub const FLAG_DEAD: u8 = 2;
/// `NearQ::flags` bit: aiming down sights.
pub const FLAG_ADS: u8 = 4;

/// An entity's near-tier state, quantized: the unit deltas are taken in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NearQ {
    /// 4-bit 512 m cell << 16 | 16-bit position in the cell (~8 mm).
    pub wx: u32,
    pub wy: u32,
    /// Height of the feet, in cm.
    pub z: u16,
    pub vx: u16,
    pub vy: u16,
    pub yaw: u16,
    pub pitch: u8,
    pub flags: u8,
    pub health: u8,
}

impl NearQ {
    pub fn new(s: &MoveState, yaw: u16, pitch: i16, health: u8, ads: bool) -> Self {
        let axis = |v: f32| {
            let c = ((v / CELL) as u32).min(CELLS - 1);
            c << 16 | quantize(v - c as f32 * CELL, 0.0, CELL, 16)
        };
        Self {
            wx: axis(s.pos[0]),
            wy: axis(s.pos[1]),
            z: quantize(s.z, 0.0, Z_MAX, 16) as u16,
            vx: quantize(s.vel[0], -MAX_SPEED, MAX_SPEED, 10) as u16,
            vy: quantize(s.vel[1], -MAX_SPEED, MAX_SPEED, 10) as u16,
            yaw: yaw >> 4,
            pitch: ((pitch as i32 + 32768) >> 8) as u8,
            flags: if s.grounded { 0 } else { FLAG_AIRBORNE } | if health == 0 { FLAG_DEAD } else { 0 } | if ads { FLAG_ADS } else { 0 },
            health: health.min(127),
        }
    }

    pub fn z(&self) -> f32 {
        dequantize(self.z as u32, 0.0, Z_MAX, 16)
    }

    pub fn pos(&self) -> [f32; 2] {
        let axis = |w: u32| (w >> 16) as f32 * CELL + dequantize(w & 0xFFFF, 0.0, CELL, 16);
        [axis(self.wx), axis(self.wy)]
    }

    pub fn vel(&self) -> [f32; 2] {
        [dequantize(self.vx as u32, -MAX_SPEED, MAX_SPEED, 10), dequantize(self.vy as u32, -MAX_SPEED, MAX_SPEED, 10)]
    }

    pub fn yaw(&self) -> f32 {
        dequantize_angle(self.yaw as u32, 12)
    }
}

#[inline]
fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

#[inline]
fn unzigzag(v: u32) -> i32 {
    (v >> 1) as i32 ^ -((v & 1) as i32)
}

/// A 2-bit size class, then the value in that many bits.
fn put_var(w: &mut BitWriter, v: u32, sizes: [u32; 4]) {
    let c = sizes.iter().position(|&n| n == 32 || v < (1u32 << n)).expect("value fits the largest class");
    w.write(c as u32, 2);
    if sizes[c] > 0 {
        w.write(v, sizes[c]);
    }
}

fn get_var(r: &mut BitReader, sizes: [u32; 4]) -> Result<u32, DecodeError> {
    let n = sizes[r.read(2)? as usize];
    if n == 0 {
        Ok(0)
    } else {
        r.read(n)
    }
}

fn put_full(w: &mut BitWriter, q: &NearQ) {
    w.write(q.wx, 20);
    w.write(q.wy, 20);
    w.write(q.z as u32, 16);
    w.write(q.vx as u32, 10);
    w.write(q.vy as u32, 10);
    w.write(q.yaw as u32, 12);
    w.write(q.pitch as u32, 8);
    w.write(q.flags as u32, 3);
    w.write(q.health as u32, 7);
}

fn get_full(r: &mut BitReader) -> Result<NearQ, DecodeError> {
    Ok(NearQ {
        wx: r.read(20)?,
        wy: r.read(20)?,
        z: r.read(16)? as u16,
        vx: r.read(10)? as u16,
        vy: r.read(10)? as u16,
        yaw: r.read(12)? as u16,
        pitch: r.read(8)? as u8,
        flags: r.read(3)? as u8,
        health: r.read(7)? as u8,
    })
}

/// One entity in a near message.
#[derive(Debug, Clone, Copy)]
pub struct NearEntry {
    pub entity: u16,
    pub state: NearQ,
    /// (ticks back, state then): a state the client acked.
    pub base: Option<(u8, NearQ)>,
}

/// Writes a whole near message. Sorts `entries` by entity.
pub fn encode_near(tick: u32, entries: &mut [NearEntry], out: &mut Writer) {
    assert!(entries.len() <= u8::MAX as usize);
    entries.sort_unstable_by_key(|e| e.entity);
    out.u8(MSG_NEAR);
    out.u32(tick);
    out.u8(entries.len() as u8);
    // ~16 B per entity at worst (a full state), so this never grows.
    let mut w = BitWriter::with_capacity(entries.len() * 16);
    let mut prev = 0u16;
    for e in entries.iter() {
        put_var(&mut w, (e.entity - prev) as u32, ID_GAP);
        prev = e.entity;
        match e.base {
            Some((age, b)) => {
                debug_assert!(age >= 1 && age as u32 <= MAX_BASE_AGE);
                let s = &e.state;
                w.write(1, 1);
                w.write(age as u32, 5);
                put_var(&mut w, zigzag(s.wx as i32 - b.wx as i32), POS);
                put_var(&mut w, zigzag(s.wy as i32 - b.wy as i32), POS);
                put_var(&mut w, zigzag(s.vx as i32 - b.vx as i32), VEL);
                put_var(&mut w, zigzag(s.vy as i32 - b.vy as i32), VEL);
                put_var(&mut w, zigzag(s.z as i32 - b.z as i32), Z);
                w.write_bool(s.yaw != b.yaw);
                if s.yaw != b.yaw {
                    w.write(s.yaw as u32, 12);
                }
                w.write_bool(s.pitch != b.pitch);
                if s.pitch != b.pitch {
                    w.write(s.pitch as u32, 8);
                }
                let rest = (s.flags, s.health) != (b.flags, b.health);
                w.write_bool(rest);
                if rest {
                    w.write(s.flags as u32, 3);
                    w.write(s.health as u32, 7);
                }
            }
            None => {
                w.write(0, 1);
                put_full(&mut w, &e.state);
            }
        }
    }
    out.bytes(&w.finish());
}

/// A client's copy of the near states it received, per entity, for the last
/// `NEAR_HISTORY` ticks.
#[derive(Debug, Default)]
pub struct NearHistory {
    states: HashMap<u16, [(u32, NearQ); NEAR_HISTORY]>,
}

impl NearHistory {
    pub fn get(&self, entity: u16, tick: u32) -> Option<NearQ> {
        let (t, q) = self.states.get(&entity)?[tick as usize % NEAR_HISTORY];
        (t == tick).then_some(q)
    }

    pub fn put(&mut self, entity: u16, tick: u32, q: NearQ) {
        let ring = self.states.entry(entity).or_insert([(u32::MAX, NearQ::default()); NEAR_HISTORY]);
        ring[tick as usize % NEAR_HISTORY] = (tick, q);
    }
}

/// Decodes a near message, storing each state in `hist` and calling `f(entity,
/// state)`. Returns the server tick. A delta against a baseline `hist` doesn't
/// have is an error (the server only uses baselines the client acked).
pub fn decode_near(data: &[u8], hist: &mut NearHistory, mut f: impl FnMut(u16, NearQ)) -> Result<u32, DecodeError> {
    if data.len() < NEAR_HEADER || data[0] != MSG_NEAR {
        return Err(DecodeError::Invalid);
    }
    let tick = u32::from_le_bytes(data[1..5].try_into().unwrap());
    let n = data[5];
    let mut r = BitReader::new(&data[NEAR_HEADER..]);
    let mut prev = 0u32;
    for _ in 0..n {
        let entity = prev + get_var(&mut r, ID_GAP)?;
        if entity > u16::MAX as u32 {
            return Err(DecodeError::Invalid);
        }
        prev = entity;
        let entity = entity as u16;
        let q = if r.read_bool()? {
            let age = r.read(5)?;
            let b = hist.get(entity, tick.wrapping_sub(age)).ok_or(DecodeError::Invalid)?;
            let d = |base: u32, delta: u32| (base as i32 + unzigzag(delta)) as u32;
            let mut q = b;
            q.wx = d(b.wx, get_var(&mut r, POS)?);
            q.wy = d(b.wy, get_var(&mut r, POS)?);
            q.vx = d(b.vx as u32, get_var(&mut r, VEL)?) as u16;
            q.vy = d(b.vy as u32, get_var(&mut r, VEL)?) as u16;
            q.z = d(b.z as u32, get_var(&mut r, Z)?) as u16;
            if r.read_bool()? {
                q.yaw = r.read(12)? as u16;
            }
            if r.read_bool()? {
                q.pitch = r.read(8)? as u8;
            }
            if r.read_bool()? {
                q.flags = r.read(3)? as u8;
                q.health = r.read(7)? as u8;
            }
            q
        } else {
            get_full(&mut r)?
        };
        hist.put(entity, tick, q);
        f(entity, q);
    }
    Ok(tick)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(x: f32, y: f32, vx: f32, vy: f32, yaw: u16) -> NearQ {
        let z = (x * 0.013) % 150.0; // terrain-like heights, varying with x
        NearQ::new(&MoveState { pos: [x, y], vel: [vx, vy], z, vz: 0.0, grounded: true }, yaw, 0, 100, false)
    }

    #[test]
    fn quantization_roundtrips_within_precision() {
        for &(x, y) in &[(0.0, 0.0), (511.99, 512.0), (4096.3, 7000.7), (WORLD_SIZE, WORLD_SIZE)] {
            let s = q(x, y, 9.0, -3.3, 40000);
            let p = s.pos();
            assert!((p[0] - x).abs() < 0.01 && (p[1] - y).abs() < 0.01, "{x},{y} -> {p:?}");
            let v = s.vel();
            assert!((v[0] - 9.0).abs() < 0.03 && (v[1] + 3.3).abs() < 0.03);
            assert!((s.z() - (x * 0.013) % 150.0).abs() <= 0.005, "height to 1 cm");
        }
    }

    #[test]
    fn full_then_deltas_decode_exactly_and_deltas_are_small() {
        let mut hist = NearHistory::default();
        let mut w = Writer::default();
        // Tick 100: three entities, no baselines yet.
        let mut state: Vec<(u16, NearQ)> = vec![(7, q(100.0, 100.0, 6.0, 0.0, 0)), (300, q(120.0, 90.0, 0.0, -9.0, 9000)), (9000, q(4000.0, 4000.0, 0.0, 0.0, 0))];
        let mut entries: Vec<NearEntry> = state.iter().map(|&(e, s)| NearEntry { entity: e, state: s, base: None }).collect();
        encode_near(100, &mut entries, &mut w);
        let full_len = w.len();
        let mut got = Vec::new();
        assert_eq!(decode_near(w.as_slice(), &mut hist, |e, s| got.push((e, s))), Ok(100));
        assert_eq!(got, state);

        // Tick 103: everyone moved a little; baselines are tick 100 (age 3).
        let base = state.clone();
        state[0].1 = q(100.6, 100.0, 6.0, 0.0, 0);
        state[1].1 = q(120.0, 89.1, 0.0, -9.0, 9000);
        state[2].1 = q(4000.0, 4000.0, 0.0, 0.0, 16); // turned in place
        let mut entries: Vec<NearEntry> =
            state.iter().zip(&base).map(|(&(e, s), &(_, b))| NearEntry { entity: e, state: s, base: Some((3, b)) }).collect();
        let mut w = Writer::default();
        encode_near(103, &mut entries, &mut w);
        got.clear();
        assert_eq!(decode_near(w.as_slice(), &mut hist, |e, s| got.push((e, s))), Ok(103));
        assert_eq!(got, state, "deltas decode to the exact quantized state");
        let per_entity = (w.len() - NEAR_HEADER) as f32 / 3.0;
        assert!(per_entity < 6.0, "{per_entity} B per moving entity (full: {} B)", (full_len - NEAR_HEADER) / 3);
    }

    #[test]
    fn a_missing_baseline_is_an_error_not_a_wrong_state() {
        let mut hist = NearHistory::default();
        let s = q(10.0, 10.0, 0.0, 0.0, 0);
        let mut w = Writer::default();
        encode_near(50, &mut [NearEntry { entity: 1, state: s, base: Some((2, s)) }], &mut w);
        assert_eq!(decode_near(w.as_slice(), &mut hist, |_, _| {}), Err(DecodeError::Invalid));
        // An entry from 32 ticks ago has been overwritten in the ring.
        hist.put(1, 48, s);
        assert!(hist.get(1, 48).is_some());
        hist.put(1, 48 + NEAR_HISTORY as u32, s);
        assert!(hist.get(1, 48).is_none());
    }

    #[test]
    fn varbits_cover_every_class_and_extremes() {
        let mut w = BitWriter::new();
        let vals = [0u32, 1, 255, 256, 4095, 4096, (1 << 21) - 1];
        for &v in &vals {
            put_var(&mut w, v, POS);
        }
        for v in [-1_000_000i32, -1, 0, 1, 1_000_000] {
            put_var(&mut w, zigzag(v), POS);
        }
        let b = w.finish();
        let mut r = BitReader::new(&b);
        for &v in &vals {
            assert_eq!(get_var(&mut r, POS).unwrap(), v);
        }
        for v in [-1_000_000i32, -1, 0, 1, 1_000_000] {
            assert_eq!(unzigzag(get_var(&mut r, POS).unwrap()), v);
        }
    }
}
