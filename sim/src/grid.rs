//! Uniform spatial grid, rebuilt from scratch every tick by counting sort.
//! This is the one index shared by every system (networking now, NPC proximity
//! and AoE later), so it is built once per tick and only read afterwards.

use crate::movement::WORLD_SIZE;

/// A cell holding more than this many items is split into sub-cells.
const DENSE: u32 = 256;
/// Items per sub-cell to aim for.
const PER_SUB: f32 = 32.0;
/// Smallest sub-cell side, in meters: coincident or near-coincident players
/// can't be separated by any subdivision, so stop splitting there.
const MIN_SUB_SIDE: f32 = 0.5;

pub struct Grid {
    cell: f32,
    dim: u32,
    /// Items of cell `c` are `items[start[c]..start[c + 1]]`.
    start: Vec<u32>,
    items: Vec<u32>,
    /// Positions inline, parallel to `items`: the distance loop never touches
    /// the entity arrays.
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// Per cell: index into `subs`, or `NOT_DENSE`.
    dense: Vec<u32>,
    /// Cells split into s x s sub-cells (`DENSE`): only the item order inside
    /// the cell's slice changes, so every other reader is unaffected.
    subs: Vec<Sub>,
    /// Sub-cell starts of every dense cell, concatenated (s * s + 1 each),
    /// as absolute offsets into `items`.
    sub_start: Vec<u32>,
    /// Scratch for the per-cell sort.
    tmp: Vec<(u32, f32, f32)>,
    sub_of: Vec<u32>,
}

const NOT_DENSE: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Sub {
    /// Cell coordinates.
    cx: u32,
    cy: u32,
    /// Sub-cells per side, and their side length.
    s: u32,
    side: f32,
    /// Offset of this cell's `s * s + 1` entries in `sub_start`.
    at: u32,
}

/// The k nearest search's selection state: packed keys `d2 bits << 32 | id`
/// (non-negative f32 bit patterns order like the floats, and the id breaks
/// ties deterministically) and a running threshold, so most candidates cost a
/// single compare. Reused across queries.
#[derive(Default)]
pub struct Knn {
    buf: Vec<u64>,
    k: usize,
    /// Only keys below this can still be in the result.
    tau: u64,
    /// Candidates whose distance the last query computed: its work.
    pub scanned: u64,
}

impl Knn {
    #[inline]
    fn key(d2: f32, id: u32) -> u64 {
        ((d2.to_bits() as u64) << 32) | id as u64
    }

    fn reset(&mut self, k: usize, r2: f32) {
        self.buf.clear();
        self.scanned = 0;
        self.k = k;
        // Inclusive of the radius: any real id is below u32::MAX.
        self.tau = Self::key(r2, u32::MAX);
    }

    /// The squared distance below which a candidate can still make the cut.
    #[inline]
    fn tau_d2(&self) -> f32 {
        f32::from_bits((self.tau >> 32) as u32)
    }

    #[inline]
    fn offer(&mut self, d2: f32, id: u32, accept: &mut impl FnMut(u32) -> bool) {
        let key = Self::key(d2, id);
        if key < self.tau && accept(id) {
            self.buf.push(key);
        }
    }

    /// With at least k kept, select the k nearest and drop the rest: the k-th
    /// becomes the threshold.
    fn tighten(&mut self) {
        let k = self.k;
        if self.buf.len() >= k {
            if self.buf.len() > k {
                self.buf.select_nth_unstable(k - 1);
                self.buf.truncate(k);
            }
            self.tau = *self.buf.iter().max().unwrap();
        }
    }

    /// The result: at most k keys, in no particular order.
    pub fn keys(&self) -> &[u64] {
        &self.buf
    }

    /// The id and squared distance of a result key.
    #[inline]
    pub fn split(key: u64) -> (u32, f32) {
        (key as u32, f32::from_bits((key >> 32) as u32))
    }
}

impl Grid {
    pub fn new(cell: f32) -> Self {
        let dim = (WORLD_SIZE / cell).ceil() as u32;
        let cells = (dim * dim) as usize;
        Self {
            cell,
            dim,
            start: vec![0; cells + 1],
            items: Vec::new(),
            xs: Vec::new(),
            ys: Vec::new(),
            dense: vec![NOT_DENSE; cells],
            subs: Vec::new(),
            sub_start: Vec::new(),
            tmp: Vec::new(),
            sub_of: Vec::new(),
        }
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
        self.xs.resize(n, 0.0);
        self.ys.resize(n, 0.0);
        // Scatter using start[c] as a cursor, then shift back to restore the offsets.
        for (id, p) in entries {
            let c = self.cell_of(p);
            let at = self.start[c] as usize;
            self.items[at] = id;
            self.xs[at] = p[0];
            self.ys[at] = p[1];
            self.start[c] += 1;
        }
        for c in (1..self.start.len()).rev() {
            self.start[c] = self.start[c - 1];
        }
        self.start[0] = 0;
        self.split_dense();
    }

    /// Splits every cell over `DENSE` items into sub-cells of ~`PER_SUB` items
    /// by a counting sort of its own slice.
    fn split_dense(&mut self) {
        for sub in self.subs.drain(..) {
            self.dense[(sub.cy * self.dim + sub.cx) as usize] = NOT_DENSE;
        }
        self.sub_start.clear();
        let max_s = ((self.cell / MIN_SUB_SIDE) as u32).max(1);
        for c in 0..(self.dim * self.dim) as usize {
            let (a, b) = (self.start[c] as usize, self.start[c + 1] as usize);
            let count = (b - a) as u32;
            if count <= DENSE {
                continue;
            }
            let s = ((count as f32 / PER_SUB).sqrt().ceil() as u32).clamp(2, max_s);
            let side = self.cell / s as f32;
            let (cx, cy) = (c as u32 % self.dim, c as u32 / self.dim);
            let (ox, oy) = (cx as f32 * self.cell, cy as f32 * self.cell);
            let at = self.sub_start.len() as u32;
            let n_sub = (s * s) as usize;
            self.sub_start.resize(self.sub_start.len() + n_sub + 1, 0);
            let starts = &mut self.sub_start[at as usize..];
            self.sub_of.clear();
            for i in a..b {
                let sx = (((self.xs[i] - ox) / side) as u32).min(s - 1);
                let sy = (((self.ys[i] - oy) / side) as u32).min(s - 1);
                let si = sy * s + sx;
                self.sub_of.push(si);
                starts[si as usize + 1] += 1;
            }
            starts[0] = a as u32;
            for j in 1..=n_sub {
                starts[j] += starts[j - 1];
            }
            // Scatter into tmp in sub-cell order, then copy back over the slice.
            self.tmp.clear();
            self.tmp.resize(b - a, (0, 0.0, 0.0));
            let mut cursor: Vec<u32> = starts[..n_sub].to_vec();
            for (k, i) in (a..b).enumerate() {
                let si = self.sub_of[k] as usize;
                self.tmp[(cursor[si] as usize) - a] = (self.items[i], self.xs[i], self.ys[i]);
                cursor[si] += 1;
            }
            for (k, &(id, x, y)) in self.tmp.iter().enumerate() {
                self.items[a + k] = id;
                self.xs[a + k] = x;
                self.ys[a + k] = y;
            }
            self.dense[c] = self.subs.len() as u32;
            self.subs.push(Sub { cx, cy, s, side, at });
        }
    }

    /// Squared distance from `q` to the box [x0, x1] x [y0, y1] (0 inside).
    #[inline]
    fn box_d2(q: [f32; 2], x0: f32, x1: f32, y0: f32, y1: f32) -> f32 {
        let dx = (x0 - q[0]).max(q[0] - x1).max(0.0);
        let dy = (y0 - q[1]).max(q[1] - y1).max(0.0);
        dx * dx + dy * dy
    }

    /// Distance from `q` (inside the box) to the nearest side of the box of
    /// cells [xl, xh] x [yl, yh] that has cells beyond it: anything outside
    /// the box is at least this far.
    fn edge(&self, q: [f32; 2], xl: i32, xh: i32, yl: i32, yh: i32) -> f32 {
        let (last, c) = (self.dim as i32 - 1, self.cell);
        let mut e = f32::INFINITY;
        if xl > 0 {
            e = e.min(q[0] - xl as f32 * c);
        }
        if xh < last {
            e = e.min((xh + 1) as f32 * c - q[0]);
        }
        if yl > 0 {
            e = e.min(q[1] - yl as f32 * c);
        }
        if yh < last {
            e = e.min((yh + 1) as f32 * c - q[1]);
        }
        e.max(0.0)
    }

    #[inline]
    fn scan(&self, a: usize, b: usize, q: [f32; 2], accept: &mut impl FnMut(u32) -> bool, out: &mut Knn) {
        out.scanned += (b - a) as u64;
        for i in a..b {
            let (dx, dy) = (self.xs[i] - q[0], self.ys[i] - q[1]);
            out.offer(dx * dx + dy * dy, self.items[i], accept);
        }
    }

    /// A dense cell: its sub-cells in rings around the one nearest `q`,
    /// skipping any whose box is beyond the current threshold.
    fn scan_dense(&self, sub: &Sub, q: [f32; 2], accept: &mut impl FnMut(u32) -> bool, out: &mut Knn) {
        let (s, side) = (sub.s as i32, sub.side);
        let (ox, oy) = (sub.cx as f32 * self.cell, sub.cy as f32 * self.cell);
        let inside = q[0] >= ox && q[0] < ox + self.cell && q[1] >= oy && q[1] < oy + self.cell;
        let qx = (((q[0] - ox) / side).floor() as i32).clamp(0, s - 1);
        let qy = (((q[1] - oy) / side).floor() as i32).clamp(0, s - 1);
        let starts = &self.sub_start[sub.at as usize..];
        let mut visit = |x: i32, y: i32, out: &mut Knn| {
            if x < 0 || y < 0 || x >= s || y >= s {
                return;
            }
            let (x0, y0) = (ox + x as f32 * side, oy + y as f32 * side);
            if Self::box_d2(q, x0, x0 + side, y0, y0 + side) > out.tau_d2() {
                return;
            }
            let si = (y * s + x) as usize;
            self.scan(starts[si] as usize, starts[si + 1] as usize, q, accept, out);
        };
        for r in 0..s {
            if r == 0 {
                visit(qx, qy, out);
            } else {
                for x in qx - r..=qx + r {
                    visit(x, qy - r, out);
                    visit(x, qy + r, out);
                }
                for y in qy - r + 1..qy + r {
                    visit(qx - r, y, out);
                    visit(qx + r, y, out);
                }
            }
            out.tighten();
            let (xl, xh, yl, yh) = (qx - r, qx + r, qy - r, qy + r);
            if xl <= 0 && yl <= 0 && xh >= s - 1 && yh >= s - 1 {
                return; // the whole cell is done
            }
            if inside && out.buf.len() >= out.k {
                // This cell's unvisited sub-cells lie outside the visited sub-box.
                let mut e = f32::INFINITY;
                if xl > 0 {
                    e = e.min(q[0] - (ox + xl as f32 * side));
                }
                if xh < s - 1 {
                    e = e.min(ox + (xh + 1) as f32 * side - q[0]);
                }
                if yl > 0 {
                    e = e.min(q[1] - (oy + yl as f32 * side));
                }
                if yh < s - 1 {
                    e = e.min(oy + (yh + 1) as f32 * side - q[1]);
                }
                if out.tau_d2() <= e * e {
                    return;
                }
            }
        }
    }

    /// The `k` nearest items within `r` of `q` that `accept` takes, exactly
    /// (ties by id), into `out` (see `Knn::keys`). Cells are walked in rings
    /// as in `walk_rings`, but a cell or sub-cell whose box is farther than the
    /// current k-th candidate is skipped, a dense cell is searched by its
    /// sub-cells nearest first, and the walk stops as soon as the k-th
    /// candidate is closer than the edge of the visited box. In a crowd that
    /// is a few hundred candidates instead of everyone in the cells.
    /// `accept` only sees candidates already close enough to make the cut.
    pub fn knn(&self, q: [f32; 2], r: f32, k: usize, mut accept: impl FnMut(u32) -> bool, out: &mut Knn) {
        out.reset(k, r * r);
        if k == 0 {
            return;
        }
        let (cx, cy) = (self.coord(q[0].max(0.0)) as i32, self.coord(q[1].max(0.0)) as i32);
        let dim = self.dim as i32;
        let rings = (r / self.cell).ceil() as i32 + 1;
        for ring in 0..=rings {
            let mut visit = |x: i32, y: i32, out: &mut Knn| {
                if x < 0 || y < 0 || x >= dim || y >= dim {
                    return;
                }
                let c = (y * dim + x) as usize;
                let (a, b) = (self.start[c] as usize, self.start[c + 1] as usize);
                if a == b {
                    return;
                }
                let (x0, y0) = (x as f32 * self.cell, y as f32 * self.cell);
                if Self::box_d2(q, x0, x0 + self.cell, y0, y0 + self.cell) > out.tau_d2() {
                    return;
                }
                // Only a cell over DENSE items can have been split.
                if b - a > DENSE as usize {
                    self.scan_dense(&self.subs[self.dense[c] as usize], q, &mut accept, out);
                } else {
                    self.scan(a, b, q, &mut accept, out);
                }
                if out.buf.len() >= 2 * k {
                    out.tighten();
                }
            };
            if ring == 0 {
                visit(cx, cy, out);
            } else {
                // A row's cells are contiguous in `items`: skip an empty stretch
                // of the ring's top or bottom row with one compare.
                let (xa, xb) = ((cx - ring).max(0), (cx + ring).min(dim - 1));
                for y in [cy - ring, cy + ring] {
                    if y < 0 || y >= dim {
                        continue;
                    }
                    let row = (y * dim) as usize;
                    if self.start[row + xa as usize] == self.start[row + xb as usize + 1] {
                        continue;
                    }
                    for x in xa..=xb {
                        visit(x, y, out);
                    }
                }
                for y in cy - ring + 1..cy + ring {
                    visit(cx - ring, y, out);
                    visit(cx + ring, y, out);
                }
            }
            out.tighten();
            let e = self.edge(q, cx - ring, cx + ring, cy - ring, cy + ring);
            // Nothing unvisited is within the radius, or closer than the k-th.
            if e * e > r * r || (out.buf.len() >= k && out.tau_d2() <= e * e) {
                break;
            }
        }
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

    /// Brute-force k nearest, keyed exactly as `Knn` keys (same float ops).
    fn oracle(pts: &[[f32; 2]], q: [f32; 2], r: f32, k: usize, accept: impl Fn(u32) -> bool) -> Vec<u64> {
        let mut keys: Vec<u64> = (0..pts.len() as u32)
            .filter_map(|i| {
                let (dx, dy) = (pts[i as usize][0] - q[0], pts[i as usize][1] - q[1]);
                let d2 = dx * dx + dy * dy;
                (d2 <= r * r && accept(i)).then(|| Knn::key(d2, i))
            })
            .collect();
        keys.sort_unstable();
        keys.truncate(k);
        keys
    }

    fn rng(seed: u64) -> impl FnMut() -> f32 {
        let mut x = seed;
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    fn disk(n: usize, c: [f32; 2], radius: f32, seed: u64) -> Vec<[f32; 2]> {
        let mut next = rng(seed);
        (0..n)
            .map(|_| {
                let (a, d) = (next() * std::f32::consts::TAU, radius * next().sqrt());
                [c[0] + d * a.cos(), c[1] + d * a.sin()]
            })
            .collect()
    }

    fn check(pts: &[[f32; 2]], cell: f32, queries: &[[f32; 2]], r: f32, k: usize) -> u64 {
        let mut g = Grid::new(cell);
        g.rebuild(pts.iter().enumerate().map(|(i, &p)| (i as u32, p)));
        let mut out = Knn::default();
        let mut scanned = 0;
        for (n, &q) in queries.iter().enumerate() {
            // Also exercise a filter, like the squad and self exclusion.
            let skip = |i: u32| i % 7 != (n % 7) as u32;
            g.knn(q, r, k, skip, &mut out);
            let mut got = out.keys().to_vec();
            got.sort_unstable();
            assert_eq!(got, oracle(pts, q, r, k, skip), "query {q:?}, k {k}");
            scanned += out.scanned;
        }
        scanned / queries.len() as u64
    }

    #[test]
    fn knn_is_exact_on_every_distribution() {
        let c = [4096.0, 4096.0]; // a corner of the 32 m grid, like the pile
        let pile = disk(10_000, c, 25.0, 1);
        let wide = disk(10_000, c, 200.0, 2);
        let mut next = rng(3);
        let uniform: Vec<[f32; 2]> = (0..10_000).map(|_| [next() * WORLD_SIZE, next() * WORLD_SIZE]).collect();
        let mut blob = disk(3_000, [3000.0, 5000.0], 200.0, 4);
        blob.extend_from_slice(&uniform[..7_000]);
        let qs = |pts: &[[f32; 2]]| pts.iter().step_by(97).copied().collect::<Vec<_>>();
        for (name, pts) in [("pile", &pile), ("disk", &wide), ("uniform", &uniform), ("blob", &blob)] {
            for (cell, r, k) in [(32.0, 150.0, 100), (64.0, 500.0, 86), (32.0, 150.0, 1)] {
                let per = check(pts, cell, &qs(pts), r, k);
                if name == "pile" && cell == 32.0 && k == 100 {
                    assert!(per < 1_500, "pile queries scan {per} candidates, not ~10,000");
                }
            }
        }
        // Edges and corners of the world, and queries off the crowd.
        let edges = [[0.0, 0.0], [WORLD_SIZE, WORLD_SIZE], [0.0, WORLD_SIZE], [WORLD_SIZE - 0.01, 3.0], [4096.0, 4096.0], [4060.0, 4130.0]];
        check(&uniform, 32.0, &edges, 150.0, 100);
        check(&pile, 32.0, &edges, 150.0, 100);
        check(&pile, 32.0, &edges, 150.0, 0);
        check(&pile, 32.0, &edges, 30.0, 20_000); // k above the population
    }

    #[test]
    fn knn_handles_coincident_players() {
        // Stacks no subdivision can split: 2,000 on one point, 1,000 on another.
        let mut pts = vec![[4100.0, 4100.0]; 2_000];
        pts.extend(vec![[4100.3, 4100.0]; 1_000]);
        pts.extend(disk(500, [4100.0, 4100.0], 10.0, 9));
        check(&pts, 32.0, &[[4100.0, 4100.0], [4100.3, 4100.0], [4095.0, 4103.0]], 150.0, 100);
    }

    #[test]
    fn dense_cells_are_split_without_disturbing_other_readers() {
        let pts = disk(10_000, [4096.0, 4096.0], 25.0, 5);
        let mut g = Grid::new(32.0);
        g.rebuild(pts.iter().enumerate().map(|(i, &p)| (i as u32, p)));
        assert_eq!(g.subs.len(), 4, "the pile's four cells are split");
        // for_each_near still sees exactly the items in range.
        let q = [4090.0, 4100.0];
        let mut got = Vec::new();
        g.for_each_near(q, 20.0, |id| {
            let p = pts[id as usize];
            if (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) <= 400.0 {
                got.push(id);
            }
        });
        got.sort_unstable();
        let want: Vec<u32> =
            (0..pts.len() as u32).filter(|&i| (pts[i as usize][0] - q[0]).powi(2) + (pts[i as usize][1] - q[1]).powi(2) <= 400.0).collect();
        assert_eq!(got, want);
        // Rebuilding sparse clears the split.
        g.rebuild([(0, [10.0, 10.0])].into_iter());
        assert!(g.subs.is_empty() && g.dense.iter().all(|&d| d == NOT_DENSE));
    }
}
