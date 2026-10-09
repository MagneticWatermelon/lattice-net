//! The world, built from a seed identically on the server and every client:
//! a heightmap, bases and outposts on flattened ground, and a scatter of
//! cover boxes.
//!
//! Generation is integer-only (hashed value noise in fixed point), and
//! sampling uses a fixed sequence of f32 operations (Rust doesn't contract
//! to FMA), so every machine gets the same bits. Movement (`movement.rs`)
//! samples this world on both sides, so prediction stays bit-exact.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::movement::WORLD_SIZE;

/// Heightmap sample spacing, in meters.
pub const TERRAIN_RES: f32 = 4.0;
/// Samples per side: the world's edges are samples.
pub const TERRAIN_N: usize = (WORLD_SIZE / TERRAIN_RES) as usize + 1;
/// Heights are stored in centimeters.
const CM: f32 = 0.01;
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

/// A static axis-aligned box: blocks movement and shots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverBox {
    pub min: [f32; 2],
    pub max: [f32; 2],
    /// Its base sits at the terrain's lowest corner (or on another box); its
    /// top is level.
    pub bottom: f32,
    pub top: f32,
    /// What it is, for drawing: collision doesn't care.
    pub kind: Kind,
    /// Which way its front faces: 0 east (+x), 1 north (+y), 2 west, 3 south.
    pub facing: u8,
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
}

/// A base or an outpost: a square of flattened ground with its buildings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Site {
    pub center: [f32; 2],
    /// Half the side of its flat square (the ground blends back to the hills
    /// over `SITE_BLEND` around it).
    pub half: f32,
    /// The height it's flattened to.
    pub level: f32,
    pub base: bool,
}

pub struct World {
    pub seed: u64,
    heights: Vec<u16>,
    sites: Vec<Site>,
    boxes: Vec<CoverBox>,
    /// Boxes overlapping each cover cell: `box_items[box_start[c]..box_start[c + 1]]`.
    box_start: Vec<u32>,
    box_items: Vec<u32>,
}

fn hash(seed: u64, octave: u64, x: i64, y: i64) -> u64 {
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
fn smooth(t: i64) -> i64 {
    (((t * t) >> 16) * (3 * 65536 - 2 * t)) >> 16
}

/// Value noise of one octave at sample (x, y): wavelength `w` samples,
/// result in [0, 65535].
fn octave(seed: u64, o: u64, w: i64, x: i64, y: i64) -> i64 {
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
        let mut world = Self { seed, heights, sites: Vec::new(), boxes: Vec::new(), box_start: Vec::new(), box_items: Vec::new() };
        world.place_sites();
        world.place_cover();
        world
    }

    /// One shared copy per seed per process: the bots of a swarm and the
    /// server's tests all use the same world without regenerating it.
    pub fn shared(seed: u64) -> Arc<World> {
        static CACHE: OnceLock<Mutex<HashMap<u64, Weak<World>>>> = OnceLock::new();
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        if let Some(w) = cache.get(&seed).and_then(Weak::upgrade) {
            return w;
        }
        let w = Arc::new(World::generate(seed));
        cache.insert(seed, Arc::downgrade(&w));
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
                self.boxes.push(CoverBox { min, max, bottom: low, top: high + tall, kind, facing });
            }
        }
        for i in 0..self.sites.len() {
            self.build_site(i);
        }
        // Static index: every box under every cover cell it overlaps.
        let mut cells: Vec<Vec<u32>> = vec![Vec::new(); COVER_DIM * COVER_DIM];
        for (i, b) in self.boxes.iter().enumerate() {
            let c = |v: f32| ((v / COVER_CELL) as usize).min(COVER_DIM - 1);
            for y in c(b.min[1])..=c(b.max[1]) {
                for x in c(b.min[0])..=c(b.max[0]) {
                    cells[y * COVER_DIM + x].push(i as u32);
                }
            }
        }
        self.box_start = Vec::with_capacity(cells.len() + 1);
        self.box_start.push(0);
        for c in &cells {
            self.box_items.extend_from_slice(c);
            self.box_start.push(self.box_items.len() as u32);
        }
    }

    pub fn boxes(&self) -> &[CoverBox] {
        &self.boxes
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
            self.sites.push(Site { center: c, half, level: level as f32 * CM, base });
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
                self.boxes.push(CoverBox { min, max, bottom, top, kind: p.kind, facing: (p.facing + turn + k) % 4 });
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
    pub fn ground(&self, x: f32, y: f32, z: f32, reach: f32) -> f32 {
        let mut g = self.terrain(x, y);
        self.boxes_near(x, y, 0.0, |b| {
            if x >= b.min[0] && x <= b.max[0] && y >= b.min[1] && y <= b.max[1] && b.top <= z + reach && b.top > g {
                g = b.top;
            }
        });
        g
    }

    /// A digest of the terrain and cover, to check two machines built the same world.
    pub fn digest(&self) -> u64 {
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
        }
        h
    }
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
                p = step(&w, p, Input { move_x: (dir[0] * 127.0) as i8, move_y: (dir[1] * 127.0) as i8, ..Default::default() });
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
        assert_eq!(w.ground(cx, cy, b.top, 0.5), b.top);
        assert_eq!(w.ground(cx, cy, b.top - 2.0, 0.5), w.terrain(cx, cy), "out of reach: the terrain");
    }
}
