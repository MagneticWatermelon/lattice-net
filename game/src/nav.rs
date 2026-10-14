//! Getting about a world (K3): a graph of where its routes go, for bots
//! that take sites. Its nodes are the routes' waypoints (where two are the
//! same place, one node), its edges the routes' steps between them (one way
//! up a lift or down a drop shaft), and for every site how far each node is
//! from standing on one of its points. A bot follows that downhill.
//!
//! Routes through a shield (a spawn room's covered exit) aren't in it: who
//! may pass changes as sites change hands, and a bot that walked into one
//! would stand there. But every spawn room has ways out of it (U4: players
//! deploy in them): from its middle out of each door, round the baffle,
//! to the nearest route node, one way (only its holder is ever in it),
//! each leg walked before it's added.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use crate::territory::{POINT_HEIGHT, POINT_RADIUS};
use crate::world::{Kind, Lane, SpawnRoom, World, ALWAYS};

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
    /// Spawn rooms with a way out, and the node at each one's middle.
    rooms: Vec<(SpawnRoom, u32)>,
}

fn cell(p: [f32; 3]) -> (i32, i32) {
    ((p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32)
}

fn sq(x: f32) -> f32 {
    x * x
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (sq(a[0] - b[0]) + sq(a[1] - b[1]) + sq(a[2] - b[2])).sqrt()
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
        // Edges: each route's steps, but none through a shield, and none of
        // the clockwork's (a night's bridge, a sandfall): the graph is the
        // world's always.
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
        for (l, &s) in lanes.iter().zip(&starts).filter(|(l, _)| l.when == crate::world::ALWAYS) {
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
        let mut nav = Nav { nodes, out, grid, to_site: Vec::new(), rooms: Vec::new() };
        nav.fields(w);
        // Out of the spawn rooms (`exits`): to nodes that lead to most sites.
        let sites = nav.to_site.len();
        let lattice: Vec<bool> = (0..nav.nodes.len()).map(|i| nav.to_site.iter().filter(|d| d[i] < f32::MAX).count() * 2 >= sites).collect();
        let rooms = w.spawn_rooms();
        let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(16);
        let found: Vec<Vec<(Vec<[f32; 3]>, u32)>> = std::thread::scope(|sc| {
            let chunk = rooms.len().div_ceil(threads).max(1);
            let (nav, lattice) = (&nav, &lattice);
            let handles: Vec<_> = rooms.chunks(chunk).map(|rs| sc.spawn(move || rs.iter().map(|r| nav.exits(w, r, lattice)).collect::<Vec<_>>())).collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        for (r, ways) in rooms.iter().zip(found) {
            if ways.is_empty() {
                continue;
            }
            let c = nav.add([r.center[0], r.center[1], r.floor]);
            for (way, goal) in ways {
                let mut from = c;
                for p in way.into_iter().skip(1) {
                    let n = nav.add(p);
                    nav.link(from, n);
                    from = n;
                }
                nav.link(from, goal);
            }
            nav.rooms.push((*r, c));
        }
        if !nav.rooms.is_empty() {
            nav.fields(w);
        }
        nav
    }

    /// Every site's distances: back along the edges from its points.
    fn fields(&mut self, w: &World) {
        let mut into: Vec<Vec<(u32, f32)>> = vec![Vec::new(); self.nodes.len()];
        for (a, es) in self.out.iter().enumerate() {
            for &(b, c) in es {
                into[b as usize].push((a as u32, c));
            }
        }
        let mut pts: Vec<Vec<[f32; 3]>> = vec![Vec::new(); w.sites().len()];
        for p in w.points() {
            pts[p.site as usize].push(p.pos);
        }
        self.to_site = pts.iter().map(|ps| self.field(ps, &into)).collect();
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

    /// Spawn room `r`'s way out: a walk of grid steps (`walk_to`) from its
    /// middle, out of whichever door leads somewhere, to a node that leads
    /// about the continent (`lattice`, by node: those beside a room are
    /// often a site's inner ways, behind its shields), and that node.
    /// Walked as the room's holder (one way: out; only its holder is ever
    /// in it).
    fn exits(&self, w: &World, r: &SpawnRoom, lattice: &[bool]) -> Vec<(Vec<[f32; 3]>, u32)> {
        // As the room's holder (its site held by faction 0, as 0).
        let mut owners: Vec<u8> = w.sites().iter().map(|s| if s.sector < 3 { s.sector } else { crate::territory::NONE }).collect();
        if let Some(o) = owners.get_mut(r.site as usize) {
            *o = 0;
        }
        let who = crate::movement::Mover { faction: 0, owners: &owners, live: crate::clock::Live::OFF };
        let mid = [r.center[0], r.center[1], r.floor];
        let mut goals: Vec<(u32, [f32; 3])> =
            self.within(mid, EXIT_REACH).into_iter().filter(|&i| lattice.get(i as usize).copied().unwrap_or(false)).map(|i| (i, self.nodes[i as usize])).collect();
        goals.sort_by(|a, b| dist(a.1, mid).total_cmp(&dist(b.1, mid)));
        goals.truncate(1000);
        match self.walk_to(w, mid, &goals, lattice, who) {
            Some((mut way, g)) => {
                way.insert(0, mid);
                vec![(way, g)]
            }
            None => {
                if std::env::var_os("NAV_DEBUG").is_some() {
                    eprintln!("room {}: no way out ({} goals, the nearest {:?})", r.site, goals.len(), goals.first().map(|g| g.1));
                }
                Vec::new()
            }
        }
    }

    fn add(&mut self, p: [f32; 3]) -> u32 {
        self.nodes.push(p);
        self.out.push(Vec::new());
        let i = (self.nodes.len() - 1) as u32;
        self.grid.entry(cell(p)).or_default().push(i);
        i
    }

    /// A one-way edge, costed as the routes' are.
    fn link(&mut self, a: u32, b: u32) {
        let (p, q) = (self.nodes[a as usize], self.nodes[b as usize]);
        let cost = ((sq(q[0] - p[0]) + sq(q[1] - p[1])).sqrt() + 0.5 * (q[2] - p[2]).abs()).max(0.1);
        self.out[a as usize].push((b, cost));
    }

    /// The spawn room `p` stands in, and its middle's node.
    fn room_of(&self, p: [f32; 3]) -> Option<u32> {
        self.rooms
            .iter()
            .find(|(r, _)| {
                let d = [p[0] - r.center[0], p[1] - r.center[1]];
                let (a, b) = (d[0] * r.axis[0] + d[1] * r.axis[1], -d[0] * r.axis[1] + d[1] * r.axis[0]);
                a.abs() <= r.half[0] && b.abs() <= r.half[1] && (p[2] - r.floor).abs() < 2.0
            })
            .map(|&(_, c)| c)
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
        // In a spawn room: out of it, by its doors (the nodes beyond its walls are near too).
        if let Some(c) = self.room_of(p) {
            return (d[c as usize] < f32::MAX).then_some(c);
        }
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

/// How far from a spawn room's door its way out looks for the graph (m).
const EXIT_REACH: f32 = 400.0;
/// The grid steps a way out may take at most.
const EXIT_STEPS: usize = 2500;

impl Nav {
    /// A way from `from` to one of `goals` (graph nodes that lead about the
    /// continent, nearest first): A* over grid steps of 4 m, each walked
    /// eight ways as the reachability check walks them (`reach`'s flood),
    /// none a fall that hurts; and along the routes' own edges from any of
    /// their nodes it stands on (up a spiral stair, a lift). The points after
    /// `from`, and the goal reached.
    fn walk_to(&self, w: &World, from: [f32; 3], goals: &[(u32, [f32; 3])], lattice: &[bool], who: crate::movement::Mover) -> Option<(Vec<[f32; 3]>, u32)> {
        use crate::movement::{fall_height, step, Input, MoveState, SAFE_FALL};
        const STEP: f32 = 4.0;
        if goals.is_empty() {
            return None;
        }
        let straight = |a: [f32; 3], b: [f32; 3]| crate::reach::walk_as(w, &Lane { a: 0, b: 0, path: vec![a, b], one_way: true, when: ALWAYS }, false, crate::movement::Mover { ..who }).is_ok();
        let h = |p: [f32; 3]| goals.iter().map(|(_, q)| dist(*q, p)).fold(f32::MAX, f32::min);
        // A place walked to (by its grid cell and height), or a graph node.
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        enum K {
            Cell(i32, i32, i32),
            Node(u32),
        }
        let cell_key = |p: [f32; 3]| K::Cell((p[0] / STEP).floor() as i32, (p[1] / STEP).floor() as i32, (p[2] / 2.0).round() as i32);
        let mut pts: Vec<([f32; 3], Option<u32>)> = vec![(from, None)];
        let mut parent = vec![usize::MAX];
        let mut g = vec![0.0f32];
        let mut seen: HashMap<K, usize> = HashMap::from([(cell_key(from), 0)]);
        let mut heap = BinaryHeap::from([(Reverse(h(from).to_bits()), 0usize)]);
        let mut expanded = 0;
        let way = |pts: &Vec<([f32; 3], Option<u32>)>, parent: &Vec<usize>, mut k: usize| {
            let mut v = Vec::new();
            while k != 0 {
                v.push(pts[k].0);
                k = parent[k];
            }
            v.reverse();
            v
        };
        let mut push = |pts: &mut Vec<([f32; 3], Option<u32>)>, parent: &mut Vec<usize>, g: &mut Vec<f32>, heap: &mut BinaryHeap<(Reverse<u32>, usize)>, key: K, q: [f32; 3], node: Option<u32>, from: usize, cost: f32| {
            if seen.contains_key(&key) {
                return;
            }
            let n = pts.len();
            seen.insert(key, n);
            pts.push((q, node));
            parent.push(from);
            g.push(g[from] + cost);
            heap.push((Reverse((g[n] + h(q)).to_bits()), n));
        };
        while let Some((_, i)) = heap.pop() {
            let (p, node) = pts[i];
            if let Some(n) = node {
                // A node that leads about: there.
                if lattice.get(n as usize).copied().unwrap_or(false) {
                    let mut v = way(&pts, &parent, i);
                    v.pop();
                    return Some((v, n));
                }
                // On along its routes, and off them walking.
                for &(m, e) in &self.out[n as usize] {
                    push(&mut pts, &mut parent, &mut g, &mut heap, K::Node(m), self.nodes[m as usize], Some(m), i, e);
                }
                push(&mut pts, &mut parent, &mut g, &mut heap, cell_key(p), p, None, i, 0.0);
                continue;
            }
            // A goal near enough to walk to straight.
            for &(gi, q) in goals.iter().filter(|(_, q)| (sq(q[0] - p[0]) + sq(q[1] - p[1])).sqrt() <= 10.0 && (q[2] - p[2]).abs() <= 3.0).take(3) {
                if straight(p, q) {
                    let mut v = way(&pts, &parent, i);
                    v.push(q);
                    return Some((v, gi));
                }
            }
            expanded += 1;
            if expanded > EXIT_STEPS {
                break;
            }
            // Onto the routes' nodes it stands by.
            for n in self.within(p, 2.5) {
                let q = self.nodes[n as usize];
                if (q[2] - p[2]).abs() <= 2.0 && straight(p, q) {
                    push(&mut pts, &mut parent, &mut g, &mut heap, K::Node(n), q, Some(n), i, dist(p, q));
                }
            }
            for (dx, dy) in [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)] {
                let to = [p[0] + dx as f32 * STEP, p[1] + dy as f32 * STEP];
                let l = ((dx * dx + dy * dy) as f32).sqrt();
                let (mx, my) = ((dx as f32 / l * 127.0) as i8, (dy as f32 / l * 127.0) as i8);
                let mut s = MoveState { pos: [p[0], p[1]], vel: [0.0; 2], z: p[2], vz: 0.0, grounded: true, crouched: false };
                // Walked (about two-thirds of a second at a run), then standing
                // till it lands: long enough to ride a drop shaft or a lift; no
                // landing that hurts (as the game measures falls, a step at a time).
                let mut hurt = false;
                let mut k = 0;
                while k < 40 && sq(s.pos[0] - to[0]) + sq(s.pos[1] - to[1]) > 0.36 {
                    let before = s;
                    s = step(w, s, Input { move_x: mx, move_y: my, ..Default::default() }, crate::movement::Mover { ..who });
                    hurt |= fall_height(&before, &s) > SAFE_FALL;
                    k += 1;
                }
                let mut j = 0;
                while j < 400 && !(s.grounded && s.vz.abs() < 0.01) {
                    let before = s;
                    s = step(w, s, Input::default(), crate::movement::Mover { ..who });
                    hurt |= fall_height(&before, &s) > SAFE_FALL;
                    j += 1;
                }
                let moved = (sq(s.pos[0] - p[0]) + sq(s.pos[1] - p[1])).sqrt() + (s.z - p[2]).abs();
                if !s.grounded || moved < 2.5 || hurt {
                    continue;
                }
                let q = [s.pos[0], s.pos[1], s.z];
                push(&mut pts, &mut parent, &mut g, &mut heap, cell_key(q), q, None, i, moved);
            }
        }
        if std::env::var_os("NAV_DEBUG").is_some() {
            eprintln!("  walk from {from:?}: {expanded} expanded, {} places", pts.len());
        }
        None
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
        // Out of every spawn room, to most sites' points.
        let hollow = w.sites().iter().position(|s| s.kind == SiteKind::Hollow).unwrap();
        let stuck: Vec<(u16, usize)> = w
            .spawn_rooms()
            .iter()
            .filter_map(|r| {
                let p = [r.center[0], r.center[1], r.floor];
                let reached = nav.room_of(p).map_or(0, |c| (0..w.sites().len()).filter(|&s| nav.cost(s, c) < f32::MAX).count());
                (reached * 2 < w.sites().len()).then_some((r.site, reached))
            })
            .collect();
        assert!(stuck.is_empty(), "no way out of the spawn rooms of sites (and how many sites they reach) {stuck:?}");
    }
}
