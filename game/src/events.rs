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
//!   Deploying 4 | site:2 at:4             to who deploys (U4): it appears there at step `at`
//!   Refused 5 | site:2 reason:1           to who asked to deploy: not there (`territory::REFUSED_*`)
//!   Reward 6 | kind:1 xp:2 about:2         to who earned it (`REWARD_*`; `about`: an entity or a site)
//! S->C unreliable  Shots    tag | step:4 | n:1 | n × (shooter:2 yaw:2 pitch:2 ago:1)
//!                           shots by near-tier players this tick, for tracers: fired `ago`
//!                           sixteenths of a step before `step`
//! S->C reliable    Recap    tag | killer:2 cause:1 flags:1 distance:2 killer_health:1 | n:1 |
//!                           n × (from:2 damage:2 hits:1 heads:1)
//!                           to who died (U4, GDD §4.4): who killed it, with what (`CAUSE_*`),
//!                           from how far (dm), with how much health left (flags: 1 head); and
//!                           the damage it took in its last `RECAP_STEPS`, by who dealt it, most first
//! S->C reliable    Fights   tag | step:4 | n:1 | n × (site:2 players:3 heat:1)
//!                           every `FIGHTS_STEPS` to everyone (U4): each site with anyone at it,
//!                           how many of each faction (capped 255) and its shots a second
//! ```

use lattice_net::wire::{DecodeError, Reader, Writer};

pub const MSG_EVENTS: u8 = 6;
pub const MSG_SHOTS: u8 = 7;
pub const MSG_RECAP: u8 = 11;
pub const MSG_FIGHTS: u8 = 12;

const HIT: u8 = 1;
const HURT: u8 = 2;
const KILL: u8 = 3;
const DEPLOYING: u8 = 4;
const REFUSED: u8 = 5;
const REWARD: u8 = 6;

/// What a reward is for (GDD §8: XP for everything that wins fights): a
/// kill (`about`: the victim), an assist (damage to someone a teammate
/// killed within `RECAP_STEPS`), a site taken (on it when it flipped to us;
/// `about`: the site) or held (an attack on it drained away).
pub const REWARD_KILL: u8 = 1;
pub const REWARD_ASSIST: u8 = 2;
pub const REWARD_CAPTURE: u8 = 3;
pub const REWARD_DEFEND: u8 = 4;
/// A kill's XP; a headshot adds `XP_HEAD`.
pub const XP_KILL: u16 = 100;
pub const XP_HEAD: u16 = 25;
pub const XP_ASSIST: u16 = 50;
/// A base, great site or the Hollow taken; an outpost.
pub const XP_CAPTURE: u16 = 500;
pub const XP_CAPTURE_OUTPOST: u16 = 250;
pub const XP_DEFEND: u16 = 100;

/// What killed someone (`Recap::cause`): a weapon (its kit, `weapon::of`:
/// 0 is the assault rifle), or a fall.
pub const CAUSE_RIFLE: u8 = 0;
pub const CAUSE_FALL: u8 = 255;
/// A spawn room's field: it hurts anyone in it whose side doesn't hold its site.
pub const CAUSE_FIELD: u8 = 254;
/// A recap's damage reaches back this far: 5 s.
pub const RECAP_STEPS: u32 = 150;
/// The most damage lines a recap carries.
pub const RECAP_LINES: usize = 8;
/// Fights go to everyone this often: 5 s.
pub const FIGHTS_STEPS: u32 = 150;
/// No one: a recap's killer for a fall.
pub const NOBODY: u16 = u16::MAX;
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
    /// We deploy at `site`, appearing at step `at` (lattice transit).
    Deploying { site: u16, at: u32 },
    /// We can't deploy at `site` (`territory::REFUSED_*`): asked for, or
    /// lost in transit.
    Refused { site: u16, reason: u8 },
    /// XP for a `REWARD_*`, `about` an entity (a kill, an assist) or a site.
    Reward { kind: u8, xp: u16, about: u16 },
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
            Event::Deploying { site, at } => {
                w.u8(DEPLOYING);
                w.u16(site);
                w.u32(at);
            }
            Event::Refused { site, reason } => {
                w.u8(REFUSED);
                w.u16(site);
                w.u8(reason);
            }
            Event::Reward { kind, xp, about } => {
                w.u8(REWARD);
                w.u8(kind);
                w.u16(xp);
                w.u16(about);
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
            DEPLOYING => Event::Deploying { site: r.u16()?, at: r.u32()? },
            REFUSED => Event::Refused { site: r.u16()?, reason: r.u8()? },
            REWARD => Event::Reward { kind: r.u8()?, xp: r.u16()?, about: r.u16()? },
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

/// A death recap (GDD §4.4): who killed us, with what, from how far and
/// with how much health left, and the damage we took in our last
/// `RECAP_STEPS`, by who dealt it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recap {
    /// `NOBODY` for a fall.
    pub killer: u16,
    pub cause: u8,
    pub head: bool,
    /// Decimeters.
    pub distance: u16,
    pub killer_health: u8,
    /// (from, damage, hits, headshots), most damage first (`NOBODY`: falls).
    pub lines: Vec<(u16, u16, u8, u8)>,
}

pub fn encode_recap(r: &Recap) -> Vec<u8> {
    let n = r.lines.len().min(RECAP_LINES);
    let mut w = Writer::with_capacity(10 + n * 6);
    w.u8(MSG_RECAP);
    w.u16(r.killer);
    w.u8(r.cause);
    w.u8(if r.head { HEAD } else { 0 });
    w.u16(r.distance);
    w.u8(r.killer_health);
    w.u8(n as u8);
    for &(from, damage, hits, heads) in &r.lines[..n] {
        w.u16(from);
        w.u16(damage);
        w.u8(hits);
        w.u8(heads);
    }
    w.into_inner()
}

pub fn decode_recap(data: &[u8]) -> Result<Recap, DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_RECAP {
        return Err(DecodeError::Invalid);
    }
    let (killer, cause, flags, distance, killer_health) = (r.u16()?, r.u8()?, r.u8()?, r.u16()?, r.u8()?);
    let n = r.u8()? as usize;
    if n > RECAP_LINES {
        return Err(DecodeError::Invalid);
    }
    let mut lines = Vec::with_capacity(n);
    for _ in 0..n {
        lines.push((r.u16()?, r.u16()?, r.u8()?, r.u8()?));
    }
    r.finish()?;
    Ok(Recap { killer, cause, head: flags & HEAD != 0, distance, killer_health, lines })
}

/// A site's fight, as the Fights message has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fight {
    pub site: u16,
    /// Players of each faction there (capped 255).
    pub players: [u8; 3],
    /// Shots fired there a second (capped 255).
    pub heat: u8,
}

pub fn encode_fights(step: u32, fights: &[Fight]) -> Vec<u8> {
    let n = fights.len().min(u8::MAX as usize);
    let mut w = Writer::with_capacity(6 + n * 6);
    w.u8(MSG_FIGHTS);
    w.u32(step);
    w.u8(n as u8);
    for f in &fights[..n] {
        w.u16(f.site);
        for p in f.players {
            w.u8(p);
        }
        w.u8(f.heat);
    }
    w.into_inner()
}

pub fn decode_fights(data: &[u8]) -> Result<(u32, Vec<Fight>), DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_FIGHTS {
        return Err(DecodeError::Invalid);
    }
    let step = r.u32()?;
    let n = r.u8()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        out.push(Fight { site: r.u16()?, players: [r.u8()?, r.u8()?, r.u8()?], heat: r.u8()? });
    }
    r.finish()?;
    Ok((step, out))
}

/// The yaw from `from` to `to`, in 1/256 turns.
pub fn direction(from: [f32; 2], to: [f32; 2]) -> u8 {
    let a = libm::atan2f(to[1] - from[1], to[0] - from[0]);
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
            Event::Deploying { site: 12, at: 0x0102_0304 },
            Event::Refused { site: 3, reason: 2 },
            Event::Reward { kind: REWARD_KILL, xp: XP_KILL + XP_HEAD, about: 7 },
        ];
        let b = encode_events(&ev);
        assert_eq!(b.len(), 2 + 5 + 5 + 6 + 7 + 4 + 6);
        assert_eq!(decode_events(&b).unwrap(), ev);
        assert!(decode_events(&b[..b.len() - 1]).is_err());
    }

    #[test]
    fn recaps_and_fights_roundtrip() {
        let r = Recap { killer: 77, cause: CAUSE_RIFLE, head: true, distance: 1432, killer_health: 35, lines: vec![(77, 60, 3, 1), (NOBODY, 12, 1, 0)] };
        let b = encode_recap(&r);
        assert_eq!(b.len(), 9 + 2 * 6);
        assert_eq!(decode_recap(&b).unwrap(), r);
        assert!(decode_recap(&b[..b.len() - 1]).is_err());
        let f = [Fight { site: 3, players: [12, 0, 255], heat: 40 }, Fight { site: 60, players: [1, 1, 1], heat: 0 }];
        let b = encode_fights(9000, &f);
        assert_eq!(decode_fights(&b).unwrap(), (9000, f.to_vec()));
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
