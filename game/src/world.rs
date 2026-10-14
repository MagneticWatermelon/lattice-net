//! The world, built from its id identically on the server and every client:
//! a heightmap, sites on it, and cover boxes.
//!
//! An id names a continent: `KILN` is the desert continent (`crate::kiln`,
//! built to its design), and any other id is a seed for Harrow, the
//! procedural hills (bases and outposts on flattened ground, and a scatter of
//! cover boxes).
//!
//! Harrow's generation is integer-only (hashed value noise in fixed point),
//! Kiln's uses basic f64 arithmetic and `libm`, and sampling uses a fixed
//! sequence of f32 operations (Rust doesn't contract to FMA), so every
//! machine gets the same bits. Movement (`movement.rs`) samples this world on
//! both sides, so prediction stays bit-exact.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::clock::Live;
use crate::movement::{Mover, WORLD_SIZE};

/// The world id of Kiln, the desert continent ("KILN" in ASCII). Any other
/// id is a seed for Harrow.
pub const KILN: u64 = 0x4B49_4C4E;

/// Heightmap sample spacing, in meters.
pub const TERRAIN_RES: f32 = 4.0;
/// Samples per side: the world's edges are samples.
pub const TERRAIN_N: usize = (WORLD_SIZE / TERRAIN_RES) as usize + 1;
/// Heights are stored in centimeters.
const CM: f32 = 0.01;
/// A height cell that spans more than this is a cliff (`World::cliff`).
pub const CLIFF_RISE: f32 = 6.0;
/// Spacing of the cover lattice (a box per cell, sometimes) and its index.
const COVER_CELL: f32 = 32.0;
const COVER_DIM: usize = (WORLD_SIZE / COVER_CELL) as usize;
/// A base per region of this side (16 on the map), and maybe an outpost per
/// region of this side; their flat squares' half-sides; the blend from a
/// site's flat ground back to the hills.
const BASE_REGION: f32 = 2048.0;
const OUTPOST_REGION: f32 = 1024.0;
const BASE_HALF: f32 = 52.0;
const OUTPOST_HALF: f32 = 24.0;
pub const SITE_BLEND: f32 = 40.0;
const CONTAINER_HEIGHT: f32 = 2.6;

/// One building of a site's template, in the site's frame (before its turn).
struct Piece {
    kind: Kind,
    at: [f32; 2],
    size: [f32; 2],
    height: f32,
    facing: u8,
    /// On top of the container below it.
    stacked: bool,
    /// Also placed a quarter, half and three quarters around the center.
    around: bool,
}

const fn piece(kind: Kind, at: [f32; 2], size: [f32; 2], height: f32, facing: u8) -> Piece {
    Piece { kind, at, size, height, facing, stacked: false, around: false }
}

const fn around(p: Piece) -> Piece {
    Piece { around: true, ..p }
}

const fn stacked(p: Piece) -> Piece {
    Piece { stacked: true, ..p }
}

/// A walled base, 90 m square with a 10 m gate on each side: a command
/// building in the middle, bunkers, containers (some stacked), guard posts
/// by the gates, sandbags in front of them and crates.
const BASE: [Piece; 18] = [
    // Each side's perimeter: two 40 m walls either side of the gate.
    around(piece(Kind::Wall, [-25.0, 45.0], [40.0, 0.6], 4.0, 1)),
    around(piece(Kind::Wall, [25.0, 45.0], [40.0, 0.6], 4.0, 1)),
    around(piece(Kind::Post, [-8.5, 41.0], [3.0, 3.0], 3.6, 1)),
    around(piece(Kind::Sandbags, [-7.5, 50.0], [3.0, 1.0], 1.0, 1)),
    around(piece(Kind::Sandbags, [7.5, 50.0], [3.0, 1.0], 1.0, 1)),
    around(piece(Kind::Sandbags, [0.0, 36.0], [3.0, 1.0], 1.0, 1)),
    piece(Kind::Command, [0.0, 0.0], [16.0, 10.0], 5.0, 3),
    piece(Kind::Bunker, [-26.0, 24.0], [8.0, 6.0], 3.0, 3),
    piece(Kind::Bunker, [26.0, -24.0], [8.0, 6.0], 3.0, 1),
    piece(Kind::Container, [24.0, 20.0], [6.0, 2.4], CONTAINER_HEIGHT, 3),
    piece(Kind::Container, [24.0, 22.4], [6.0, 2.4], CONTAINER_HEIGHT, 1),
    stacked(piece(Kind::Container, [24.0, 20.0], [6.0, 2.4], CONTAINER_HEIGHT, 3)),
    piece(Kind::Container, [-24.0, -20.0], [2.4, 6.0], CONTAINER_HEIGHT, 0),
    piece(Kind::Container, [-20.0, -20.0], [2.4, 6.0], CONTAINER_HEIGHT, 2),
    piece(Kind::Container, [-30.0, 6.0], [6.0, 2.4], CONTAINER_HEIGHT, 1),
    piece(Kind::Crate, [10.0, 14.0], [2.0, 1.2], 1.0, 0),
    piece(Kind::Crate, [-12.0, -9.0], [1.2, 2.0], 1.0, 1),
    piece(Kind::Crate, [6.0, -17.0], [2.0, 1.2], 1.0, 0),
];

/// An outpost, ~40 m across and open: a bunker, a guard post, a stack of
/// containers, sandbag lines and crates.
const OUTPOST: [Piece; 11] = [
    piece(Kind::Bunker, [5.0, 6.0], [8.0, 6.0], 3.0, 3),
    piece(Kind::Post, [-9.0, -9.0], [3.0, 3.0], 3.6, 3),
    piece(Kind::Container, [-8.0, 10.0], [6.0, 2.4], CONTAINER_HEIGHT, 3),
    stacked(piece(Kind::Container, [-8.0, 10.0], [6.0, 2.4], CONTAINER_HEIGHT, 3)),
    piece(Kind::Sandbags, [-3.2, -16.0], [3.0, 1.0], 1.0, 3),
    piece(Kind::Sandbags, [0.0, -16.0], [3.0, 1.0], 1.0, 3),
    piece(Kind::Sandbags, [3.2, -16.0], [3.0, 1.0], 1.0, 3),
    piece(Kind::Sandbags, [16.0, -2.0], [1.0, 3.0], 1.0, 0),
    piece(Kind::Sandbags, [16.0, 1.2], [1.0, 3.0], 1.0, 0),
    piece(Kind::Crate, [12.0, 12.0], [2.0, 1.2], 1.0, 0),
    piece(Kind::Crate, [-15.0, 2.0], [1.2, 2.0], 1.0, 1),
];

/// A static solid: blocks movement and shots (GDD §6.1). Its footprint is a
/// rectangle, turned about the vertical by its `axis` (C1); it stands from
/// `bottom` up to its top, which is level (a box) or rises evenly along its
/// axis (a wedge: a ramp, C1). A shield (C1) blocks all but the faction it
/// lets through, and is never stood on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverBox {
    /// The footprint's bounds (for a box that isn't turned, the footprint).
    pub min: [f32; 2],
    pub max: [f32; 2],
    /// Its base sits at the terrain's lowest corner (or on another box).
    pub bottom: f32,
    /// Its top: a wedge's at the high end of its axis.
    pub top: f32,
    /// What it is, for drawing: collision doesn't care.
    pub kind: Kind,
    /// Which way its front faces, in its own frame: 0 along its axis (east
    /// for a box that isn't turned), 1 a quarter turn left of it (north), 2, 3.
    pub facing: u8,
    /// The footprint's center and its half-sizes along the axis and across.
    pub center: [f32; 2],
    pub half: [f32; 2],
    /// A unit vector: (1, 0) for a box that isn't turned.
    pub axis: [f32; 2],
    /// A wedge's top at the low end of its axis (`-half[0]` along it), rising
    /// evenly to `top` at the other; `top` for a box.
    pub low: f32,
    /// A shield's faction, the one it lets through; `SOLID` for the rest.
    pub pass: u8,
    /// A shield that belongs to a site (`NO_SITE`: none): it lets through
    /// whoever holds the site (`territory`), and `pass` is who held it at
    /// the start.
    pub site: u16,
    /// When it's there (K4's clockwork, `clock`): `ALWAYS`, or `BRIDGE` (a
    /// living bridge's, only at night).
    pub when: u8,
}

/// `CoverBox::site` of a box that belongs to no site.
pub const NO_SITE: u16 = u16::MAX;

/// When a box, a volume or a route is there (`clock::Live::there`): always;
/// a living bridge's, from nightfall to a minute into dawn; a sandfall's of
/// the first or the second set, in turns (`clock::falls`); a vent lift's,
/// always there but running on its cycle (`clock::vent_on`).
pub const ALWAYS: u8 = 0;
pub const BRIDGE: u8 = 1;
pub const FALLS_A: u8 = 2;
pub const FALLS_B: u8 = 3;
pub const VENT: u8 = 4;

/// `CoverBox::pass` for everything but shields: it blocks every faction.
pub const SOLID: u8 = u8::MAX;
/// How far below the terrain's surface the feet must be to be under it (in a
/// tunnel, through a hole): there the surface is a ceiling, not the ground.
/// More than any slope a move can climb in a step, less than any tunnel's
/// depth.
pub const UNDER: f32 = 2.0;

impl CoverBox {
    /// A box that isn't turned, from its footprint's bounds.
    pub fn new(min: [f32; 2], max: [f32; 2], bottom: f32, top: f32, kind: Kind, facing: u8) -> Self {
        let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
        let half = [(max[0] - min[0]) * 0.5, (max[1] - min[1]) * 0.5];
        CoverBox { min, max, bottom, top, kind, facing, center, half, axis: [1.0, 0.0], low: top, pass: SOLID, site: NO_SITE, when: ALWAYS }
    }

    /// A box turned about the vertical: its footprint `half` along `axis` (a
    /// unit vector) and across it, around `center`.
    pub fn turned(center: [f32; 2], half: [f32; 2], axis: [f32; 2], bottom: f32, top: f32, kind: Kind, facing: u8) -> Self {
        let ex = axis[0].abs() * half[0] + axis[1].abs() * half[1];
        let ey = axis[1].abs() * half[0] + axis[0].abs() * half[1];
        let (min, max) = ([center[0] - ex, center[1] - ey], [center[0] + ex, center[1] + ey]);
        CoverBox { min, max, bottom, top, kind, facing, center, half, axis, low: top, pass: SOLID, site: NO_SITE, when: ALWAYS }
    }

    /// This as a wedge: its top `low` at the low end of its axis, rising to `top`.
    pub fn wedge(self, low: f32) -> Self {
        CoverBox { low, ..self }
    }

    /// This as a shield that lets `faction` through.
    pub fn shield(self, faction: u8) -> Self {
        CoverBox { pass: faction, ..self }
    }

    /// This as a shield of site `site`, held by `faction` at the start: it
    /// lets through whoever holds the site.
    pub fn site_shield(self, site: u16, faction: u8) -> Self {
        CoverBox { pass: faction, site, ..self }
    }

    /// This, there only `when` the clockwork says (`ALWAYS`, `BRIDGE`).
    pub fn when(self, when: u8) -> Self {
        CoverBox { when, ..self }
    }

    /// Whether it's there with the clockwork at `live`.
    #[inline]
    pub fn there(&self, live: Live) -> bool {
        self.when == ALWAYS || live.there(self.when)
    }

    /// Whether it stops `who`: there, and not a shield that lets them through.
    #[inline]
    pub fn stops(&self, who: &Mover) -> bool {
        self.there(who.live) && self.blocks_for(who.faction, who.owners)
    }

    pub fn is_turned(&self) -> bool {
        self.axis != [1.0, 0.0]
    }

    pub fn is_wedge(&self) -> bool {
        self.low != self.top
    }

    /// Whether it blocks `faction` (a shield lets its own through, as of
    /// the start).
    #[inline]
    pub fn blocks(&self, faction: u8) -> bool {
        self.blocks_for(faction, &[])
    }

    /// Whether it blocks `faction` while the sites are held by `owners` (by
    /// site; empty: as at the start). No one passes a shield of a site no
    /// one holds.
    #[inline]
    pub fn blocks_for(&self, faction: u8, owners: &[u8]) -> bool {
        if self.pass == SOLID {
            return true;
        }
        let pass = if self.site != NO_SITE { owners.get(self.site as usize).copied().unwrap_or(self.pass) } else { self.pass };
        pass != faction
    }

    /// (x, y) in its frame: along its axis and across, from its center.
    #[inline]
    pub fn local(&self, x: f32, y: f32) -> [f32; 2] {
        let (dx, dy) = (x - self.center[0], y - self.center[1]);
        [dx * self.axis[0] + dy * self.axis[1], dy * self.axis[0] - dx * self.axis[1]]
    }

    /// A direction in its frame, in the world's.
    #[inline]
    pub fn world_dir(&self, l: [f32; 2]) -> [f32; 2] {
        [l[0] * self.axis[0] - l[1] * self.axis[1], l[0] * self.axis[1] + l[1] * self.axis[0]]
    }

    /// Whether (x, y) is on its footprint.
    #[inline]
    pub fn covers(&self, x: f32, y: f32) -> bool {
        if !self.is_turned() {
            return x >= self.min[0] && x <= self.max[0] && y >= self.min[1] && y <= self.max[1];
        }
        let l = self.local(x, y);
        l[0].abs() <= self.half[0] && l[1].abs() <= self.half[1]
    }

    /// Its top at `along` its axis (clamped to its footprint).
    #[inline]
    pub fn top_along(&self, along: f32) -> f32 {
        if self.low == self.top {
            return self.top;
        }
        let a = along.clamp(-self.half[0], self.half[0]);
        self.low + (self.top - self.low) * ((a + self.half[0]) / (2.0 * self.half[0]))
    }

    /// Its top over (x, y) (the footprint's nearest point's, outside it).
    #[inline]
    pub fn top_at(&self, x: f32, y: f32) -> f32 {
        if self.low == self.top {
            return self.top;
        }
        self.top_along(self.local(x, y)[0])
    }

    /// The square of the distance from (x, y) to its footprint (0 on it).
    #[inline]
    pub fn distance2(&self, x: f32, y: f32) -> f32 {
        let l = if self.is_turned() { self.local(x, y) } else { [x - self.center[0], y - self.center[1]] };
        let dx = (l[0].abs() - self.half[0]).max(0.0);
        let dy = (l[1].abs() - self.half[1]).max(0.0);
        dx * dx + dy * dy
    }
}

/// What a force volume does to a player inside it (Kiln's connectors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Force {
    /// An updraft (a vent lift, a lift): carries players up at `vel[2]`.
    Lift = 0,
    /// A drop shaft: a fall slowed to `vel[2]` (negative), one way, down.
    Drop = 1,
    /// A lattice spring: a player who stands on it is thrown at `vel`.
    Spring = 2,
}

/// A force volume: an axis-aligned box of air that moves players in it.
/// It acts on whoever's feet are in it, the same on every machine; a
/// clockwork one (`when`) as the step says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    /// Its bounds; the feet are in it from `min[2]` up to (not at) `max[2]`.
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub force: Force,
    /// What it does: see `Force`.
    pub vel: [f32; 3],
    /// When it's there: `ALWAYS`; `FALLS_A`, `FALLS_B` (a sandfall, a drop
    /// in turns); `VENT` (a vent lift: there always, running on its cycle,
    /// at `phase` steps into it, `clock::vent_on`; resting, it only slows a
    /// fall, to `clock::VENT_SINK`).
    pub when: u8,
    pub phase: u16,
}

impl Volume {
    #[inline]
    pub fn contains(&self, x: f32, y: f32, z: f32) -> bool {
        x >= self.min[0] && x <= self.max[0] && y >= self.min[1] && y <= self.max[1] && z >= self.min[2] && z < self.max[2]
    }
}

/// What a cover box is drawn as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// A concrete wall: the scattered thin walls and bases' perimeters.
    Wall = 0,
    /// A low crate, low enough to jump onto.
    Crate = 1,
    Container = 2,
    /// A low sandbag wall, low enough to jump onto.
    Sandbags = 3,
    /// A guard post by a base's gate.
    Post = 4,
    /// A base's command building.
    Command = 5,
    Bunker = 6,
    /// A blockout volume: a building, a wall, a tower, a deck (Kiln's sites
    /// until they have art).
    Block = 7,
    /// Natural rock: boulders, hoodoos, outcrops, reefs, arches.
    Rock = 8,
    /// Glass and crystal: fulgurite spikes, a Geode's crystals.
    Crystal = 9,
    /// The bones of the Dry Sea's megafauna.
    Bone = 10,
    /// A faction's shield (C1): drawn in its colors, see-through.
    Shield = 11,
    /// Lattice coral: a living bridge (K4), there only at night.
    Coral = 12,
}

/// Which generator made a world: how it's drawn, and what its sites mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Continent {
    Harrow = 0,
    Kiln = 1,
}

/// What a site is (GDD §3.2, §6.4). Harrow has bases and outposts; Kiln has
/// its design's seven kinds, its bases' and outposts' kinds among them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SiteKind {
    Base = 0,
    Outpost = 1,
    /// A faction's home: can't be taken.
    Landing = 2,
    /// Kiln's central crater, where the three rifts meet.
    Hollow = 3,
    /// A bridge fortress halfway along a rift.
    Span = 4,
    /// A fortress built into a rift wall.
    Cliffhold = 5,
    /// A crystal dome in a sector's mesa country.
    Geode = 6,
    MesaCitadel = 7,
    SlotHold = 8,
    RiftLedge = 9,
    Cenote = 10,
    ReefWarren = 11,
    Ossuary = 12,
    GlassVault = 13,
    Arch = 14,
    Vent = 15,
    TunnelMouth = 16,
    SpirePerch = 17,
    WellShrine = 18,
    Wreck = 19,
}

impl SiteKind {
    /// Kiln's major facilities and the Hollow (Landings aren't: they can't be taken).
    pub fn major(self) -> bool {
        matches!(self, SiteKind::Hollow | SiteKind::Span | SiteKind::Cliffhold | SiteKind::Geode)
    }

    /// A base (2-3 points) rather than an outpost (1) or a great site.
    pub fn base(self) -> bool {
        matches!(
            self,
            SiteKind::Base
                | SiteKind::MesaCitadel
                | SiteKind::SlotHold
                | SiteKind::RiftLedge
                | SiteKind::Cenote
                | SiteKind::ReefWarren
                | SiteKind::Ossuary
                | SiteKind::GlassVault
        )
    }
}

/// A site: a base, an outpost, a great site or a Landing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Site {
    pub center: [f32; 2],
    /// Half its size. Harrow's are squares flattened to `level` (the ground
    /// blends back to the hills over `SITE_BLEND` around them).
    pub half: f32,
    /// The height of its main ground.
    pub level: f32,
    pub base: bool,
    pub kind: SiteKind,
    /// The sector it's in (Kiln: 0-2, the factions' starting thirds); `NO_SECTOR`
    /// for the Hollow and on Harrow.
    pub sector: u8,
}

pub const NO_SECTOR: u8 = u8::MAX;

/// A route between two sites (`a` != `b`: a lane of the lattice, GDD §3.2), or
/// inside one (`a` == `b`: e.g. from a rim gate down to a crater's floor). Its
/// path is waypoints to walk, feet heights included; walking it straight from
/// one to the next gets there (`kiln`'s tests walk them all). Where it goes
/// up a lift or down a drop shaft, two waypoints stand one over the other:
/// whoever walks it waits there to be carried (`steer`).
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub a: u16,
    pub b: u16,
    pub path: Vec<[f32; 3]>,
    /// Only walked from `a` to `b` (up a lift, down a drop shaft).
    pub one_way: bool,
    /// When it can be walked (`ALWAYS`; over a living bridge, `BRIDGE`; down
    /// a sandfall, `FALLS_A` or `FALLS_B`): the bots' graph and the
    /// reachability check keep to those there always.
    pub when: u8,
}

impl Lane {
    /// Whether it runs mostly under the ground: a tunnel (Kiln's Undervein)
    /// under a lane of the lattice, not one of its own.
    pub fn tunnel(&self, w: &World) -> bool {
        self.path.iter().filter(|p| w.under(p[0], p[1], p[2])).count() * 3 > self.path.len()
    }

    /// Its length along the path, horizontally.
    pub fn length(&self) -> f32 {
        self.path.windows(2).map(|w| ((w[1][0] - w[0][0]) * (w[1][0] - w[0][0]) + (w[1][1] - w[0][1]) * (w[1][1] - w[0][1])).sqrt()).sum()
    }

    /// Waypoint `i` walking it forward, or backward from its end.
    pub fn waypoint(&self, i: usize, backward: bool) -> [f32; 3] {
        self.path[if backward { self.path.len() - 1 - i } else { i }]
    }

    /// Walking the lane: the stick (world x, y in -127..127) toward the next
    /// waypoint from `pos`, moving `next` on past those within reach; None at
    /// its end. What the tests and the bots that time lanes walk by.
    pub fn steer(&self, pos: [f32; 3], next: &mut usize, backward: bool) -> Option<(i8, i8)> {
        let n = self.path.len();
        while *next < n {
            let w = self.waypoint(*next, backward);
            let (dx, dy) = (w[0] - pos[0], w[1] - pos[1]);
            let d = (dx * dx + dy * dy).sqrt();
            // At its height (a stair's turns pass over the same corners).
            let level = (pos[2] - w[2]).abs() < 2.5;
            // Reached, or passed (beside the next leg already), but only
            // where the path runs on nearly straight: a sharp turn (a stair's
            // corner, a deck's end) is turned at its point.
            let passed = level && *next + 1 < n && *next > 0 && {
                let u = self.waypoint(*next + 1, backward);
                let p = self.waypoint(*next - 1, backward);
                let (sx, sy) = (u[0] - w[0], u[1] - w[1]);
                let (ix, iy) = (w[0] - p[0], w[1] - p[1]);
                let straight = sx * ix + sy * iy > 0.9 * (sx * sx + sy * sy).sqrt() * (ix * ix + iy * iy).sqrt();
                straight && (pos[0] - w[0]) * sx + (pos[1] - w[1]) * sy > 0.0 && d < 6.0
            };
            // Over a waypoint with the next straight over or under it (a
            // spring's pad, a shaft): carried on from it, at any height.
            let carried = d < 1.5 && *next + 1 < n && {
                let u = self.waypoint(*next + 1, backward);
                (u[0] - w[0]).abs() < 0.5 && (u[1] - w[1]).abs() < 0.5
            };
            if (d < 1.5 && level) || passed || carried {
                *next += 1;
                continue;
            }
            if d < 1.5 {
                // Right under it (or over it): carried there, by a lift or a fall.
                return Some((0, 0));
            }
            return Some(((dx / d * 127.0) as i8, (dy / d * 127.0) as i8));
        }
        None
    }
}

/// A control point: where standing holds a site's tap (GDD §3.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub pos: [f32; 3],
    pub site: u16,
}

/// What the ground is made of at a height sample, for drawing (Kiln's
/// layers each have their own: its design's "readable at a glance").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Ground {
    Sand = 0,
    /// The Glasslands' fused glass.
    Glass = 1,
    /// The Dry Sea's salt flats.
    Salt = 2,
    /// Banded red and ochre rock: mesas, cliffs.
    Ochre = 3,
    /// Dark basalt and scree: the rift floors and the Hollow's.
    Basalt = 4,
    /// Pale, wind-polished stone: the Crown and the Spire layer.
    Pale = 5,
    /// A lane's road.
    Road = 6,
    /// A site's ground.
    Pad = 7,
    /// Loose scree and gravel on steep slopes.
    Scree = 8,
}

pub struct World {
    pub seed: u64,
    pub continent: Continent,
    heights: Vec<u16>,
    sites: Vec<Site>,
    boxes: Vec<CoverBox>,
    /// Boxes overlapping each cover cell: `box_items[box_start[c]..box_start[c + 1]]`.
    box_start: Vec<u32>,
    box_items: Vec<u32>,
    /// `Ground` per height sample, for drawing (empty on Harrow: it's drawn
    /// from its slopes).
    ground: Vec<u8>,
    lanes: Vec<Lane>,
    points: Vec<Point>,
    /// Holes in the terrain (tunnel mouths, shafts), a bit per height cell
    /// (`hole`); empty: none.
    holes: Vec<u64>,
    volumes: Vec<Volume>,
    /// Volumes overlapping each cover cell, as for boxes.
    vol_start: Vec<u32>,
    vol_items: Vec<u32>,
    digest: OnceLock<u64>,
}

/// Height cells per side (between samples).
pub const CELLS: usize = TERRAIN_N - 1;

/// What a generator makes, for `World::from_parts`.
pub struct Parts {
    pub id: u64,
    pub continent: Continent,
    /// Centimeters, `TERRAIN_N` × `TERRAIN_N`, row by row (y north).
    pub heights: Vec<u16>,
    pub sites: Vec<Site>,
    pub boxes: Vec<CoverBox>,
    pub ground: Vec<u8>,
    pub lanes: Vec<Lane>,
    pub points: Vec<Point>,
    /// A bit per height cell (`CELLS` × `CELLS`, row by row, 64 a word):
    /// set, the terrain has a hole there. Empty: none.
    pub holes: Vec<u64>,
    pub volumes: Vec<Volume>,
}

pub(crate) fn hash(seed: u64, octave: u64, x: i64, y: i64) -> u64 {
    let mut h = seed
        ^ octave.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (x as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (y as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^ (h >> 33)
}

/// Smoothstep in Q16: t in [0, 65536].
pub(crate) fn smooth(t: i64) -> i64 {
    (((t * t) >> 16) * (3 * 65536 - 2 * t)) >> 16
}

/// Value noise of one octave at sample (x, y): wavelength `w` samples,
/// result in [0, 65535].
pub(crate) fn octave(seed: u64, o: u64, w: i64, x: i64, y: i64) -> i64 {
    let (cx, cy) = (x.div_euclid(w), y.div_euclid(w));
    let (sx, sy) = (smooth((x.rem_euclid(w) << 16) / w), smooth((y.rem_euclid(w) << 16) / w));
    let v = |dx: i64, dy: i64| (hash(seed, o, cx + dx, cy + dy) >> 48) as i64;
    let a = v(0, 0) + (((v(1, 0) - v(0, 0)) * sx) >> 16);
    let b = v(0, 1) + (((v(1, 1) - v(0, 1)) * sx) >> 16);
    a + (((b - a) * sy) >> 16)
}

/// (wavelength in samples, amplitude in cm): rolling hills, ~160 m of relief.
const OCTAVES: [(i64, i64); 8] =
    [(512, 8000), (256, 4000), (128, 2000), (64, 1000), (32, 500), (16, 250), (8, 120), (4, 60)];

impl World {
    /// Generates the world for `seed` (a fraction of a second; see `shared`).
    pub fn generate(seed: u64) -> Self {
        let n = TERRAIN_N;
        let mut heights = vec![0u16; n * n];
        // Rows in parallel: the result doesn't depend on how they're split.
        let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(16);
        let rows_per = n.div_ceil(threads);
        std::thread::scope(|s| {
            for (chunk, rows) in heights.chunks_mut(rows_per * n).enumerate() {
                s.spawn(move || {
                    for (i, h) in rows.iter_mut().enumerate() {
                        let (x, y) = ((i % n) as i64, (chunk * rows_per + i / n) as i64);
                        let cm: i64 = OCTAVES
                            .iter()
                            .enumerate()
                            .map(|(o, &(w, amp))| (octave(seed, o as u64, w, x, y) * amp) >> 16)
                            .sum();
                        *h = cm.clamp(0, u16::MAX as i64) as u16;
                    }
                });
            }
        });
        let mut world = Self {
            seed,
            continent: Continent::Harrow,
            heights,
            sites: Vec::new(),
            boxes: Vec::new(),
            box_start: Vec::new(),
            box_items: Vec::new(),
            ground: Vec::new(),
            lanes: Vec::new(),
            points: Vec::new(),
            holes: Vec::new(),
            volumes: Vec::new(),
            vol_start: Vec::new(),
            vol_items: Vec::new(),
            digest: OnceLock::new(),
        };
        world.place_sites();
        world.place_cover();
        world
    }

    /// The world with id `id`: Kiln for `KILN`, the collision kit's test
    /// level for `kit::KIT`, else Harrow from that seed.
    pub fn build(id: u64) -> Self {
        if id == KILN {
            crate::kiln::generate()
        } else if id == crate::kit::KIT {
            crate::kit::level()
        } else {
            Self::generate(id)
        }
    }

    /// A world from a generator's parts (cover indexed here).
    pub fn from_parts(p: Parts) -> Self {
        assert_eq!(p.heights.len(), TERRAIN_N * TERRAIN_N);
        assert!(p.ground.is_empty() || p.ground.len() == TERRAIN_N * TERRAIN_N);
        assert!(p.holes.is_empty() || p.holes.len() == (CELLS * CELLS).div_ceil(64));
        let mut world = Self {
            seed: p.id,
            continent: p.continent,
            heights: p.heights,
            sites: p.sites,
            boxes: p.boxes,
            box_start: Vec::new(),
            box_items: Vec::new(),
            ground: p.ground,
            lanes: p.lanes,
            points: p.points,
            holes: p.holes,
            volumes: p.volumes,
            vol_start: Vec::new(),
            vol_items: Vec::new(),
            digest: OnceLock::new(),
        };
        world.index_cover();
        world
    }

    /// One shared copy per id per process: the bots of a swarm and the
    /// server's tests all use the same world without regenerating it.
    pub fn shared(id: u64) -> Arc<World> {
        static CACHE: OnceLock<Mutex<HashMap<u64, Weak<World>>>> = OnceLock::new();
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        if let Some(w) = cache.get(&id).and_then(Weak::upgrade) {
            return w;
        }
        let w = Arc::new(World::build(id));
        cache.insert(id, Arc::downgrade(&w));
        w
    }

    /// Terrain height at sample (ix, iy), each < `TERRAIN_N`: the point
    /// (ix, iy) × `TERRAIN_RES`. (Renderers mesh the samples directly.)
    #[inline]
    pub fn sample(&self, ix: usize, iy: usize) -> f32 {
        self.heights[iy * TERRAIN_N + ix] as f32 * CM
    }

    /// Terrain height at (x, y), bilinear between samples. Positions outside
    /// the world are clamped to it.
    pub fn terrain(&self, x: f32, y: f32) -> f32 {
        let last = (TERRAIN_N - 1) as f32;
        let fx = (x.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).min(last);
        let fy = (y.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).min(last);
        let (ix, iy) = ((fx as usize).min(TERRAIN_N - 2), (fy as usize).min(TERRAIN_N - 2));
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let (h00, h10) = (self.sample(ix, iy), self.sample(ix + 1, iy));
        let (h01, h11) = (self.sample(ix, iy + 1), self.sample(ix + 1, iy + 1));
        let a = h00 + (h10 - h00) * tx;
        let b = h01 + (h11 - h01) * tx;
        a + (b - a) * ty
    }

    /// A box in about a quarter of the 32 m lattice cells: walls (thin and
    /// long, 2.5-4 m tall) and crates (low enough to jump onto).
    fn place_cover(&mut self) {
        let seed = self.seed ^ 0xC0FE_C0FE;
        for cy in 0..COVER_DIM as i64 {
            for cx in 0..COVER_DIM as i64 {
                let h = hash(seed, 100, cx, cy);
                if !h.is_multiple_of(4) || self.on_site(cx as f32 * COVER_CELL, cy as f32 * COVER_CELL, COVER_CELL) {
                    continue;
                }
                let r = |shift: u32, lo: f32, hi: f32| lo + ((h >> shift) & 0xFFFF) as f32 / 65535.0 * (hi - lo);
                let wall = (h >> 2) & 1 == 0;
                let (hx, hy, tall) = if wall {
                    let (long, thin) = (r(8, 2.0, 6.0), 0.3);
                    if (h >> 3) & 1 == 0 { (long, thin, r(24, 2.5, 4.0)) } else { (thin, long, r(24, 2.5, 4.0)) }
                } else {
                    (r(8, 0.6, 1.5), r(40, 0.6, 1.5), r(24, 0.8, 1.0))
                };
                let x = cx as f32 * COVER_CELL + r(48, hx + 1.0, COVER_CELL - hx - 1.0);
                let y = cy as f32 * COVER_CELL + r(32, hy + 1.0, COVER_CELL - hy - 1.0);
                let (min, max) = ([x - hx, y - hy], [x + hx, y + hy]);
                let (low, high) = self.ground_under(min, max);
                let kind = if wall { Kind::Wall } else { Kind::Crate };
                let facing = (if hx < hy { 0 } else { 1 }) + 2 * ((h >> 5) & 1) as u8;
                self.boxes.push(CoverBox::new(min, max, low, high + tall, kind, facing));
            }
        }
        for i in 0..self.sites.len() {
            self.build_site(i);
        }
        self.index_cover();
    }

    /// The static index: every box (and force volume) under every cover cell
    /// it overlaps.
    fn index_cover(&mut self) {
        let bounds: Vec<([f32; 2], [f32; 2])> = self.boxes.iter().map(|b| (b.min, b.max)).collect();
        (self.box_start, self.box_items) = index(&bounds);
        let bounds: Vec<([f32; 2], [f32; 2])> = self.volumes.iter().map(|v| ([v.min[0], v.min[1]], [v.max[0], v.max[1]])).collect();
        (self.vol_start, self.vol_items) = index(&bounds);
    }

    pub fn boxes(&self) -> &[CoverBox] {
        &self.boxes
    }

    /// Force volumes: lifts, drop shafts, springs.
    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    /// The force volume the feet at (x, y, z) are in with the clockwork at
    /// `live`, if any (the first there, in the index's order).
    pub fn volume_at(&self, x: f32, y: f32, z: f32, live: Live) -> Option<&Volume> {
        if self.volumes.is_empty() {
            return None;
        }
        let c = |v: f32| ((v.clamp(0.0, WORLD_SIZE) / COVER_CELL) as usize).min(COVER_DIM - 1);
        let cell = c(y) * COVER_DIM + c(x);
        self.vol_items[self.vol_start[cell] as usize..self.vol_start[cell + 1] as usize]
            .iter()
            .map(|&i| &self.volumes[i as usize])
            .find(|v| v.contains(x, y, z) && live.there(v.when))
    }

    /// Whether the terrain has a hole at (x, y): no ground there, open to
    /// what's under the surface.
    #[inline]
    pub fn hole(&self, x: f32, y: f32) -> bool {
        if self.holes.is_empty() {
            return false;
        }
        let c = |v: f32| ((v.clamp(0.0, WORLD_SIZE) / TERRAIN_RES) as usize).min(CELLS - 1);
        self.hole_cell(c(x), c(y))
    }

    /// Whether height cell (cx, cy) (between samples cx..cx+1, cy..cy+1) is a hole.
    #[inline]
    pub fn hole_cell(&self, cx: usize, cy: usize) -> bool {
        let i = cy * CELLS + cx;
        self.holes.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 != 0)
    }

    /// Whether the world has any holes at all.
    pub fn has_holes(&self) -> bool {
        !self.holes.is_empty()
    }

    /// The lowest underside above `above` over the circle of radius `r` at
    /// (x, y): a box's bottom (not a shield's, and one there with the
    /// clockwork at `live`), or the terrain's surface where (x, y) is under
    /// it; `f32::MAX` if there's none. What a head bumps into (C1: ceilings).
    pub fn ceiling(&self, x: f32, y: f32, r: f32, above: f32, live: Live) -> f32 {
        let mut c = f32::MAX;
        if !self.holes.is_empty() {
            let t = self.terrain(x, y);
            if t >= above && !self.hole(x, y) {
                c = t;
            }
        }
        self.boxes_near(x, y, r, |b| {
            if b.pass == SOLID && b.bottom >= above && b.bottom < c && b.there(live) && b.distance2(x, y) < r * r {
                c = b.bottom;
            }
        });
        c
    }

    /// Whether feet at `z` over (x, y) are under the terrain's surface there
    /// (in a tunnel): it's a ceiling over them, not their ground.
    #[inline]
    pub fn under(&self, x: f32, y: f32, z: f32) -> bool {
        !self.holes.is_empty() && self.terrain(x, y) > z + UNDER
    }

    /// Routes along the lattice's lanes, and inside sites (none on Harrow).
    pub fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    /// Control points (none on Harrow yet).
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// What the ground is at sample (ix, iy), as a `Ground`; None where the
    /// world doesn't say (Harrow).
    pub fn ground_at(&self, ix: usize, iy: usize) -> Option<u8> {
        self.ground.get(iy * TERRAIN_N + ix).copied()
    }

    /// Whether (x, y) is on a cliff: its height cell spans more than
    /// `CLIFF_RISE` (no slope a player walks spans that in one 4 m cell).
    pub fn cliff(&self, x: f32, y: f32) -> bool {
        let last = (TERRAIN_N - 1) as f32;
        let fx = (x.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).min(last);
        let fy = (y.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).min(last);
        let (ix, iy) = ((fx as usize).min(TERRAIN_N - 2), (fy as usize).min(TERRAIN_N - 2));
        let h = [self.heights[iy * TERRAIN_N + ix], self.heights[iy * TERRAIN_N + ix + 1], self.heights[(iy + 1) * TERRAIN_N + ix], self.heights[(iy + 1) * TERRAIN_N + ix + 1]];
        let (lo, hi) = h.iter().fold((u16::MAX, 0u16), |(l, m), &v| (l.min(v), m.max(v)));
        (hi - lo) as f32 * CM > CLIFF_RISE
    }

    /// The terrain's slope at (x, y): its gradient, rise per meter along x
    /// and along y, of the bilinear patch there (as `terrain` samples it).
    pub fn gradient(&self, x: f32, y: f32) -> [f32; 2] {
        let last = (TERRAIN_N - 1) as f32;
        let fx = (x.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).min(last);
        let fy = (y.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).min(last);
        let (ix, iy) = ((fx as usize).min(TERRAIN_N - 2), (fy as usize).min(TERRAIN_N - 2));
        let (tx, ty) = (fx - ix as f32, fy - iy as f32);
        let (h00, h10) = (self.sample(ix, iy), self.sample(ix + 1, iy));
        let (h01, h11) = (self.sample(ix, iy + 1), self.sample(ix + 1, iy + 1));
        let dx = ((h10 - h00) * (1.0 - ty) + (h11 - h01) * ty) / TERRAIN_RES;
        let dy = ((h01 - h00) * (1.0 - tx) + (h11 - h10) * tx) / TERRAIN_RES;
        [dx, dy]
    }

    /// Bases and outposts.
    pub fn sites(&self) -> &[Site] {
        &self.sites
    }

    /// The lowest and highest terrain under a box's corners.
    fn ground_under(&self, min: [f32; 2], max: [f32; 2]) -> (f32, f32) {
        let corners = [self.terrain(min[0], min[1]), self.terrain(max[0], min[1]), self.terrain(min[0], max[1]), self.terrain(max[0], max[1])];
        corners.iter().fold((f32::MAX, f32::MIN), |(l, h), &c| (l.min(c), h.max(c)))
    }

    /// Whether the square at (x, y) with side `side` touches a site's flat
    /// square or its blend.
    fn on_site(&self, x: f32, y: f32, side: f32) -> bool {
        self.sites.iter().any(|s| {
            let r = s.half + SITE_BLEND;
            x + side > s.center[0] - r && x < s.center[0] + r && y + side > s.center[1] - r && y < s.center[1] + r
        })
    }

    /// Picks the sites (one base per 2 km region at its flattest candidate
    /// spot; outposts in about half the 1 km regions, away from bases) and
    /// flattens the ground under them.
    fn place_sites(&mut self) {
        let seed = self.seed ^ 0xBA5E_BA5E;
        let regions = |size: f32| (WORLD_SIZE / size) as i64;
        let pick = |this: &Self, o: u64, rx: i64, ry: i64, size: f32, half: f32| -> (f32, [f32; 2], i64) {
            // The flattest of 8 candidates: the smallest height range over
            // the square and its blend.
            let margin = half + SITE_BLEND + 64.0;
            let mut best = (f32::MAX, [0.0; 2], 0);
            for k in 0..8 {
                let h = hash(seed, o, rx * 8 + k, ry);
                let r = |shift: u32| margin + ((h >> shift) & 0xFFFF) as f32 / 65535.0 * (size - 2.0 * margin);
                let c = [rx as f32 * size + r(0), ry as f32 * size + r(16)];
                let reach = half + SITE_BLEND;
                let (mut lo, mut hi, mut sum) = (i64::MAX, i64::MIN, 0i64);
                for j in 0..9 {
                    for i in 0..9 {
                        let (x, y) = (c[0] - reach + reach * i as f32 / 4.0, c[1] - reach + reach * j as f32 / 4.0);
                        let cm = this.heights[this.index(x, y)] as i64;
                        (lo, hi, sum) = (lo.min(cm), hi.max(cm), sum + cm);
                    }
                }
                let range = (hi - lo) as f32;
                if range < best.0 {
                    best = (range, c, sum / 81);
                }
            }
            best
        };
        let mut sites = Vec::new();
        for ry in 0..regions(BASE_REGION) {
            for rx in 0..regions(BASE_REGION) {
                let (_, c, level) = pick(self, 1, rx, ry, BASE_REGION, BASE_HALF);
                sites.push((c, BASE_HALF, level, true));
            }
        }
        for ry in 0..regions(OUTPOST_REGION) {
            for rx in 0..regions(OUTPOST_REGION) {
                if !hash(seed, 3, rx, ry).is_multiple_of(2) {
                    continue;
                }
                let (_, c, level) = pick(self, 2, rx, ry, OUTPOST_REGION, OUTPOST_HALF);
                let clear = sites.iter().all(|&(b, half, _, _): &([f32; 2], f32, i64, bool)| {
                    (b[0] - c[0]).abs().max((b[1] - c[1]).abs()) > half + OUTPOST_HALF + 2.0 * SITE_BLEND + 64.0
                });
                if clear {
                    sites.push((c, OUTPOST_HALF, level, false));
                }
            }
        }
        for &(c, half, level, base) in &sites {
            self.flatten(c, half, level);
            let kind = if base { SiteKind::Base } else { SiteKind::Outpost };
            self.sites.push(Site { center: c, half, level: level as f32 * CM, base, kind, sector: NO_SECTOR });
        }
    }

    /// The heightmap index nearest (x, y).
    fn index(&self, x: f32, y: f32) -> usize {
        let i = |v: f32| ((v.clamp(0.0, WORLD_SIZE) / TERRAIN_RES).round() as usize).min(TERRAIN_N - 1);
        i(y) * TERRAIN_N + i(x)
    }

    /// Sets the ground to `level` (cm) over the square of half-side `half`
    /// around `c`, blending back to the hills over `SITE_BLEND` around it
    /// (smoothstep in integer math, so every machine agrees).
    fn flatten(&mut self, c: [f32; 2], half: f32, level: i64) {
        let res = TERRAIN_RES as i64;
        let (flat, blend) = (half as i64, SITE_BLEND as i64);
        let (cx, cy) = ((c[0] / TERRAIN_RES).round() as i64, (c[1] / TERRAIN_RES).round() as i64);
        let reach = (flat + blend) / res + 1;
        for iy in (cy - reach).max(0)..=(cy + reach).min(TERRAIN_N as i64 - 1) {
            for ix in (cx - reach).max(0)..=(cx + reach).min(TERRAIN_N as i64 - 1) {
                // Distance outside the flat square, in meters (Chebyshev).
                let d = ((ix - cx).abs().max((iy - cy).abs()) * res - flat).max(0);
                if d >= blend {
                    continue;
                }
                let w = 65536 - smooth((d << 16) / blend);
                let h = &mut self.heights[iy as usize * TERRAIN_N + ix as usize];
                let v = *h as i64;
                *h = (v + (((level - v) * w) >> 16)).clamp(0, u16::MAX as i64) as u16;
            }
        }
    }

    /// Places site `i`'s buildings: its template turned by a quarter turn or
    /// more (hash), on its flat ground.
    fn build_site(&mut self, i: usize) {
        let s = self.sites[i];
        let turn = (hash(self.seed ^ 0xBA5E_BA5E, 4, s.center[0] as i64, s.center[1] as i64) & 3) as u8;
        let rot = |p: [f32; 2]| match turn {
            0 => p,
            1 => [-p[1], p[0]],
            2 => [-p[0], -p[1]],
            _ => [p[1], -p[0]],
        };
        let template: &[Piece] = if s.base { &BASE } else { &OUTPOST };
        for p in template {
            for k in 0..if p.around { 4 } else { 1 } {
                // `around`: the piece and its copies a quarter, half and
                // three quarters around the center (gates on every side).
                let spin = |q: [f32; 2]| (0..k).fold(q, |q, _| [-q[1], q[0]]);
                let at = rot(spin(p.at));
                let size = if (turn + k) % 2 == 1 { [p.size[1], p.size[0]] } else { p.size };
                let c = [s.center[0] + at[0], s.center[1] + at[1]];
                let (min, max) = ([c[0] - size[0] / 2.0, c[1] - size[1] / 2.0], [c[0] + size[0] / 2.0, c[1] + size[1] / 2.0]);
                let (low, high) = self.ground_under(min, max);
                let bottom = if p.stacked { low + CONTAINER_HEIGHT } else { low };
                let top = (if p.stacked { bottom } else { high }) + p.height;
                self.boxes.push(CoverBox::new(min, max, bottom, top, p.kind, (p.facing + turn + k) % 4));
            }
        }
    }

    /// Calls `f` with every box whose cover cells overlap the square of
    /// half-size `r` around (x, y), in a fixed order (a box spanning several
    /// cells can come more than once).
    pub fn boxes_near(&self, x: f32, y: f32, r: f32, mut f: impl FnMut(&CoverBox)) {
        let c = |v: f32| ((v.clamp(0.0, WORLD_SIZE) / COVER_CELL) as usize).min(COVER_DIM - 1);
        for cy in c(y - r)..=c(y + r) {
            for cx in c(x - r)..=c(x + r) {
                let cell = cy * COVER_DIM + cx;
                for &i in &self.box_items[self.box_start[cell] as usize..self.box_start[cell + 1] as usize] {
                    f(&self.boxes[i as usize]);
                }
            }
        }
    }

    /// The highest surface at (x, y) a player whose feet are at `z` can stand
    /// on: the terrain, or the top of a box under (x, y) no higher than `z +
    /// reach` (so a player steps up onto low boxes and lands on high ones).
    /// Under the terrain's surface (more than `UNDER` below it) or over a
    /// hole in it, the terrain isn't ground: only boxes are, and with none,
    /// the bottom of the world (0). Shields are never ground, nor boxes the
    /// clockwork at `live` has taken away (a living bridge by day).
    pub fn ground(&self, x: f32, y: f32, z: f32, reach: f32, live: Live) -> f32 {
        let t = self.terrain(x, y);
        let mut g = if self.holes.is_empty() || (t <= z + UNDER.max(reach) && !self.hole(x, y)) { t } else { 0.0 };
        self.boxes_near(x, y, 0.0, |b| {
            if b.pass == SOLID && b.covers(x, y) && b.there(live) {
                let top = b.top_at(x, y);
                if top <= z + reach && top > g {
                    g = top;
                }
            }
        });
        g
    }

    /// A digest of the terrain, cover and sites, to check two machines built
    /// the same world (computed once).
    pub fn digest(&self) -> u64 {
        *self.digest.get_or_init(|| self.compute_digest())
    }

    fn compute_digest(&self) -> u64 {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        let mut eat = |v: u64| {
            h ^= v;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        };
        for &v in &self.heights {
            eat(v as u64);
        }
        for b in &self.boxes {
            for v in [b.min[0], b.min[1], b.max[0], b.max[1], b.bottom, b.top] {
                eat(v.to_bits() as u64);
            }
            eat(b.kind as u64 | (b.facing as u64) << 8);
            if b.is_turned() || b.is_wedge() || b.pass != SOLID {
                for v in [b.center[0], b.center[1], b.half[0], b.half[1], b.axis[0], b.axis[1], b.low] {
                    eat(v.to_bits() as u64);
                }
                eat(b.pass as u64 | (b.site as u64) << 8);
            }
            if b.when != ALWAYS {
                eat(0xC10C_0000 | b.when as u64);
            }
        }
        for (i, &w) in self.holes.iter().enumerate() {
            if w != 0 {
                eat(i as u64);
                eat(w);
            }
        }
        for v in &self.volumes {
            for x in v.min.iter().chain(&v.max).chain(&v.vel) {
                eat(x.to_bits() as u64);
            }
            eat(v.force as u64 | (v.when as u64) << 8 | (v.phase as u64) << 16);
        }
        for s in &self.sites {
            for v in [s.center[0], s.center[1], s.half, s.level] {
                eat(v.to_bits() as u64);
            }
            eat(s.kind as u64 | (s.sector as u64) << 8);
        }
        for l in &self.lanes {
            eat(l.a as u64 | (l.b as u64) << 16 | (l.one_way as u64) << 32 | (l.when as u64) << 40);
            for p in &l.path {
                for v in p {
                    eat(v.to_bits() as u64);
                }
            }
        }
        for p in &self.points {
            for v in p.pos {
                eat(v.to_bits() as u64);
            }
            eat(p.site as u64);
        }
        h
    }
}

/// `Parts::holes` with a hole in each of `cells` (height cells, (x, y)).
pub fn hole_bits(cells: &[(usize, usize)]) -> Vec<u64> {
    let mut bits = vec![0u64; (CELLS * CELLS).div_ceil(64)];
    for &(x, y) in cells {
        let i = y * CELLS + x;
        bits[i / 64] |= 1 << (i % 64);
    }
    bits
}

/// An index over cover cells: each item under every cell its bounds overlap,
/// as (where each cell's items start, and one past the last; the items).
fn index(bounds: &[([f32; 2], [f32; 2])]) -> (Vec<u32>, Vec<u32>) {
    let mut cells: Vec<Vec<u32>> = vec![Vec::new(); COVER_DIM * COVER_DIM];
    for (i, (min, max)) in bounds.iter().enumerate() {
        let c = |v: f32| ((v.max(0.0) / COVER_CELL) as usize).min(COVER_DIM - 1);
        for y in c(min[1])..=c(max[1]) {
            for x in c(min[0])..=c(max[0]) {
                cells[y * COVER_DIM + x].push(i as u32);
            }
        }
    }
    let mut start = Vec::with_capacity(cells.len() + 1);
    let mut items = Vec::new();
    start.push(0);
    for c in &cells {
        items.extend_from_slice(c);
        start.push(items.len() as u32);
    }
    (start, items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_world_and_seeds_differ() {
        let (a, b) = (World::generate(7), World::generate(7));
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), World::generate(8).digest());
        assert!(Arc::ptr_eq(&World::shared(7), &World::shared(7)));
    }

    #[test]
    fn terrain_is_continuous_and_hilly() {
        let w = World::shared(1);
        let (mut lo, mut hi, mut steepest) = (f32::MAX, f32::MIN, 0.0f32);
        for i in 0..2000 {
            let (x, y) = (i as f32 * 4.09, 8000.0 - i as f32 * 3.7);
            let h = w.terrain(x, y);
            let h2 = w.terrain(x + 0.5, y);
            (lo, hi) = (lo.min(h), hi.max(h));
            steepest = steepest.max((h2 - h).abs() / 0.5);
            assert!((h2 - h).abs() < 2.0, "a 0.5 m step can't jump {} m", h2 - h);
        }
        assert!(hi - lo > 20.0, "relief {} m", hi - lo);
        assert!(steepest < 3.0, "slope {steepest}");
        // Samples are hit exactly; edges and beyond clamp.
        assert_eq!(w.terrain(8.0, 12.0), w.sample(2, 3));
        assert_eq!(w.terrain(-5.0, -5.0), w.sample(0, 0));
        assert_eq!(w.terrain(WORLD_SIZE + 9.0, WORLD_SIZE), w.sample(TERRAIN_N - 1, TERRAIN_N - 1));
    }

    #[test]
    fn bases_and_outposts_stand_on_flat_ground() {
        let w = World::shared(1);
        let bases: Vec<_> = w.sites().iter().filter(|s| s.base).collect();
        let outposts = w.sites().len() - bases.len();
        eprintln!("{} bases, {outposts} outposts", bases.len());
        for s in w.sites() {
            eprintln!("  {} at ({:.0}, {:.0}), level {:.1} m", if s.base { "base" } else { "outpost" }, s.center[0], s.center[1], s.level);
        }
        assert_eq!(bases.len(), 16);
        assert!(outposts >= 10, "{outposts} outposts");
        for s in w.sites() {
            // Flat across the square (to the centimeter), and blended back
            // to the hills without cliffs.
            for k in 0..=20 {
                let t = k as f32 / 20.0 * 2.0 - 1.0;
                for (x, y) in [(t, -1.0), (t, 1.0), (-1.0, t), (1.0, t), (t, t)] {
                    let h = w.terrain(s.center[0] + x * (s.half - 4.0), s.center[1] + y * (s.half - 4.0));
                    assert!((h - s.level).abs() < 0.02, "flat: {h} vs level {}", s.level);
                }
            }
            for k in 0..200 {
                let a = k as f32 / 200.0 * std::f32::consts::TAU;
                let r = s.half + SITE_BLEND / 2.0;
                let (x, y) = (s.center[0] + libm::cosf(a) * r, s.center[1] + libm::sinf(a) * r);
                let slope = (w.terrain(x + 0.5, y) - w.terrain(x - 0.5, y)).abs().max((w.terrain(x, y + 0.5) - w.terrain(x, y - 0.5)).abs());
                assert!(slope < 1.0, "walkable blend: slope {slope}");
            }
            // Only its own buildings on it.
            let mut kinds = Vec::new();
            w.boxes_near(s.center[0], s.center[1], s.half, |b| {
                if (b.min[0] + b.max[0]) / 2.0 > s.center[0] - s.half
                    && (b.min[0] + b.max[0]) / 2.0 < s.center[0] + s.half
                    && (b.min[1] + b.max[1]) / 2.0 > s.center[1] - s.half
                    && (b.min[1] + b.max[1]) / 2.0 < s.center[1] + s.half
                {
                    kinds.push(b.kind);
                }
            });
            kinds.sort_by_key(|k| *k as u8);
            kinds.dedup_by_key(|k| *k as u8); // boxes_near repeats boxes spanning cells
            if s.base {
                assert!(kinds.contains(&Kind::Command) && kinds.contains(&Kind::Wall) && kinds.contains(&Kind::Post), "{kinds:?}");
            } else {
                assert!(kinds.contains(&Kind::Bunker) && !kinds.contains(&Kind::Wall), "{kinds:?}");
            }
        }
    }

    #[test]
    fn players_walk_into_a_base_through_its_gates() {
        use crate::movement::{step, Input, MoveState};
        let w = World::shared(1);
        let s = w.sites().iter().find(|s| s.base).unwrap();
        // From 70 m out on each side, straight at the center: through the
        // gate, past the sandbags (around them), up to the command building.
        for (dx, dy) in [(1.0f32, 0.0f32), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            let mut p = MoveState::standing(&w, [s.center[0] + dx * 70.0, s.center[1] + dy * 70.0]);
            let mut inside = false;
            for i in 0..600 {
                let to = [s.center[0] - p.pos[0], s.center[1] - p.pos[1]];
                let d = (to[0] * to[0] + to[1] * to[1]).sqrt();
                // Steer around what's in the way: aim a little off-center now and then.
                let wobble = if (i / 30) % 2 == 0 { 0.6 } else { -0.6 };
                let (c, sn) = libm::sincosf(wobble as f32); // as before: (sin, cos)
                let dir = [(to[0] * sn - to[1] * c) / d, (to[0] * c + to[1] * sn) / d];
                p = step(&w, p, Input { move_x: (dir[0] * 127.0) as i8, move_y: (dir[1] * 127.0) as i8, ..Default::default() }, 0);
                inside |= (p.pos[0] - s.center[0]).abs() < 40.0 && (p.pos[1] - s.center[1]).abs() < 40.0;
            }
            assert!(inside, "got in from ({dx}, {dy}): ended at {:?}", p.pos);
        }
    }

    #[test]
    fn cover_is_scattered_indexed_and_standable() {
        let w = World::shared(1);
        let n = w.boxes().len();
        assert!(n > 10_000 && n < 25_000, "{n} boxes");
        let b = w.boxes()[n / 2];
        let (cx, cy) = ((b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0);
        let mut found = false;
        w.boxes_near(cx, cy, 0.0, |x| found |= *x == b);
        assert!(found, "the index finds a box at its center");
        // On the box's top from above; on the terrain from below its top.
        assert_eq!(w.ground(cx, cy, b.top, 0.5, crate::clock::Live::OFF), b.top);
        assert_eq!(w.ground(cx, cy, b.top - 2.0, 0.5, crate::clock::Live::OFF), w.terrain(cx, cy), "out of reach: the terrain");
    }
}
