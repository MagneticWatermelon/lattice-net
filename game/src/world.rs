//! The world, built from a seed identically on the server and every client:
//! a heightmap and a scatter of cover boxes.
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

/// A static axis-aligned box: blocks movement and (later) shots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverBox {
    pub min: [f32; 2],
    pub max: [f32; 2],
    /// Its base sits at the terrain's lowest corner; its top is level.
    pub bottom: f32,
    pub top: f32,
}

pub struct World {
    pub seed: u64,
    heights: Vec<u16>,
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
        let mut world = Self { seed, heights, boxes: Vec::new(), box_start: Vec::new(), box_items: Vec::new() };
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
                if !h.is_multiple_of(4) {
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
                let corners = [self.terrain(min[0], min[1]), self.terrain(max[0], min[1]), self.terrain(min[0], max[1]), self.terrain(max[0], max[1])];
                let (low, high) = corners.iter().fold((f32::MAX, f32::MIN), |(l, h), &c| (l.min(c), h.max(c)));
                self.boxes.push(CoverBox { min, max, bottom: low, top: high + tall });
            }
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
