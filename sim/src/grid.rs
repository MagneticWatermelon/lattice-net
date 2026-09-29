//! Uniform spatial grid, rebuilt from scratch every tick by counting sort.
//! This is the one index shared by every system (networking now, NPC proximity
//! and AoE later), so it is built once per tick and only read afterwards.

use crate::movement::WORLD_SIZE;

pub struct Grid {
    cell: f32,
    dim: u32,
    /// Items of cell `c` are `items[start[c]..start[c + 1]]`.
    start: Vec<u32>,
    items: Vec<u32>,
}

impl Grid {
    pub fn new(cell: f32) -> Self {
        let dim = (WORLD_SIZE / cell).ceil() as u32;
        Self { cell, dim, start: vec![0; (dim * dim) as usize + 1], items: Vec::new() }
    }

    #[inline]
    fn coord(&self, v: f32) -> u32 {
        ((v / self.cell) as u32).min(self.dim - 1)
    }

    #[inline]
    fn cell_of(&self, p: [f32; 2]) -> usize {
        (self.coord(p[1]) * self.dim + self.coord(p[0])) as usize
    }

    /// `entries` is iterated twice, so it must be cheap to clone.
    pub fn rebuild<I>(&mut self, entries: I)
    where
        I: Iterator<Item = (u32, [f32; 2])> + Clone,
    {
        self.start.fill(0);
        let mut n = 0;
        for (_, p) in entries.clone() {
            let c = self.cell_of(p);
            self.start[c + 1] += 1;
            n += 1;
        }
        for c in 1..self.start.len() {
            self.start[c] += self.start[c - 1];
        }
        self.items.resize(n, 0);
        // Scatter using start[c] as a cursor, then shift back to restore the offsets.
        for (id, p) in entries {
            let c = self.cell_of(p);
            self.items[self.start[c] as usize] = id;
            self.start[c] += 1;
        }
        for c in (1..self.start.len()).rev() {
            self.start[c] = self.start[c - 1];
        }
        self.start[0] = 0;
    }

    /// Calls `f` with every item in cells overlapping the square of half-size `r`
    /// around `p`. Callers filter by exact distance.
    pub fn for_each_near(&self, p: [f32; 2], r: f32, mut f: impl FnMut(u32)) {
        let (x0, x1) = (self.coord((p[0] - r).max(0.0)), self.coord(p[0] + r));
        let (y0, y1) = (self.coord((p[1] - r).max(0.0)), self.coord(p[1] + r));
        for y in y0..=y1 {
            let row = (y * self.dim) as usize;
            let (a, b) = (self.start[row + x0 as usize], self.start[row + x1 as usize + 1]);
            for &id in &self.items[a as usize..b as usize] {
                f(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_brute_force() {
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32 * WORLD_SIZE
        };
        let pts: Vec<[f32; 2]> = (0..5000).map(|_| [next(), next()]).collect();
        let mut g = Grid::new(32.0);
        g.rebuild(pts.iter().enumerate().map(|(i, &p)| (i as u32, p)));

        for q in [[0.0, 0.0], [4000.0, 4000.0], [WORLD_SIZE, WORLD_SIZE], [10.0, 8000.0]] {
            let r = 300.0;
            let d2 = |p: [f32; 2]| (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2);
            let mut got = Vec::new();
            g.for_each_near(q, r, |id| {
                if d2(pts[id as usize]) <= r * r {
                    got.push(id);
                }
            });
            got.sort_unstable();
            let want: Vec<u32> = (0..pts.len() as u32).filter(|&i| d2(pts[i as usize]) <= r * r).collect();
            assert_eq!(got, want);
        }
    }
}
