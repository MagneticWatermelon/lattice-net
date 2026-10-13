//! Getting about a world (K3): a graph of where its routes go, for bots
//! that take sites. Its nodes are the routes' waypoints (where two are the
//! same place, one node), its edges the routes' steps between them (one way
//! up a lift or down a drop shaft), and for every site how far each node is
//! from standing on one of its points. A bot follows that downhill.
//!
//! Routes through a shield (a spawn room's covered exit) aren't in it: who
//! may pass changes as sites change hands, and a bot that walked into one
//! would stand there.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use crate::territory::{POINT_HEIGHT, POINT_RADIUS};
use crate::world::{Kind, World};

/// Waypoints within this of each other (and `MERGE_Z` in height) are one node.
const MERGE: f32 = 2.5;
const MERGE_Z: f32 = 2.5;
/// The grid nodes are found by.
const CELL: f32 = 16.0;
/// How far from a node someone off the graph looks for one.
pub const NEAR: f32 = 40.0;

pub struct Nav {
    nodes: Vec<[f32; 3]>,
    /// Out of each node: (to, cost in meters).
    out: Vec<Vec<(u32, f32)>>,
    grid: HashMap<(i32, i32), Vec<u32>>,
    /// Per site, per node: how far it is from standing on one of the site's
    /// points (`f32::MAX`: no way there).
    to_site: Vec<Vec<f32>>,
}

fn cell(p: [f32; 3]) -> (i32, i32) {
    ((p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32)
}

fn sq(x: f32) -> f32 {
    x * x
}

impl Nav {
    /// The graph of world `w`'s routes, and every site's distances.
    pub fn new(w: &World) -> Self {
        let lanes = w.lanes();
        // Waypoints, merged where they're the same place (union-find).
        let mut places: Vec<[f32; 3]> = Vec::new();
        let mut starts = Vec::with_capacity(lanes.len());
        for l in lanes {
            starts.push(places.len());
            places.extend(l.path.iter().copied());
        }
        let n = places.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while p[r] != r {
                r = p[r];
            }
            let mut j = i;
            while p[j] != r {
                let next = p[j];
                p[j] = r;
                j = next;
            }
            r
        }
        let mut by_cell: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, p) in places.iter().enumerate() {
            by_cell.entry(((p[0] / MERGE) as i32, (p[1] / MERGE) as i32)).or_default().push(i);
        }
        for i in 0..n {
            let p = places[i];
            let (cx, cy) = ((p[0] / MERGE) as i32, (p[1] / MERGE) as i32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    for &j in by_cell.get(&(cx + dx, cy + dy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                        let q = places[j];
                        if j != i && sq(q[0] - p[0]) + sq(q[1] - p[1]) <= MERGE * MERGE && (q[2] - p[2]).abs() <= MERGE_Z {
                            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                            parent[a.max(b)] = a.min(b);
                        }
                    }
                }
            }
        }
        let mut id = vec![u32::MAX; n];
        let mut nodes = Vec::new();
        for i in 0..n {
            let r = find(&mut parent, i);
            if id[r] == u32::MAX {
                id[r] = nodes.len() as u32;
                nodes.push(places[r]);
            }
            id[i] = id[r];
        }
        // Edges: each route's steps, but none through a shield.
        let mut out: Vec<Vec<(u32, f32)>> = vec![Vec::new(); nodes.len()];
        let shielded = |a: [f32; 3], b: [f32; 3]| {
            let steps = ((sq(b[0] - a[0]) + sq(b[1] - a[1])).sqrt()).ceil().max(1.0) as usize;
            (0..=steps).any(|k| {
                let t = k as f32 / steps as f32;
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
                let mut hit = false;
                w.boxes_near(p[0], p[1], 0.0, |bx| hit |= bx.kind == Kind::Shield && bx.covers(p[0], p[1]) && p[2] > bx.bottom - 1.0 && p[2] < bx.top);
                hit
            })
        };
        for (l, &s) in lanes.iter().zip(&starts) {
            for k in 0..l.path.len() - 1 {
                let (a, b) = (id[s + k], id[s + k + 1]);
                if a == b {
                    continue;
                }
                let (p, q) = (l.path[k], l.path[k + 1]);
                if shielded(p, q) {
                    continue;
                }
                // Meters walked, and a lift's or a fall's climb at about half the pace.
                let cost = ((sq(q[0] - p[0]) + sq(q[1] - p[1])).sqrt() + 0.5 * (q[2] - p[2]).abs()).max(0.1);
                out[a as usize].push((b, cost));
                if !l.one_way {
                    out[b as usize].push((a, cost));
                }
            }
        }
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, &p) in nodes.iter().enumerate() {
            grid.entry(cell(p)).or_default().push(i as u32);
        }
        let mut nav = Nav { nodes, out, grid, to_site: Vec::new() };
        // Every site's distances: back along the edges from its points.
        let mut into: Vec<Vec<(u32, f32)>> = vec![Vec::new(); nav.nodes.len()];
        for (a, es) in nav.out.iter().enumerate() {
            for &(b, c) in es {
                into[b as usize].push((a as u32, c));
            }
        }
        let mut pts: Vec<Vec<[f32; 3]>> = vec![Vec::new(); w.sites().len()];
        for p in w.points() {
            pts[p.site as usize].push(p.pos);
        }
        nav.to_site = pts.iter().map(|ps| nav.field(ps, &into)).collect();
        nav
    }

    /// The graph of `w` (one a world, shared).
    pub fn shared(w: &World) -> Arc<Nav> {
        static NAVS: OnceLock<Mutex<HashMap<u64, Arc<Nav>>>> = OnceLock::new();
        let mut m = NAVS.get_or_init(Default::default).lock().unwrap();
        Arc::clone(m.entry(w.digest()).or_insert_with(|| Arc::new(Nav::new(w))))
    }

    /// How far each node is from standing on one of points `ps`: from the
    /// nodes where someone would count for one (or near one, the rest of the
    /// way straight), back along the edges.
    fn field(&self, ps: &[[f32; 3]], into: &[Vec<(u32, f32)>]) -> Vec<f32> {
        let mut d = vec![f32::MAX; self.nodes.len()];
        let mut heap = BinaryHeap::new();
        for &p in ps {
            let r = POINT_RADIUS * 0.75;
            let on: Vec<u32> = self.within(p, r).into_iter().filter(|&i| (self.nodes[i as usize][2] - p[2]).abs() <= POINT_HEIGHT).collect();
            let seeds: Vec<(u32, f32)> = if on.is_empty() {
                self.within(p, 30.0).into_iter().filter(|&i| (self.nodes[i as usize][2] - p[2]).abs() <= 6.0).map(|i| (i, self.flat(i, p))).collect()
            } else {
                on.into_iter().map(|i| (i, 0.0)).collect()
            };
            for (i, c) in seeds {
                if c < d[i as usize] {
                    d[i as usize] = c;
                    heap.push((Reverse(c.to_bits()), i));
                }
            }
        }
        while let Some((Reverse(c), i)) = heap.pop() {
            let c = f32::from_bits(c);
            if c > d[i as usize] {
                continue;
            }
            for &(j, e) in &into[i as usize] {
                if c + e < d[j as usize] {
                    d[j as usize] = c + e;
                    heap.push((Reverse((c + e).to_bits()), j));
                }
            }
        }
        d
    }

    fn flat(&self, i: u32, p: [f32; 3]) -> f32 {
        let q = self.nodes[i as usize];
        (sq(q[0] - p[0]) + sq(q[1] - p[1])).sqrt()
    }

    /// The nodes within `r` of `p` (across, not up).
    fn within(&self, p: [f32; 3], r: f32) -> Vec<u32> {
        let (c0, c1) = (cell([p[0] - r, p[1] - r, 0.0]), cell([p[0] + r, p[1] + r, 0.0]));
        let mut v = Vec::new();
        for cy in c0.1..=c1.1 {
            for cx in c0.0..=c1.0 {
                for &i in self.grid.get(&(cx, cy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if self.flat(i, p) <= r {
                        v.push(i);
                    }
                }
            }
        }
        v
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, i: u32) -> [f32; 3] {
        self.nodes[i as usize]
    }

    /// The node nearest `p` within `NEAR` (and 6 m up or down) that leads to
    /// site `site`, if any.
    pub fn nearest(&self, p: [f32; 3], site: usize) -> Option<u32> {
        let d = &self.to_site[site];
        self.within(p, NEAR)
            .into_iter()
            .filter(|&i| (self.nodes[i as usize][2] - p[2]).abs() <= 6.0 && d[i as usize] < f32::MAX)
            .min_by(|&a, &b| (self.flat(a, p) + 0.2 * d[a as usize]).total_cmp(&(self.flat(b, p) + 0.2 * d[b as usize])))
    }

    /// How far node `i` is from standing on one of site `site`'s points
    /// (`f32::MAX`: no way).
    pub fn cost(&self, site: usize, i: u32) -> f32 {
        self.to_site[site][i as usize]
    }

    /// The next node from `i` toward site `site`'s points (None: there, as
    /// near as the graph goes, or no way).
    pub fn toward(&self, site: usize, i: u32) -> Option<u32> {
        let d = &self.to_site[site];
        let here = d[i as usize];
        self.out[i as usize]
            .iter()
            .filter(|&&(j, e)| d[j as usize] < f32::MAX && e + d[j as usize] <= here)
            .min_by(|a, b| (a.1 + d[a.0 as usize]).total_cmp(&(b.1 + d[b.0 as usize])))
            .map(|&(j, _)| j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{SiteKind, KILN};

    #[test]
    fn every_site_is_found_from_every_landing() {
        // Kiln: down the graph from each Landing to every site's points, as
        // a bot would follow it, step by step, arriving.
        let w = World::shared(KILN);
        let t = std::time::Instant::now();
        let nav = Nav::new(&w);
        eprintln!("{} nodes, built in {:.1} s", nav.len(), t.elapsed().as_secs_f32());
        for (l, land) in w.sites().iter().enumerate().filter(|(_, s)| s.kind == SiteKind::Landing) {
            let c = [land.center[0], land.center[1], w.terrain(land.center[0], land.center[1])];
            for s in 0..w.sites().len() {
                if s == l || !w.points().iter().any(|p| p.site as usize == s) {
                    continue;
                }
                let mut at = nav.nearest(c, s).unwrap_or_else(|| panic!("Landing {l}: no way to site {s}"));
                let mut steps = 0;
                while let Some(next) = nav.toward(s, at) {
                    assert!(nav.cost(s, next) < nav.cost(s, at), "downhill");
                    at = next;
                    steps += 1;
                    assert!(steps < 100_000);
                }
                let end = nav.node(at);
                assert!(w.points().iter().any(|p| p.site as usize == s && sq(p.pos[0] - end[0]) + sq(p.pos[1] - end[1]) <= 30.0 * 30.0), "Landing {l} to site {s}: ends at {end:?}");
            }
        }
    }
}
