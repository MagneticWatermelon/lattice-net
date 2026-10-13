//! Walking a world (K3): the route walker the tests and tools share, and the
//! reachability check an exporter runs before a continent ships (GDD §16:
//! "every control point can be reached from every spawn"), here on a
//! generated world. From each Landing, as its faction (its own shields let
//! it through, no one else's), on foot and without jumping: along the routes
//! (each walked by the game's own movement), and in and around every site by
//! a flood of short steps across a 4 m grid, each walked by the same
//! movement, that finds whatever's there to stand on (stairs, decks, drops).
//! A point is reached when someone stands where they'd count for it
//! (`territory::POINT_RADIUS`, `POINT_HEIGHT`).

use std::collections::HashMap;

use crate::movement::{step, Input, MoveState, Mover, BUTTON_SPRINT, DT, WORLD_SIZE};
use crate::territory::{POINT_HEIGHT, POINT_RADIUS};
use crate::world::{Lane, SiteKind, World};

/// Walks lane `l` (backward from its end if `backward`), sprinting, never
/// jumping, as faction 0 with the sites held as at the start: the time it
/// took, or where it got stuck.
pub fn walk(w: &World, l: &Lane, backward: bool) -> Result<f32, String> {
    walk_as(w, l, backward, 0.into())
}

/// `walk`, as `who`.
pub fn walk_as(w: &World, l: &Lane, backward: bool, who: Mover) -> Result<f32, String> {
    let start = l.waypoint(0, backward);
    // On whatever's there (a stair's top, a deck): the highest surface
    // under the route's own height; under the ground (a chamber), its floor.
    let mut s = if start[2] < w.terrain(start[0], start[1]) - 2.0 {
        MoveState::standing_at(w, [start[0], start[1]], start[2])
    } else {
        let s = MoveState::standing(w, [start[0], start[1]]);
        MoveState { z: w.ground(start[0], start[1], start[2].max(s.z) + 1.0, 1.5).max(s.z), ..s }
    };
    let mut next = 1;
    // Sprinting along it, and up lifts and down drop shafts at 5 m/s or so.
    let climb: f32 = l.path.windows(2).map(|w| ((w[1][2] - w[0][2]).abs() - (w[0][0] - w[1][0]).abs() - (w[0][1] - w[1][1]).abs()).max(0.0)).sum();
    let budget = (l.length() / 9.0 * 1.6 + climb / 5.0 + 30.0) / DT;
    for i in 0..budget as u32 {
        let Some((mx, my)) = l.steer([s.pos[0], s.pos[1], s.z], &mut next, backward) else {
            let end = l.waypoint(l.path.len() - 1, backward);
            let dz = s.z - end[2];
            return if dz > -4.0 { Ok(i as f32 * DT) } else { Err(format!("reached the end at z {:.1}, not {:.1}", s.z, end[2])) };
        };
        s = step(w, s, Input { move_x: mx, move_y: my, yaw: 0, pitch: 0, buttons: BUTTON_SPRINT }, who);
    }
    let w_next = l.waypoint(next.min(l.path.len() - 1), backward);
    Err(format!("stuck at ({:.0}, {:.0}, {:.1}) heading for waypoint {next}/{} at ({:.0}, {:.0}, {:.1})", s.pos[0], s.pos[1], s.z, l.path.len(), w_next[0], w_next[1], w_next[2]))
}

/// What the check found.
#[derive(Debug, Default)]
pub struct Report {
    /// Routes that didn't walk (as faction 0).
    pub broken_routes: Vec<usize>,
    /// Control points not reached from a Landing: (point, the Landing's
    /// site, the nearest place that was reached).
    pub unreached: Vec<(usize, usize, [f32; 3])>,
    /// Control points inside something (cover, a shield their holder can't
    /// pass): where no one could stand on them.
    pub buried: Vec<usize>,
    pub landings: usize,
    /// The most places (merged route waypoints, grid cells' floors) one
    /// Landing's search reached, and the grid steps all of them walked.
    pub places: usize,
    pub steps: usize,
    /// Where the first Landing's search stood.
    pub reached: Vec<[f32; 3]>,
}

/// Route waypoints within this of each other (and `MERGE_Z` in height) are
/// one place; a waypoint and the floor under it, too.
const MERGE: f32 = 2.5;
const MERGE_Z: f32 = 2.5;
/// The flood's grid, and how far round a site (beyond its half size) and a
/// point it goes.
const CELL: f32 = 4.0;
const SITE_MARGIN: f32 = 40.0;
const POINT_MARGIN: f32 = 40.0;
const GRID: usize = (WORLD_SIZE / CELL) as usize;

/// A place in the search: a route waypoint (merged), or a floor in a grid
/// cell (whatever's there to stand on at its center, at about that height).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Place(u32),
    Cell(u32, i32),
}

type Found = (Key, [f32; 3]);

/// The check: walks every route, floods round every site, and searches
/// from every Landing's center as its faction.
pub fn check(w: &World) -> Report {
    let lanes = w.lanes();
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(16);
    // Route waypoints, merged where they're the same place (union-find).
    let mut places: Vec<[f32; 3]> = Vec::new();
    let mut spans = Vec::with_capacity(lanes.len());
    for l in lanes {
        spans.push(places.len()..places.len() + l.path.len());
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
    let node: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
    // The waypoints in each grid cell.
    let mut in_cell: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, p) in places.iter().enumerate() {
        let (x, y) = ((p[0] / CELL) as usize, (p[1] / CELL) as usize);
        if x < GRID && y < GRID {
            in_cell.entry((y * GRID + x) as u32).or_default().push(i);
        }
    }
    // The flood's area: round every site and every point.
    let mut area = vec![false; GRID * GRID];
    let mut mark = |c: [f32; 2], r: f32| {
        let (x0, x1) = (((c[0] - r) / CELL).max(0.0) as usize, (((c[0] + r) / CELL) as usize).min(GRID - 1));
        let (y0, y1) = (((c[1] - r) / CELL).max(0.0) as usize, (((c[1] + r) / CELL) as usize).min(GRID - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let q = [(x as f32 + 0.5) * CELL, (y as f32 + 0.5) * CELL];
                if sq(q[0] - c[0]) + sq(q[1] - c[1]) <= r * r {
                    area[y * GRID + x] = true;
                }
            }
        }
    };
    for s in w.sites() {
        mark(s.center, s.half + SITE_MARGIN);
    }
    for p in w.points() {
        mark([p.pos[0], p.pos[1]], POINT_MARGIN);
    }
    let area = &area;
    let owners: Vec<u8> = w.sites().iter().map(|s| s.sector).collect();
    let mut report = Report::default();
    for (pi, pt) in w.points().iter().enumerate() {
        let q = pt.pos;
        let s = MoveState { pos: [q[0], q[1]], vel: [0.0; 2], z: q[2], vz: 0.0, grounded: true, crouched: false };
        let who = Mover { faction: w.sites()[pt.site as usize].sector, owners: &owners };
        let pushed = crate::movement::nudge(w, s, [0.0, 0.0], who);
        if (pushed.pos[0] - q[0]).abs() > 0.01 || (pushed.pos[1] - q[1]).abs() > 0.01 || w.ground(q[0], q[1], q[2] + 0.5, 1.0) > q[2] + 0.3 {
            report.buried.push(pi);
        }
    }
    for (si, s) in w.sites().iter().enumerate().filter(|(_, s)| s.kind == SiteKind::Landing) {
        report.landings += 1;
        let who = Mover { faction: s.sector, owners: &owners };
        // The routes, as this faction.
        let route_ok: Vec<[bool; 2]> = std::thread::scope(|sc| {
            let chunk = lanes.len().div_ceil(threads).max(1);
            let handles: Vec<_> = lanes.chunks(chunk).map(|ls| sc.spawn(move || ls.iter().map(|l| [walk_as(w, l, false, who).is_ok(), !l.one_way && walk_as(w, l, true, who).is_ok()]).collect::<Vec<_>>())).collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let mut route_out: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (span, ok) in spans.iter().zip(&route_ok) {
            for i in span.start..span.end - 1 {
                if ok[0] {
                    route_out[node[i]].push(node[i + 1]);
                }
                if ok[1] {
                    route_out[node[i + 1]].push(node[i]);
                }
            }
        }
        // The search: breadth first, each wave's grid steps walked in parallel.
        let mut seen: HashMap<Key, [f32; 3]> = HashMap::new();
        let mut wave: Vec<Found> = Vec::new();
        let push = |seen: &mut HashMap<Key, [f32; 3]>, wave: &mut Vec<Found>, (k, p): Found| {
            if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(k) {
                e.insert(p);
                wave.push((k, p));
            }
        };
        let start = MoveState::standing(w, s.center);
        if let Some(f) = floor(w, area, start.pos, start.z, who) {
            push(&mut seen, &mut wave, f);
        }
        for (i, p) in places.iter().enumerate() {
            if (p[0] - s.center[0]).abs() < 8.0 && (p[1] - s.center[1]).abs() < 8.0 {
                push(&mut seen, &mut wave, (Key::Place(node[i] as u32), places[node[i]]));
            }
        }
        while !wave.is_empty() {
            let current = std::mem::take(&mut wave);
            // Grid steps out of this wave's floors, in parallel.
            let cells: Vec<(u32, [f32; 3])> = current.iter().filter_map(|&(k, p)| if let Key::Cell(c, _) = k { Some((c, p)) } else { None }).collect();
            let stepped: Vec<Vec<Found>> = std::thread::scope(|sc| {
                let chunk = cells.len().div_ceil(threads).max(1);
                let handles: Vec<_> = cells.chunks(chunk).map(|cs| sc.spawn(move || cs.iter().map(|&(c, p)| steps_from(w, area, c, p, who)).collect::<Vec<_>>())).collect();
                handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
            });
            report.steps += cells.len() * 8;
            for f in stepped.into_iter().flatten() {
                push(&mut seen, &mut wave, f);
            }
            for &(k, p) in &current {
                match k {
                    // Along the routes that walk, and onto the floor under it.
                    Key::Place(i) => {
                        for &j in &route_out[i as usize] {
                            push(&mut seen, &mut wave, (Key::Place(j as u32), places[j]));
                        }
                        if let Some(f) = floor(w, area, [p[0], p[1]], p[2], who) {
                            if (f.1[2] - p[2]).abs() <= MERGE_Z {
                                push(&mut seen, &mut wave, f);
                            }
                        }
                    }
                    // Onto the route waypoints in it, at its height.
                    Key::Cell(c, _) => {
                        for &j in in_cell.get(&c).map(|v| v.as_slice()).unwrap_or(&[]) {
                            if (places[j][2] - p[2]).abs() <= MERGE_Z {
                                push(&mut seen, &mut wave, (Key::Place(node[j] as u32), places[node[j]]));
                            }
                        }
                    }
                }
            }
        }
        report.places = report.places.max(seen.len());
        if report.landings == 1 {
            report.reached = seen.values().copied().collect();
        }
        if report.landings == 1 {
            // (A route that doesn't walk as faction 0.)
            for (r, l) in lanes.iter().enumerate() {
                if walk(w, l, false).is_err() {
                    report.broken_routes.push(r);
                }
            }
        }
        // The points: someone standing where they'd count for it.
        let mut reached: HashMap<(i32, i32), Vec<[f32; 3]>> = HashMap::new();
        for p in seen.values() {
            reached.entry(((p[0] / POINT_RADIUS) as i32, (p[1] / POINT_RADIUS) as i32)).or_default().push(*p);
        }
        for (pi, pt) in w.points().iter().enumerate() {
            let q = pt.pos;
            let (cx, cy) = ((q[0] / POINT_RADIUS) as i32, (q[1] / POINT_RADIUS) as i32);
            let counts = (-1..=1)
                .flat_map(|dy| (-1..=1).map(move |dx| (cx + dx, cy + dy)))
                .flat_map(|c| reached.get(&c).into_iter().flatten())
                .any(|p| sq(p[0] - q[0]) + sq(p[1] - q[1]) <= POINT_RADIUS * POINT_RADIUS && (p[2] - q[2]).abs() <= POINT_HEIGHT);
            if !counts {
                let nearest = seen.values().min_by(|a, b| dist3(**a, q).total_cmp(&dist3(**b, q))).copied().unwrap_or_default();
                report.unreached.push((pi, si, nearest));
            }
        }
    }
    report
}

fn sq(x: f32) -> f32 {
    x * x
}

fn dist3(a: [f32; 3], b: [f32; 3]) -> f32 {
    (sq(a[0] - b[0]) + sq(a[1] - b[1]) + sq(a[2] - b[2])).sqrt()
}

/// The floor of the grid cell at `p`, at about height `z`: where someone
/// stands at its center (None outside the flood's area, or where its center
/// is inside something).
fn floor(w: &World, area: &[bool], p: [f32; 2], z: f32, who: Mover) -> Option<Found> {
    let (x, y) = ((p[0] / CELL) as usize, (p[1] / CELL) as usize);
    if p[0] < 0.0 || p[1] < 0.0 || x >= GRID || y >= GRID || !area[y * GRID + x] {
        return None;
    }
    let c = [(x as f32 + 0.5) * CELL, (y as f32 + 0.5) * CELL];
    let mut s = MoveState { pos: c, vel: [0.0; 2], z: 0.0, vz: 0.0, grounded: true, crouched: false };
    s.z = w.ground(c[0], c[1], z + 1.0, 1.5);
    // Inside cover (or a shield it may not pass): nobody stands there.
    let pushed = crate::movement::nudge(w, s, [0.0, 0.0], who);
    if (pushed.pos[0] - c[0]).abs() > 0.01 || (pushed.pos[1] - c[1]).abs() > 0.01 || (s.z - z).abs() > 3.0 {
        return None;
    }
    Some((Key::Cell((y * GRID + x) as u32, (s.z * 8.0).round() as i32), [c[0], c[1], s.z]))
}

/// The floors a walk of a cell over (each of eight ways) from the floor at
/// `p` gets to: walked, then left to land.
fn steps_from(w: &World, area: &[bool], cell: u32, p: [f32; 3], who: Mover) -> Vec<Found> {
    let mut out = Vec::new();
    for (dx, dy) in [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)] {
        let to = [p[0] + dx as f32 * CELL, p[1] + dy as f32 * CELL];
        let mut s = MoveState { pos: [p[0], p[1]], vel: [0.0; 2], z: p[2], vz: 0.0, grounded: true, crouched: false };
        let l = ((dx * dx + dy * dy) as f32).sqrt();
        let (mx, my) = ((dx as f32 / l * 127.0) as i8, (dy as f32 / l * 127.0) as i8);
        // Walking (about two-thirds of a second at a run), then standing
        // still till it lands.
        let mut i = 0;
        while i < 40 && sq(s.pos[0] - to[0]) + sq(s.pos[1] - to[1]) > 0.36 {
            s = step(w, s, Input { move_x: mx, move_y: my, ..Default::default() }, who);
            i += 1;
        }
        let mut j = 0;
        while j < 90 && !(s.grounded && s.vz.abs() < 0.01) {
            s = step(w, s, Input::default(), who);
            j += 1;
        }
        if !s.grounded {
            continue;
        }
        if let Some(f) = floor(w, area, s.pos, s.z, who) {
            if !matches!(f.0, Key::Cell(c, _) if c == cell) {
                out.push(f);
            }
        }
    }
    out
}
