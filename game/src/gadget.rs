//! Gadgets (GDD §4.3, U6): what the classes throw, drop and build. A
//! gadget is thrown (or set down) by a player's tool key (`movement::Input::tools`)
//! or its ability (the engineer's turret), flies under gravity, bounces or
//! sticks, and does its thing: a frag bursts after its fuse, smoke spreads a
//! cloud, C4 waits for its owner, a mine for an enemy, an ammo pack refills
//! friends' reserves, a recon dart spots enemies, a turret shoots them.
//!
//! The server keeps them; it tells clients each one as it's spawned (its
//! position, velocity and step, full precision) and gone (`msg_gadgets`).
//! Flight is this module's `Gadget::step`, the same on both sides from the
//! same start: a client draws the same arc and the same resting place
//! without being sent them. What they do to players is the server's.

use crate::hit::segment_hit;
use crate::movement::DT;
use crate::world::World;

/// `movement::Input::tools` bits: the use key held (E: heal, revive,
/// repair, hack), the class's first and second tools pressed (G, X).
pub const TOOL_USE: u8 = 1;
pub const TOOL_ONE: u8 = 2;
pub const TOOL_TWO: u8 = 4;

/// Gadget kinds.
pub const NONE: u8 = 0;
pub const FRAG: u8 = 1;
pub const SMOKE: u8 = 2;
pub const C4: u8 = 3;
pub const MINE: u8 = 4;
pub const AMMO_PACK: u8 = 5;
pub const DART: u8 = 6;
pub const TURRET: u8 = 7;
/// Not a gadget: the Exo launcher's shell bursting (`Blast` news only).
pub const SHELL: u8 = 8;
pub const KINDS: u8 = 9;

pub const NAMES: [&str; KINDS as usize] = ["", "frag grenade", "smoke grenade", "C4", "mine", "ammo pack", "recon dart", "turret", "shell"];

/// What each class carries (`class`): its first and second tools and how
/// many of each a life (0: none). The medic's are its use key (heal,
/// revive); the Exo carries none.
pub const TOOLS: [[(u8, u8); 2]; crate::class::CLASSES as usize] = [
    [(FRAG, 2), (NONE, 0)],
    [(NONE, 0), (NONE, 0)],
    [(AMMO_PACK, 1), (MINE, 2)],
    [(DART, 2), (NONE, 0)],
    [(C4, 2), (SMOKE, 1)],
    [(NONE, 0), (NONE, 0)],
];

/// The class's tool `slot` (0: G, 1: X) and its count a life.
pub fn tool(kit: u8, slot: usize) -> (u8, u8) {
    TOOLS.get(kit as usize).map_or((NONE, 0), |t| t[slot.min(1)])
}

/// Gravity on thrown things, m/s² (a little more than on players' slugs, for an arc that reads).
pub const GRAVITY: f32 = 15.0;
/// How a kind leaves the hand and lands: speed (m/s), extra loft (radians
/// above the aim), what's left of its speed after a bounce, whether it
/// sticks to what it hits.
#[derive(Debug, Clone, Copy)]
pub struct Throw {
    pub speed: f32,
    pub loft: f32,
    pub bounce: f32,
    pub sticks: bool,
}

pub fn throw_of(kind: u8) -> Throw {
    match kind {
        FRAG => Throw { speed: 17.0, loft: 0.12, bounce: 0.35, sticks: false },
        SMOKE => Throw { speed: 16.0, loft: 0.12, bounce: 0.3, sticks: false },
        C4 => Throw { speed: 11.0, loft: 0.1, bounce: 0.0, sticks: true },
        MINE | AMMO_PACK => Throw { speed: 4.5, loft: 0.3, bounce: 0.1, sticks: false },
        DART => Throw { speed: 45.0, loft: 0.02, bounce: 0.0, sticks: true },
        _ => Throw { speed: 0.0, loft: 0.0, bounce: 0.0, sticks: false },
    }
}

const S: u32 = crate::movement::TICK_HZ;
/// A frag's fuse, from the throw; smoke's, before it spreads; how long its
/// cloud lasts and how wide it is.
pub const FRAG_FUSE: u32 = 3 * S;
pub const SMOKE_FUSE: u32 = S * 3 / 2;
pub const SMOKE_STEPS: u32 = 12 * S;
pub const SMOKE_RADIUS: f32 = 7.0;
/// C4 waits this long for its owner, then goes off by itself.
pub const C4_STEPS: u32 = 90 * S;
/// A mine arms this long after it lands, then goes off when an enemy's feet
/// come within `MINE_TRIGGER` (and 1.5 m up or down); it lasts five minutes.
pub const MINE_ARM: u32 = 2 * S;
pub const MINE_TRIGGER: f32 = 2.5;
pub const MINE_STEPS: u32 = 300 * S;
/// An ammo pack lasts this long, refilling friends within `PACK_RADIUS`
/// every `PACK_PERIOD` (a magazine's worth, up to their reserve's full).
pub const PACK_STEPS: u32 = 30 * S;
pub const PACK_RADIUS: f32 = 4.0;
pub const PACK_PERIOD: u32 = 2 * S;
/// A recon dart, stuck, spots enemies within `DART_RADIUS` for this long.
pub const DART_STEPS: u32 = 20 * S;
pub const DART_RADIUS: f32 = 30.0;
/// A turret: its health; it shoots the nearest enemy it sees within
/// `TURRET_RANGE` every `TURRET_PERIOD` steps, `TURRET_DAMAGE` a hit, within
/// `TURRET_CONE` degrees of where it aims; its gun is `TURRET_HEIGHT` up. An
/// engineer has one: a new one takes the old's place.
pub const TURRET_HEALTH: u16 = 200;
pub const TURRET_RANGE: f32 = 35.0;
pub const TURRET_PERIOD: u32 = 6;
pub const TURRET_DAMAGE: u8 = 9;
pub const TURRET_CONE: f32 = 1.5;
pub const TURRET_HEIGHT: f32 = 0.9;
/// Shots, blasts and repairs hit a turret within this of its middle (0.7 m up).
pub const TURRET_RADIUS: f32 = 0.6;
/// The turret: set down this far ahead of its engineer (on the ground there).
pub const TURRET_AHEAD: f32 = 1.8;

/// A burst: how far it reaches (m) and its damage at its heart (falling to
/// nothing at its reach; only where its center sees).
pub fn blast(kind: u8) -> Option<(f32, u8)> {
    match kind {
        FRAG => Some((6.0, 120)),
        C4 => Some((5.0, 160)),
        MINE => Some((4.0, 110)),
        SHELL => Some((3.5, 70)),
        _ => None,
    }
}

/// A burst's damage at distance `d` from its heart.
pub fn blast_damage(kind: u8, d: f32) -> u8 {
    blast(kind).map_or(0, |(r, max)| {
        let k = (1.0 - d / r).clamp(0.0, 1.0);
        libm::roundf(max as f32 * k * k.sqrt()) as u8
    })
}

/// The use key's reach (heal, revive, repair, hack), m; how long a revive
/// and a hack take to complete; what heals and repairs give.
pub const USE_RANGE: f32 = 3.0;
pub const REVIVE_STEPS: u32 = 2 * S;
pub const HACK_STEPS: u32 = 3 * S;
/// Health a heal gives every `USE_PERIOD` steps (10 a second); a repair to
/// a turret, and to an Exo (its health counts thrice).
pub const USE_PERIOD: u32 = S / 2;
pub const HEAL: u8 = 5;
pub const REPAIR_TURRET: u16 = 15;
pub const REPAIR_EXO: u8 = 3;
/// A revived player stands with this.
pub const REVIVE_HEALTH: u8 = 50;

/// What the use key is doing (the HUD's: `msg::SnapshotHeader::using`).
pub const USING_NONE: u8 = 0;
pub const USING_HEAL: u8 = 1;
pub const USING_REVIVE: u8 = 2;
pub const USING_REPAIR: u8 = 3;
pub const USING_HACK: u8 = 4;

/// A gadget, as the server keeps it and clients draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gadget {
    pub id: u32,
    pub kind: u8,
    /// Who threw or built it, and the side it's on (a hacked turret changes side).
    pub owner: u16,
    pub faction: u8,
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    /// Where it lies, the face it lies on.
    pub normal: [f32; 3],
    pub resting: bool,
    /// The step it was thrown at, and the step it's at now.
    pub born: u32,
    pub at: u32,
    /// A turret's.
    pub health: u16,
}

impl Gadget {
    /// Thrown by `owner` (of `faction`) from `eye` along its aim (`yaw`,
    /// `pitch` radians) at step `step`.
    #[allow(clippy::too_many_arguments)]
    pub fn thrown(id: u32, kind: u8, owner: u16, faction: u8, eye: [f32; 3], yaw: f32, pitch: f32, step: u32) -> Self {
        let t = throw_of(kind);
        let p = (pitch + t.loft).min(1.4);
        let (sy, cy) = libm::sincosf(yaw);
        let (sp, cp) = libm::sincosf(p);
        let dir = [cy * cp, sy * cp, sp];
        // Out of the hand, a little ahead and below the eyes.
        let pos = [eye[0] + dir[0] * 0.4, eye[1] + dir[1] * 0.4, eye[2] - 0.15];
        let vel = dir.map(|v| v * t.speed);
        Self { id, kind, owner, faction, pos, vel, normal: [0.0, 0.0, 1.0], resting: false, born: step, at: step, health: 0 }
    }

    /// Set down at `pos` (on the ground there), facing `yaw` (a turret).
    pub fn placed(id: u32, kind: u8, owner: u16, faction: u8, pos: [f32; 3], step: u32) -> Self {
        let health = if kind == TURRET { TURRET_HEALTH } else { 0 };
        Self { id, kind, owner, faction, pos, vel: [0.0; 3], normal: [0.0, 0.0, 1.0], resting: true, born: step, at: step, health }
    }

    /// One step of its flight (nothing once it lies still): gravity, then
    /// what it hits (its own side's shields let it through). Returns whether
    /// it came to rest in this step.
    pub fn step(&mut self, world: &World) -> bool {
        self.at += 1;
        if self.resting {
            return false;
        }
        let t = throw_of(self.kind);
        let vel = [self.vel[0], self.vel[1], self.vel[2] - GRAVITY * DT];
        let next = [self.pos[0] + vel[0] * DT, self.pos[1] + vel[1] * DT, self.pos[2] + vel[2] * DT];
        match segment_hit(world, self.pos, next, self.faction) {
            Some(hit) => {
                let n = hit.normal;
                let lift = [n[0] * 0.03, n[1] * 0.03, n[2] * 0.03];
                self.pos = [hit.pos[0] + lift[0], hit.pos[1] + lift[1], hit.pos[2] + lift[2]];
                if t.sticks {
                    (self.vel, self.normal, self.resting) = ([0.0; 3], n, true);
                    return true;
                }
                let into = vel[0] * n[0] + vel[1] * n[1] + vel[2] * n[2];
                let out = [vel[0] - 2.0 * into * n[0], vel[1] - 2.0 * into * n[1], vel[2] - 2.0 * into * n[2]];
                self.vel = out.map(|v| v * t.bounce);
                let speed = (self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1] + self.vel[2] * self.vel[2]).sqrt();
                if speed < 1.2 && n[2] > 0.5 {
                    (self.vel, self.normal, self.resting) = ([0.0; 3], n, true);
                    return true;
                }
                false
            }
            None => {
                (self.pos, self.vel) = (next, vel);
                // Out of the world (a hole to nowhere): it lies where it is.
                if self.pos[2] < -50.0 {
                    (self.vel, self.resting) = ([0.0; 3], true);
                    return true;
                }
                false
            }
        }
    }

    /// Steps until `step` (a client catching up with a gadget it was just told of).
    pub fn step_to(&mut self, world: &World, step: u32) {
        while self.at < step {
            self.step(world);
        }
    }

    /// Where shots, blasts and the use key reach a turret.
    pub fn middle(&self) -> [f32; 3] {
        [self.pos[0], self.pos[1], self.pos[2] + 0.7]
    }

    /// A turret's gun.
    pub fn gun(&self) -> [f32; 3] {
        [self.pos[0], self.pos[1], self.pos[2] + TURRET_HEIGHT]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frag_arcs_bounces_and_rests_the_same_every_time() {
        let w = World::shared(crate::world::KILN);
        let (x, y) = (2048.0f32, 2048.0f32);
        let z = w.terrain(x, y);
        let mut a = Gadget::thrown(1, FRAG, 3, 0, [x, y, z + 1.6], 0.3, 0.2, 100);
        let mut b = a;
        let mut landed = None;
        for k in 0..FRAG_FUSE {
            if a.step(&w) {
                landed = Some(k);
            }
            b.step(&w);
            assert_eq!(a, b, "deterministic");
        }
        let k = landed.expect("it comes to rest");
        let d = ((a.pos[0] - x) * (a.pos[0] - x) + (a.pos[1] - y) * (a.pos[1] - y)).sqrt();
        assert!(a.resting && d > 5.0 && d < 40.0, "landed {d:.1} m off after {k} steps: {a:?}");
        assert!((a.pos[2] - w.terrain(a.pos[0], a.pos[1])).abs() < 0.3, "on the ground: {a:?}");
        // A late client catches up to the same place.
        let mut c = Gadget::thrown(1, FRAG, 3, 0, [x, y, z + 1.6], 0.3, 0.2, 100);
        c.step_to(&w, 100 + FRAG_FUSE);
        assert_eq!(c, a);
    }

    #[test]
    fn a_dart_sticks_and_blasts_fall_off() {
        let w = World::shared(crate::world::KILN);
        let (x, y) = (2048.0f32, 2048.0f32);
        let z = w.terrain(x, y);
        let mut d = Gadget::thrown(1, DART, 3, 0, [x, y, z + 1.6], 1.0, -0.3, 0);
        for _ in 0..90 {
            d.step(&w);
        }
        assert!(d.resting && d.vel == [0.0; 3], "stuck: {d:?}");
        assert_eq!(blast_damage(FRAG, 0.0), 120);
        assert!(blast_damage(FRAG, 3.0) < 60 && blast_damage(FRAG, 3.0) > 30);
        assert_eq!(blast_damage(FRAG, 6.5), 0);
        assert_eq!(tool(crate::class::ENGINEER, 1), (MINE, 2));
    }
}

// ---- news ---------------------------------------------------------------------

/// Reliable, server to client: what gadgets did since the last (one message
/// a tick a client, when there's any).
///
/// ```text
/// tag | n:1 | n × news
///   Spawn   1 | id:4 kind:1 owner:2 faction:1 born:4 at:4 pos:3×f32 vel:3×f32 normal:3×f32 resting:1 health:2
///   Gone    2 | id:4 why:1                 (`GONE_*`)
///   Blast   3 | kind:1 pos:3×f32           (a frag, C4, a mine, a launcher's shell bursting)
///   Shot    4 | id:4 yaw:2 pitch:2         (a turret fires: tracers from its gun)
///   State   5 | id:4 owner:2 faction:1 health:2   (a turret hurt, repaired or hacked)
///   Spotted 6 | n:1 | n × entity:2         (the enemies our side's darts see now; replaces the last)
/// ```
pub const MSG_GADGETS: u8 = 13;

const SPAWN: u8 = 1;
const GONE: u8 = 2;
const BLAST: u8 = 3;
const SHOT: u8 = 4;
const STATE: u8 = 5;
const SPOTTED: u8 = 6;

/// Why a gadget went.
pub const GONE_EXPIRED: u8 = 0;
pub const GONE_BURST: u8 = 1;
pub const GONE_DESTROYED: u8 = 2;
pub const GONE_REPLACED: u8 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum News {
    /// A gadget thrown or set down (or, to a player joining, one already there).
    Spawn(Gadget),
    Gone { id: u32, why: u8 },
    Blast { kind: u8, pos: [f32; 3] },
    /// A turret's shot: `yaw` and `pitch` as an input's (`weapon::aim`).
    Shot { id: u32, yaw: u16, pitch: i16 },
    State { id: u32, owner: u16, faction: u8, health: u16 },
    Spotted(Vec<u16>),
}

pub fn encode_news(news: &[News]) -> Vec<u8> {
    use lattice_net::wire::Writer;
    let mut w = Writer::with_capacity(2 + news.len() * 24);
    w.u8(MSG_GADGETS);
    w.u8(news.len().min(255) as u8);
    let f3 = |w: &mut Writer, v: [f32; 3]| {
        for x in v {
            w.u32(x.to_bits());
        }
    };
    for n in news.iter().take(255) {
        match n {
            News::Spawn(g) => {
                w.u8(SPAWN);
                w.u32(g.id);
                w.u8(g.kind);
                w.u16(g.owner);
                w.u8(g.faction);
                w.u32(g.born);
                w.u32(g.at);
                f3(&mut w, g.pos);
                f3(&mut w, g.vel);
                f3(&mut w, g.normal);
                w.u8(g.resting as u8);
                w.u16(g.health);
            }
            News::Gone { id, why } => {
                w.u8(GONE);
                w.u32(*id);
                w.u8(*why);
            }
            News::Blast { kind, pos } => {
                w.u8(BLAST);
                w.u8(*kind);
                f3(&mut w, *pos);
            }
            News::Shot { id, yaw, pitch } => {
                w.u8(SHOT);
                w.u32(*id);
                w.u16(*yaw);
                w.u16(*pitch as u16);
            }
            News::State { id, owner, faction, health } => {
                w.u8(STATE);
                w.u32(*id);
                w.u16(*owner);
                w.u8(*faction);
                w.u16(*health);
            }
            News::Spotted(ids) => {
                w.u8(SPOTTED);
                w.u8(ids.len().min(255) as u8);
                for &e in ids.iter().take(255) {
                    w.u16(e);
                }
            }
        }
    }
    w.into_inner()
}

pub fn decode_news(data: &[u8]) -> Result<Vec<News>, lattice_net::wire::DecodeError> {
    use lattice_net::wire::{DecodeError, Reader};
    let mut r = Reader::new(data);
    if r.u8()? != MSG_GADGETS {
        return Err(DecodeError::Invalid);
    }
    let n = r.u8()?;
    let f3 = |r: &mut Reader| -> Result<[f32; 3], DecodeError> { Ok([f32::from_bits(r.u32()?), f32::from_bits(r.u32()?), f32::from_bits(r.u32()?)]) };
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        out.push(match r.u8()? {
            SPAWN => {
                let (id, kind, owner, faction, born, at) = (r.u32()?, r.u8()?, r.u16()?, r.u8()?, r.u32()?, r.u32()?);
                let (pos, vel, normal) = (f3(&mut r)?, f3(&mut r)?, f3(&mut r)?);
                let (resting, health) = (r.u8()? != 0, r.u16()?);
                News::Spawn(Gadget { id, kind, owner, faction, pos, vel, normal, resting, born, at, health })
            }
            GONE => News::Gone { id: r.u32()?, why: r.u8()? },
            BLAST => News::Blast { kind: r.u8()?, pos: f3(&mut r)? },
            SHOT => News::Shot { id: r.u32()?, yaw: r.u16()?, pitch: r.u16()? as i16 },
            STATE => News::State { id: r.u32()?, owner: r.u16()?, faction: r.u8()?, health: r.u16()? },
            SPOTTED => {
                let k = r.u8()?;
                let mut ids = Vec::with_capacity(k as usize);
                for _ in 0..k {
                    ids.push(r.u16()?);
                }
                News::Spotted(ids)
            }
            _ => return Err(DecodeError::Invalid),
        });
    }
    r.finish()?;
    Ok(out)
}

/// What killed someone (`events::Recap::cause`) when it was a gadget: 200 + its kind.
pub const fn cause_of(kind: u8) -> u8 {
    200 + kind
}

#[cfg(test)]
mod news_tests {
    use super::*;

    #[test]
    fn news_roundtrips() {
        let g = Gadget::placed(77, TURRET, 12, 2, [1.0, 2.5, 3.25], 400);
        let news = vec![
            News::Spawn(g),
            News::Gone { id: 5, why: GONE_BURST },
            News::Blast { kind: FRAG, pos: [9.0, 8.0, 7.0] },
            News::Shot { id: 77, yaw: 1000, pitch: -300 },
            News::State { id: 77, owner: 13, faction: 1, health: 150 },
            News::Spotted(vec![3, 4, 900]),
        ];
        assert_eq!(decode_news(&encode_news(&news)).unwrap(), news);
    }
}
