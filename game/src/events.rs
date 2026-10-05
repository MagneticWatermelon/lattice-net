//! Combat news (M3d.3): what the server tells players about hits, kills and
//! who's shooting.
//!
//! ```text
//! S->C reliable    Events   tag | n:1 | n × event        (one message per client per tick, if any)
//!   Hit  1 | target:2 damage:1 flags:1     to the shooter: a hit marker (flags: 1 head, 2 killed)
//!   Hurt 2 | from:2 amount:1 dir:1         to the target: who, and where from (the yaw from the
//!                                          target to the shooter, in 1/256 turns, so it works
//!                                          even when the shooter isn't drawn)
//!   Kill 3 | killer:2 victim:2 flags:1     to both and their squads (flags: 1 head)
//! S->C unreliable  Shots    tag | step:4 | n:1 | n × (shooter:2 yaw:2 pitch:2 ago:1)
//!                           shots by near-tier players this tick, for tracers: fired `ago`
//!                           sixteenths of a step before `step`
//! ```

use lattice_net::wire::{DecodeError, Reader, Writer};

pub const MSG_EVENTS: u8 = 6;
pub const MSG_SHOTS: u8 = 7;

const HIT: u8 = 1;
const HURT: u8 = 2;
const KILL: u8 = 3;
const HEAD: u8 = 1;
const KILLED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Our shot hit `target` for `damage` (0 never sent).
    Hit { target: u16, damage: u8, head: bool, killed: bool },
    /// We were hit by `from` for `amount`; `dir` is the yaw from us to it,
    /// in 1/256 turns.
    Hurt { from: u16, amount: u8, dir: u8 },
    Kill { killer: u16, victim: u16, head: bool },
}

pub fn encode_events(events: &[Event]) -> Vec<u8> {
    assert!(events.len() <= u8::MAX as usize);
    let mut w = Writer::with_capacity(2 + events.len() * 6);
    w.u8(MSG_EVENTS);
    w.u8(events.len() as u8);
    for e in events {
        match *e {
            Event::Hit { target, damage, head, killed } => {
                w.u8(HIT);
                w.u16(target);
                w.u8(damage);
                w.u8(if head { HEAD } else { 0 } | if killed { KILLED } else { 0 });
            }
            Event::Hurt { from, amount, dir } => {
                w.u8(HURT);
                w.u16(from);
                w.u8(amount);
                w.u8(dir);
            }
            Event::Kill { killer, victim, head } => {
                w.u8(KILL);
                w.u16(killer);
                w.u16(victim);
                w.u8(if head { HEAD } else { 0 });
            }
        }
    }
    w.into_inner()
}

pub fn decode_events(data: &[u8]) -> Result<Vec<Event>, DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_EVENTS {
        return Err(DecodeError::Invalid);
    }
    let n = r.u8()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        out.push(match r.u8()? {
            HIT => {
                let (target, damage, flags) = (r.u16()?, r.u8()?, r.u8()?);
                Event::Hit { target, damage, head: flags & HEAD != 0, killed: flags & KILLED != 0 }
            }
            HURT => Event::Hurt { from: r.u16()?, amount: r.u8()?, dir: r.u8()? },
            KILL => {
                let (killer, victim, flags) = (r.u16()?, r.u16()?, r.u8()?);
                Event::Kill { killer, victim, head: flags & HEAD != 0 }
            }
            _ => return Err(DecodeError::Invalid),
        });
    }
    r.finish()?;
    Ok(out)
}

/// A shot someone near fired, for its tracer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeenShot {
    pub shooter: u16,
    /// When it fired, in game steps.
    pub step: f64,
    pub yaw: u16,
    pub pitch: i16,
}

/// Bytes per shot in a Shots message.
pub const SHOT_BYTES: usize = 7;
pub const SHOTS_HEADER: usize = 1 + 4 + 1;

/// `shots`: (shooter, yaw, pitch, when in steps), all within ~16 steps
/// before `step`.
pub fn encode_shots(step: u32, shots: impl ExactSizeIterator<Item = (u16, u16, i16, f64)>) -> Vec<u8> {
    let n = shots.len().min(u8::MAX as usize);
    let mut w = Writer::with_capacity(SHOTS_HEADER + n * SHOT_BYTES);
    w.u8(MSG_SHOTS);
    w.u32(step);
    w.u8(n as u8);
    for (shooter, yaw, pitch, at) in shots.take(n) {
        w.u16(shooter);
        w.u16(yaw);
        w.u16(pitch as u16);
        w.u8(((step as f64 - at) * 16.0).round().clamp(0.0, 255.0) as u8);
    }
    w.into_inner()
}

pub fn decode_shots(data: &[u8]) -> Result<Vec<SeenShot>, DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_SHOTS {
        return Err(DecodeError::Invalid);
    }
    let step = r.u32()? as f64;
    let n = r.u8()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (shooter, yaw, pitch, ago) = (r.u16()?, r.u16()?, r.u16()? as i16, r.u8()?);
        out.push(SeenShot { shooter, step: step - ago as f64 / 16.0, yaw, pitch });
    }
    r.finish()?;
    Ok(out)
}

/// The yaw from `from` to `to`, in 1/256 turns.
pub fn direction(from: [f32; 2], to: [f32; 2]) -> u8 {
    let a = (to[1] - from[1]).atan2(to[0] - from[0]);
    (a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 256.0).round() as u32 as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_roundtrip() {
        let ev = [
            Event::Hit { target: 7, damage: 40, head: true, killed: true },
            Event::Hurt { from: 9, amount: 20, dir: 200 },
            Event::Kill { killer: 9, victim: 7, head: false },
        ];
        let b = encode_events(&ev);
        assert_eq!(b.len(), 2 + 5 + 5 + 6);
        assert_eq!(decode_events(&b).unwrap(), ev);
        assert!(decode_events(&b[..b.len() - 1]).is_err());
    }

    #[test]
    fn shots_roundtrip_with_their_time() {
        let b = encode_shots(1000, [(3u16, 100u16, -5i16, 999.25), (4, 0, 0, 1000.0)].into_iter());
        assert_eq!(b.len(), SHOTS_HEADER + 2 * SHOT_BYTES);
        let s = decode_shots(&b).unwrap();
        assert_eq!(s[0], SeenShot { shooter: 3, step: 999.25, yaw: 100, pitch: -5 });
        assert_eq!(s[1].step, 1000.0);
    }

    #[test]
    fn directions() {
        assert_eq!(direction([0.0, 0.0], [10.0, 0.0]), 0, "east");
        assert_eq!(direction([0.0, 0.0], [0.0, 10.0]), 64, "north");
        assert_eq!(direction([0.0, 0.0], [-10.0, 0.0]), 128);
    }
}
