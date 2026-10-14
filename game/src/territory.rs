//! Territory (GDD §3.2-§3.3, U5): who holds each site, its control points
//! and its control meter, by the lattice's rules. The server runs it
//! (`Territory::update`, from who stands on the points) and tells clients
//! (`MSG_TERRITORY`); clients keep the same state to draw it and to know
//! which shields let them through (`Territory::owners_at`).
//!
//! - A site's points (1-5, `World::points`) each belong to the faction with
//!   players on it while the others are absent; a contested point freezes:
//!   it counts for no one, and while no one's ahead the meter holds.
//! - Holding more of a site's points than its owner moves its control meter
//!   toward you, if you may attack it: it isn't a Landing, it didn't flip in
//!   the last `LOCK_STEPS`, and it's linked by a lane to a site you hold. The
//!   owner holding as many back pulls the meter home.
//! - When the meter fills, the site flips. A flip takes effect `FLIP_DELAY`
//!   steps after the server decides it, so every client hears of it before
//!   it changes a shield under anyone's feet (prediction stays exact).
//!
//! On Kiln each sector's sites start as its faction's (sector k: faction k)
//! and the Hollow as no one's. A world without lanes (Harrow) has nothing to
//! capture: every site no one's, none attackable.
//!
//! ```text
//! S->C reliable  Territory tag | kind:1 | step:4 | n:1 | n × record
//!   kind 1 (Owners), at join and on flips: site:2 owner:1 prev:1 since:4
//!   kind 2 (Fronts), twice a second, the sites whose fight changed:
//!                  site:2 attacker:1 meter:1 points:2 (2 bits a point: 0 none,
//!                  1-3 faction+1) contested:1 (a bit a point) locked:1
//! ```

use lattice_net::wire::{DecodeError, Reader, Writer};

use crate::faction::FACTIONS;
use crate::world::{SiteKind, World, NO_SECTOR};

pub const MSG_TERRITORY: u8 = 9;
const OWNERS: u8 = 1;
const FRONTS: u8 = 2;

/// No faction: a site no one holds, a point no one's on, no attacker.
pub const NONE: u8 = u8::MAX;
/// The most points a site has.
pub const MAX_POINTS: usize = 6;
/// Steps between updates (twice a second).
pub const UPDATE_STEPS: u32 = 15;
/// Steps between a flip being decided and taking effect: a second.
pub const FLIP_DELAY: u32 = 30;
/// Steps a site can't be attacked after it flips: three minutes.
pub const LOCK_STEPS: u32 = 30 * 180;
/// The meter's full scale.
pub const FULL: u32 = 255 << 10;
/// A point's capture area: within this of it on the ground, and this far
/// above or below its height.
pub const POINT_RADIUS: f32 = 8.0;
pub const POINT_HEIGHT: f32 = 4.0;

/// Whether a site of this kind is spawned at (it has a spawn room): the
/// Landings, the bases, the great sites (GDD §3.4).
pub fn spawns_at(kind: SiteKind) -> bool {
    kind == SiteKind::Landing || kind == SiteKind::Hollow || kind.base() || kind.major()
}

/// Where at each site someone spawning there stands (U4's deploy screen
/// will pick the site; the server's `--spawn sites` does now): its routes'
/// waypoints on the ground within it, near its center first (empty for a
/// site no one spawns at).
pub fn spawn_points(w: &World) -> Vec<Vec<[f32; 2]>> {
    w.sites()
        .iter()
        .map(|s| {
            if !spawns_at(s.kind) {
                return Vec::new();
            }
            let mut v: Vec<(f32, [f32; 2])> = w
                .lanes()
                .iter()
                .filter(|l| l.when == crate::world::ALWAYS)
                .flat_map(|l| l.path.iter())
                .map(|p| (((p[0] - s.center[0]) * (p[0] - s.center[0]) + (p[1] - s.center[1]) * (p[1] - s.center[1])).sqrt(), p))
                .filter(|(d, p)| *d <= s.half.min(120.0) && (p[2] - w.terrain(p[0], p[1])).abs() < 1.0 && firm(w, p))
                .map(|(d, p)| (d, [p[0], p[1]]))
                .collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v.dedup_by(|a, b| (a.1[0] - b.1[0]).abs() < 4.0 && (a.1[1] - b.1[1]).abs() < 4.0);
            v.into_iter().take(24).map(|(_, p)| p).collect()
        })
        .collect()
}

/// Whether a spawn can stand anywhere within its scatter (2 m) of `p`, with
/// room: on ground at `p`'s height (no hole), nothing that moves anyone (a
/// spring's pad, a lift's foot) within 4 m.
fn firm(w: &World, p: &[f32; 3]) -> bool {
    (0..=16).all(|j| {
        let (r, a) = if j == 0 { (0.0, 0.0) } else if j <= 8 { (2.0, j as f32) } else { (4.0, j as f32 + 0.5) };
        let a = a * std::f32::consts::FRAC_PI_4;
        let (x, y) = (p[0] + r * libm::cosf(a), p[1] + r * libm::sinf(a));
        let still = (-1..=3).all(|dz| w.volume_at(x, y, p[2] + dz as f32, crate::clock::Live::OFF).is_none());
        still && (r > 2.0 || (!w.hole(x, y) && (w.ground(x, y, p[2] + 0.5, 1.0, crate::clock::Live::OFF) - p[2]).abs() < 1.0))
    })
}

/// Seconds to flip a site, uncontested (GDD §3.3).
pub fn flip_secs(kind: SiteKind) -> u32 {
    match kind {
        SiteKind::Hollow => 300,
        k if k.major() => 240,
        k if k.base() => 210,
        _ => 120,
    }
}

/// A site's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteState {
    /// Its holder from step `since` on, and before that `prev`.
    pub owner: u8,
    pub prev: u8,
    pub since: u32,
    /// The faction the meter is moving toward, and how far (of `FULL`).
    pub attacker: u8,
    pub meter: u32,
    /// Who holds each point, and which are contested.
    pub points: [u8; MAX_POINTS],
    pub contested: u8,
    /// No attacks until this step.
    pub locked_until: u32,
}

impl SiteState {
    /// Its holder at step `step`.
    pub fn owner_at(&self, step: u32) -> u8 {
        if step >= self.since {
            self.owner
        } else {
            self.prev
        }
    }
}

/// A site's fixed facts.
#[derive(Debug, Clone)]
struct SiteInfo {
    landing: bool,
    /// Steps to flip it, uncontested.
    flip_steps: u32,
    /// Its points' positions.
    points: Vec<[f32; 3]>,
    /// The sites lanes link it to.
    links: Vec<u16>,
}

pub struct Territory {
    info: Vec<SiteInfo>,
    pub sites: Vec<SiteState>,
}

impl Territory {
    /// The territory of `world` at its start.
    pub fn new(world: &World) -> Self {
        Self::with_speed(world, 1)
    }

    /// The same, its meters `speed` times faster (tests).
    pub fn with_speed(world: &World, speed: u32) -> Self {
        let n = world.sites().len();
        let mut info: Vec<SiteInfo> = world
            .sites()
            .iter()
            .map(|s| SiteInfo { landing: s.kind == SiteKind::Landing, flip_steps: (flip_secs(s.kind) * 30 / speed.max(1)).max(1), points: Vec::new(), links: Vec::new() })
            .collect();
        for p in world.points() {
            if let Some(i) = info.get_mut(p.site as usize) {
                if i.points.len() < MAX_POINTS {
                    i.points.push(p.pos);
                }
            }
        }
        for l in world.lanes() {
            if l.a != l.b && (l.a as usize) < n && (l.b as usize) < n {
                for (x, y) in [(l.a, l.b), (l.b, l.a)] {
                    if !info[x as usize].links.contains(&y) {
                        info[x as usize].links.push(y);
                    }
                }
            }
        }
        for i in &mut info {
            i.links.sort_unstable();
        }
        let sites = world
            .sites()
            .iter()
            .map(|s| {
                let owner = if s.sector != NO_SECTOR && (s.sector as u16) < FACTIONS { s.sector } else { NONE };
                SiteState { owner, prev: owner, since: 0, attacker: NONE, meter: 0, points: [NONE; MAX_POINTS], contested: 0, locked_until: 0 }
            })
            .collect();
        Self { info, sites }
    }

    pub fn len(&self) -> usize {
        self.sites.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// Site `i`'s points' positions.
    pub fn points(&self, i: usize) -> &[[f32; 3]] {
        &self.info[i].points
    }

    /// The sites lanes link site `i` to.
    pub fn links(&self, i: usize) -> &[u16] {
        &self.info[i].links
    }

    /// Every site's holder at step `step`, into `out` (resized).
    pub fn owners_at(&self, step: u32, out: &mut Vec<u8>) {
        out.clear();
        out.extend(self.sites.iter().map(|s| s.owner_at(step)));
    }

    /// Whether `faction` may attack site `i` at step `step`: not a Landing,
    /// not just flipped, not its own, and linked to a site it holds.
    pub fn attackable(&self, i: usize, faction: u8, step: u32) -> bool {
        let s = &self.sites[i];
        !self.info[i].landing
            && step >= s.locked_until
            && s.owner_at(step) != faction
            && self.info[i].links.iter().any(|&j| self.sites[j as usize].owner_at(step) == faction)
    }

    /// One update at step `step` (every `UPDATE_STEPS`), `dt` seconds after
    /// the last: `presence(site, point)` counts each faction's living players
    /// on that point. Returns the sites that flipped, and marks in `changed`
    /// the sites whose fight changed (for `Fronts`).
    pub fn update(&mut self, step: u32, dt_steps: u32, presence: &mut dyn FnMut(usize, usize) -> [u16; 3], changed: &mut Vec<u16>) -> Vec<u16> {
        let mut flipped = Vec::new();
        changed.clear();
        for i in 0..self.sites.len() {
            let before = self.sites[i];
            let n = self.info[i].points.len();
            // The points.
            let mut contested = 0u8;
            for p in 0..n {
                let c = presence(i, p);
                let on: Vec<u8> = (0..3u8).filter(|&f| c[f as usize] > 0).collect();
                match on.len() {
                    0 => {}
                    1 => self.sites[i].points[p] = on[0],
                    _ => contested |= 1 << p,
                }
            }
            self.sites[i].contested = contested;
            // The meter.
            let owner = self.sites[i].owner_at(step);
            // (A contested point counts for no one.)
            let held = |f: u8| (0..n).filter(|&p| self.sites[i].points[p] == f && contested & (1 << p) == 0).count();
            let own = if owner == NONE { 0 } else { held(owner) };
            // The strongest attacker that may attack (ties: none).
            let mut best: (u8, usize, bool) = (NONE, 0, false);
            for f in 0..3u8 {
                if f == owner || !self.attackable(i, f, step) {
                    continue;
                }
                let h = held(f);
                if h > best.1 {
                    best = (f, h, false);
                } else if h == best.1 && h > 0 {
                    best.2 = true;
                }
            }
            let ahead = (best.0 != NONE && !best.2 && best.1 > own).then_some(best.0);
            let step_gain = (FULL * dt_steps).div_ceil(self.info[i].flip_steps);
            let s = &mut self.sites[i];
            match ahead {
                Some(f) if s.attacker == f || s.meter == 0 => {
                    s.attacker = f;
                    s.meter = (s.meter + step_gain).min(FULL);
                }
                // Another attacker's progress drains first.
                Some(_) => s.meter = s.meter.saturating_sub(step_gain),
                // A contested site holds its meter; else it drains home.
                None if contested != 0 => {}
                None => s.meter = s.meter.saturating_sub(step_gain),
            }
            if s.meter == 0 && ahead.is_none() {
                s.attacker = NONE;
            }
            if s.meter >= FULL && s.attacker != NONE {
                s.prev = owner;
                s.owner = s.attacker;
                s.since = step + FLIP_DELAY;
                s.locked_until = s.since + LOCK_STEPS;
                s.attacker = NONE;
                s.meter = 0;
                flipped.push(i as u16);
            }
            if *s != before {
                changed.push(i as u16);
            }
        }
        flipped
    }

    /// The Owners message: every site's holder (at join), or `sites`'.
    pub fn encode_owners(&self, step: u32, sites: Option<&[u16]>) -> Vec<u8> {
        let all: Vec<u16> = (0..self.sites.len() as u16).collect();
        let list = sites.unwrap_or(&all);
        let mut w = Writer::with_capacity(7 + list.len() * 8);
        w.u8(MSG_TERRITORY);
        w.u8(OWNERS);
        w.u32(step);
        w.u8(list.len() as u8);
        for &i in list {
            let s = &self.sites[i as usize];
            w.u16(i);
            w.u8(s.owner);
            w.u8(s.prev);
            w.u32(s.since);
        }
        w.into_inner()
    }

    /// The Fronts message: `sites`' fights (meter, points).
    pub fn encode_fronts(&self, step: u32, sites: &[u16]) -> Vec<u8> {
        let mut w = Writer::with_capacity(7 + sites.len() * 8);
        w.u8(MSG_TERRITORY);
        w.u8(FRONTS);
        w.u32(step);
        w.u8(sites.len() as u8);
        for &i in sites {
            let s = &self.sites[i as usize];
            w.u16(i);
            w.u8(s.attacker);
            w.u8((s.meter * 255 / FULL) as u8);
            let mut bits = 0u16;
            for (p, &h) in s.points.iter().enumerate() {
                let v = if h == NONE { 0 } else { (h as u16 + 1) & 3 };
                bits |= v << (2 * p);
            }
            w.u16(bits);
            w.u8(s.contested);
            w.u8((s.locked_until > step) as u8);
        }
        w.into_inner()
    }

    /// Applies a Territory message (a client's copy).
    pub fn apply(&mut self, data: &[u8]) -> Result<(), DecodeError> {
        let mut r = Reader::new(data);
        if r.u8()? != MSG_TERRITORY {
            return Err(DecodeError::Invalid);
        }
        let kind = r.u8()?;
        let step = r.u32()?;
        let n = r.u8()?;
        for _ in 0..n {
            let i = r.u16()? as usize;
            match kind {
                OWNERS => {
                    let (owner, prev, since) = (r.u8()?, r.u8()?, r.u32()?);
                    if let Some(s) = self.sites.get_mut(i) {
                        (s.owner, s.prev, s.since) = (owner, prev, since);
                        if owner != prev {
                            s.locked_until = since + LOCK_STEPS;
                        }
                    }
                }
                FRONTS => {
                    let (attacker, meter, bits, contested, locked) = (r.u8()?, r.u8()?, r.u16()?, r.u8()?, r.u8()?);
                    if let Some(s) = self.sites.get_mut(i) {
                        s.attacker = attacker;
                        s.meter = meter as u32 * FULL / 255;
                        for p in 0..MAX_POINTS {
                            let v = (bits >> (2 * p)) & 3;
                            s.points[p] = if v == 0 { NONE } else { v as u8 - 1 };
                        }
                        s.contested = contested;
                        if locked == 0 && s.locked_until > step {
                            s.locked_until = step;
                        }
                    }
                }
                _ => return Err(DecodeError::Invalid),
            }
        }
        r.finish()?;
        Ok(())
    }
}

/// Counts living players on each of a territory's points, from their feet
/// (`players`: position, faction): a helper for servers and tests.
pub fn presence_of(points: &[[f32; 3]], p: usize, players: &mut dyn Iterator<Item = ([f32; 3], u8)>) -> [u16; 3] {
    let mut c = [0u16; 3];
    let q = points[p];
    for (pos, f) in players {
        let (dx, dy) = (pos[0] - q[0], pos[1] - q[1]);
        if dx * dx + dy * dy <= POINT_RADIUS * POINT_RADIUS && (pos[2] - q[2]).abs() <= POINT_HEIGHT && (f as usize) < 3 {
            c[f as usize] += 1;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::KILN;

    fn kiln() -> (std::sync::Arc<World>, Territory) {
        let w = World::shared(KILN);
        let t = Territory::new(&w);
        (w, t)
    }

    /// Runs updates for `secs` seconds with `on` (site, faction): that
    /// faction alone on every point of the site.
    fn hold(t: &mut Territory, step: &mut u32, secs: u32, on: &[(usize, u8)]) -> Vec<u16> {
        let mut flipped = Vec::new();
        let mut changed = Vec::new();
        for _ in 0..secs * 2 {
            *step += UPDATE_STEPS;
            let mut presence = |i: usize, _p: usize| {
                let mut c = [0u16; 3];
                for &(s, f) in on {
                    if s == i {
                        c[f as usize] += 1;
                    }
                }
                c
            };
            flipped.extend(t.update(*step, UPDATE_STEPS, &mut presence, &mut changed));
        }
        flipped
    }

    #[test]
    fn kiln_starts_with_each_sector_its_factions() {
        let (w, t) = kiln();
        for (i, s) in w.sites().iter().enumerate() {
            let want = if s.sector == NO_SECTOR { NONE } else { s.sector };
            assert_eq!(t.sites[i].owner, want);
            assert!(!t.points(i).is_empty() || s.kind == SiteKind::Landing, "{:?} has points", s.kind);
        }
        // The Hollow is linked to all three sectors.
        let factions: Vec<u8> = t.links(0).iter().map(|&j| t.sites[j as usize].owner).collect();
        for f in 0..3 {
            assert!(factions.contains(&f));
        }
    }

    #[test]
    fn sites_flip_by_the_lattices_rules() {
        let (w, mut t) = kiln();
        let mut step = 0;
        // Landings can't be taken.
        let landing = w.sites().iter().position(|s| s.kind == SiteKind::Landing && s.sector == 1).unwrap();
        assert!(!t.attackable(landing, 0, step));
        // The Hollow: faction 0 takes it in 5 min alone on its points.
        let flipped = hold(&mut t, &mut step, flip_secs(SiteKind::Hollow) - 2, &[(0, 0)]);
        assert!(flipped.is_empty(), "not yet: {:?} meter {}", t.sites[0], t.sites[0].meter);
        let flipped = hold(&mut t, &mut step, 3, &[(0, 0)]);
        assert_eq!(flipped, vec![0]);
        assert_eq!(t.sites[0].owner, 0);
        let since = t.sites[0].since;
        assert_eq!((t.sites[0].prev, t.sites[0].owner_at(since - 1), t.sites[0].owner_at(since)), (NONE, NONE, 0), "not until FLIP_DELAY after");
        // Locked: faction 1 standing there now gains nothing.
        hold(&mut t, &mut step, 60, &[(0, 1)]);
        assert_eq!(t.sites[0].meter, 0, "locked for three minutes");
        // A site deep in sector 1, linked to nothing faction 0 holds: no.
        let deep = w.sites().iter().position(|s| s.kind == SiteKind::Landing && s.sector == 1).unwrap();
        let inner = t.links(deep)[0] as usize;
        assert!(!t.attackable(inner, 0, step), "{:?} isn't linked to anything of faction 0's", w.sites()[inner].kind);
        // A contested site freezes: two factions on its one point.
        let outpost = (0..t.len()).find(|&i| t.points(i).len() == 1 && t.attackable(i, 2, step) && t.sites[i].owner_at(step) != 2).expect("an outpost faction 2 may attack");
        hold(&mut t, &mut step, 10, &[(outpost, 2)]);
        let m = t.sites[outpost].meter;
        assert!(m > 0);
        let defender = t.sites[outpost].owner_at(step);
        hold(&mut t, &mut step, 10, &[(outpost, 2), (outpost, defender)]);
        assert_eq!(t.sites[outpost].meter, m, "a contested point freezes the meter");
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn every_site_can_be_taken_from_its_neighbors() {
        // Spread out from each faction's sector: every site but the Landings
        // falls to some faction attacking from a linked site it holds.
        let (w, mut t) = kiln();
        let mut step = 0;
        let mut taken = vec![false; t.len()];
        for round in 0..t.len() {
            let mut any = false;
            for i in 0..t.len() {
                if taken[i] || w.sites()[i].kind == SiteKind::Landing {
                    continue;
                }
                let Some(f) = (0..3u8).find(|&f| t.attackable(i, f, step)) else { continue };
                let secs = flip_secs(w.sites()[i].kind) + 2;
                let flipped = hold(&mut t, &mut step, secs, &[(i, f)]);
                assert!(flipped.contains(&(i as u16)), "site {i} ({:?}) to faction {f} in round {round}", w.sites()[i].kind);
                step += LOCK_STEPS; // let locks lapse
                taken[i] = true;
                any = true;
            }
            if !any {
                break;
            }
        }
        let left: Vec<_> = (0..t.len()).filter(|&i| !taken[i] && w.sites()[i].kind != SiteKind::Landing).collect();
        assert!(left.is_empty(), "never attackable: {left:?}");
    }

    #[test]
    fn messages_round_trip() {
        let (_, mut t) = kiln();
        let mut step = 0;
        hold(&mut t, &mut step, 30, &[(0, 1)]);
        t.sites[3].points[1] = 2;
        t.sites[3].contested = 0b10;
        let mut copy = Territory::new(&World::shared(KILN));
        copy.apply(&t.encode_owners(step, None)).unwrap();
        let all: Vec<u16> = (0..t.len() as u16).collect();
        copy.apply(&t.encode_fronts(step, &all)).unwrap();
        for i in 0..t.len() {
            let (a, b) = (&t.sites[i], &copy.sites[i]);
            assert_eq!((a.owner, a.prev, a.since, a.attacker, a.points, a.contested), (b.owner, b.prev, b.since, b.attacker, b.points, b.contested));
            assert!(a.meter.abs_diff(b.meter) <= FULL / 255, "the meter to a 255th");
        }
    }
}
