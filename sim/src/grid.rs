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

/// What `Grid::walk_rings` reports to its visitor.
pub enum Ring {
    /// An item in a cell of the current ring.
    Item(u32),
    /// A ring is done: every item not visited yet is at least this far from `p`.
    /// Return true to stop.
    Done(f32),
}

impl Grid {
    /// Visits the cells around `p` in rings of growing Chebyshev distance, out
    /// to radius `r`, so a k-nearest search can stop as soon as its k-th
    /// candidate is closer than anything unvisited. In a dense crowd that is a
    /// handful of cells instead of every cell in `r`.
    pub fn walk_rings<F: FnMut(Ring) -> bool>(&self, p: [f32; 2], r: f32, mut visit: F) {
        let (cx, cy) = (self.coord(p[0]) as i32, self.coord(p[1]) as i32);
        let rings = (r / self.cell).ceil() as i32 + 1;
        for k in 0..=rings {
            if k == 0 {
                self.visit_cell(cx, cy, &mut visit);
            } else {
                for x in cx - k..=cx + k {
                    self.visit_cell(x, cy - k, &mut visit);
                    self.visit_cell(x, cy + k, &mut visit);
                }
                for y in cy - k + 1..cy + k {
                    self.visit_cell(cx - k, y, &mut visit);
                    self.visit_cell(cx + k, y, &mut visit);
                }
            }
            // p sits anywhere in its cell, so ring k+1 is at least k cells away.
            if visit(Ring::Done(k as f32 * self.cell)) {
                return;
            }
        }
    }

    #[inline]
    fn visit_cell<F: FnMut(Ring) -> bool>(&self, x: i32, y: i32, visit: &mut F) {
        let dim = self.dim as i32;
        if x >= 0 && y >= 0 && x < dim && y < dim {
            let c = (y * dim + x) as usize;
            for &id in &self.items[self.start[c] as usize..self.start[c + 1] as usize] {
                visit(Ring::Item(id));
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

    #[test]
    fn ring_walk_finds_the_k_nearest_and_stops_early() {
        let mut rng = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        // A dense 200 m blob in the middle of a sparse world.
        let pts: Vec<[f32; 2]> = (0..6000)
            .map(|i| if i < 3000 { [4000.0 + next() * 200.0, 4000.0 + next() * 200.0] } else { [next() * WORLD_SIZE, next() * WORLD_SIZE] })
            .collect();
        let mut g = Grid::new(32.0);
        g.rebuild(pts.iter().enumerate().map(|(i, &p)| (i as u32, p)));

        for (q, r, k) in [([4100.0, 4100.0], 150.0, 100), ([100.0, 100.0], 150.0, 100), ([4000.0, 4000.0], 150.0, 10)] {
            let d2 = |i: u32| (pts[i as usize][0] - q[0]).powi(2) + (pts[i as usize][1] - q[1]).powi(2);
            let mut found: Vec<(f32, u32)> = Vec::new();
            let mut visited = 0;
            g.walk_rings(q, r, |v| match v {
                Ring::Item(i) => {
                    visited += 1;
                    if d2(i) <= r * r {
                        found.push((d2(i), i));
                    }
                    false
                }
                Ring::Done(bound) if found.len() >= k => {
                    found.select_nth_unstable_by(k - 1, |a, b| a.0.total_cmp(&b.0));
                    found[k - 1].0 <= bound * bound
                }
                Ring::Done(_) => false,
            });
            found.sort_by(|a, b| a.0.total_cmp(&b.0));
            found.truncate(k);
            let mut want: Vec<(f32, u32)> = (0..pts.len() as u32).map(|i| (d2(i), i)).filter(|x| x.0 <= r * r).collect();
            want.sort_by(|a, b| a.0.total_cmp(&b.0));
            want.truncate(k);
            assert_eq!(found, want, "query at {q:?}");
            if q == [4100.0, 4100.0] {
                assert!(visited < 800, "a dense query stops early: visited {visited} of ~3000");
            }
        }
    }
}
