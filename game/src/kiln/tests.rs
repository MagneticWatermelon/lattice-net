//! K0's bars (docs/kiln.md, build plan): every lane and every route inside a
//! site walked end to end by the game's own movement, and the lanes' times
//! the same in every sector, within 10%.

use super::*;
use crate::movement::{step, Input, MoveState, BUTTON_SPRINT, DT, HEIGHT};
use crate::world::{SiteKind, World, KILN};
use std::sync::Arc;

fn kiln() -> Arc<World> {
    World::shared(KILN)
}

/// Walks lane `l` (backward from its end if `backward`), sprinting: the time
/// it took, or where it got stuck.
pub(crate) fn walk(w: &World, l: &crate::world::Lane, backward: bool) -> Result<f32, String> {
    let start = l.waypoint(0, backward);
    let mut s = MoveState::standing(w, [start[0], start[1]]);
    // On whatever's there (a stair's top, a deck): the highest surface
    // under the route's own height.
    s.z = w.ground(start[0], start[1], start[2].max(s.z) + 1.0, 1.5).max(s.z);
    let mut next = 1;
    let budget = (l.length() / 9.0 * 1.6 + 30.0) / DT;
    let mut best = (f32::MAX, 0u32);
    for i in 0..budget as u32 {
        let Some((mx, my)) = l.steer([s.pos[0], s.pos[1], s.z], &mut next, backward) else {
            let end = l.waypoint(l.path.len() - 1, backward);
            let dz = s.z - end[2];
            return if dz > -4.0 { Ok(i as f32 * DT) } else { Err(format!("reached the end at z {:.1}, not {:.1}", s.z, end[2])) };
        };
        // Stuck: no closer to the next waypoint in 10 s.
        let w_next = l.waypoint(next.min(l.path.len() - 1), backward);
        let d = ((w_next[0] - s.pos[0]) * (w_next[0] - s.pos[0]) + (w_next[1] - s.pos[1]) * (w_next[1] - s.pos[1])).sqrt();
        if d < best.0 - 0.5 || next as u32 != best.1 {
            best = (d, next as u32);
            best.0 = d;
        }
        s = step(w, s, Input { move_x: mx, move_y: my, yaw: 0, pitch: 0, buttons: BUTTON_SPRINT });
    }
    let w_next = l.waypoint(next.min(l.path.len() - 1), backward);
    Err(format!("stuck at ({:.0}, {:.0}, {:.1}) heading for waypoint {next}/{} at ({:.0}, {:.0}, {:.1})", s.pos[0], s.pos[1], s.z, l.path.len(), w_next[0], w_next[1], w_next[2]))
}

#[test]
fn kiln_is_built_to_its_design() {
    let w = kiln();
    let sites = w.sites();
    assert_eq!(sites.len(), 67);
    let count = |k: SiteKind, sector: u8| sites.iter().filter(|s| s.kind == k && s.sector == sector).count();
    for sector in 0..3u8 {
        for (kind, n) in [(SiteKind::Landing, 1), (SiteKind::Geode, 1), (SiteKind::Span, 1), (SiteKind::Cliffhold, 1), (SiteKind::MesaCitadel, 1), (SiteKind::SlotHold, 1), (SiteKind::RiftLedge, 2)] {
            assert_eq!(count(kind, sector), n, "{kind:?} in sector {sector}");
        }
        for kind in [SiteKind::Arch, SiteKind::Vent, SiteKind::TunnelMouth, SiteKind::SpirePerch, SiteKind::WellShrine, SiteKind::Wreck] {
            assert_eq!(count(kind, sector), 2, "{kind:?} in sector {sector}");
        }
        assert_eq!(sites.iter().filter(|s| s.sector == sector && s.kind.base()).count(), 6);
    }
    assert_eq!(count(SiteKind::Hollow, crate::world::NO_SECTOR), 1);
    // Heights: the layers where the design puts them, all inside the wire's 0-655 m.
    let h = |deg: f64, r: f64| {
        let p = Frame::new(0).polar(r, deg);
        w.terrain(p[0] as f32, p[1] as f32)
    };
    let (lo, hi) = (0..TERRAIN_N).flat_map(|y| (0..TERRAIN_N).map(move |x| (x, y))).step_by(7).fold((f32::MAX, f32::MIN), |(l, hh), (x, y)| {
        let v = w.sample(x, y);
        (l.min(v), hh.max(v))
    });
    eprintln!("heights {lo:.1}-{hi:.1} m");
    assert!(lo >= 60.0 && hi <= 470.0, "{lo} {hi}");
    let crown = h(-5.0, 1100.0);
    assert!((crown - 420.0).abs() < 12.0, "the Crown at {crown}");
    let floor = w.terrain(4096.0 + 200.0, 4096.0 - 150.0);
    assert!((floor - 80.0).abs() < 3.0, "the Hollow's floor at {floor}");
    let rim = w.terrain(150.0, 4096.0);
    assert!((rim - 460.0).abs() < 10.0, "the Rim at {rim}");
    let rift = Rift::new(0);
    let span_floor = rift.at(SPAN_ALONG, 0.0);
    let f = w.terrain(span_floor[0] as f32, span_floor[1] as f32);
    assert!(f > 75.0 && f < 95.0, "the rift's floor at the Span: {f}");
    let top = w.boxes().iter().map(|b| b.top).fold(0.0f32, f32::max);
    assert!((top - 610.0).abs() < 1.0 && top < 655.0, "the Heartwell's top {top}");
    // About 110 lanes, 2-4 a site (more at the great sites).
    let lanes: Vec<_> = w.lanes().iter().filter(|l| l.a != l.b).collect();
    eprintln!("{} lanes, {} routes inside sites", lanes.len(), w.lanes().len() - lanes.len());
    assert!(lanes.len() >= 100 && lanes.len() <= 125, "{}", lanes.len());
    for (i, s) in sites.iter().enumerate() {
        let n = lanes.iter().filter(|l| l.a as usize == i || l.b as usize == i).count();
        let (lo, hi) = match s.kind {
            SiteKind::Hollow => (6, 6),
            SiteKind::Landing => (3, 3),
            k if k.major() => (3, 7),
            _ => (2, 5),
        };
        assert!(n >= lo && n <= hi, "{:?} at ({:.0}, {:.0}) has {n} lanes", s.kind, s.center[0], s.center[1]);
    }
    let lengths: Vec<f32> = lanes.iter().map(|l| l.length()).collect();
    let within = lengths.iter().filter(|&&l| (450.0..=1700.0).contains(&l)).count();
    eprintln!("lane lengths {:.0}-{:.0} m, {within} of {} within 450-1,700", lengths.iter().copied().fold(f32::MAX, f32::min), lengths.iter().copied().fold(0.0, f32::max), lengths.len());
    assert!(within * 10 >= lengths.len() * 8, "most lanes 500-1,500 m");
}

#[test]
fn every_route_walks_end_to_end() {
    let w = kiln();
    let mut failed = Vec::new();
    for (i, l) in w.lanes().iter().enumerate() {
        for backward in [false, true] {
            if let Err(e) = walk(&w, l, backward) {
                let (a, b) = (w.sites()[l.a as usize], w.sites()[l.b as usize]);
                failed.push(format!("route {i} {:?} {} -> {:?} {} ({}): {e}", a.kind, l.a, b.kind, l.b, if backward { "back" } else { "forth" }));
            }
        }
    }
    for f in &failed {
        eprintln!("{f}");
    }
    assert!(failed.is_empty(), "{} of {} walks failed", failed.len(), 2 * w.lanes().len());
}

#[test]
fn every_sector_is_as_far_from_the_hollow() {
    let w = kiln();
    let times: Vec<[Option<f32>; 2]> = w.lanes().iter().map(|l| if l.a == l.b { [None, None] } else { [walk(&w, l, false).ok(), walk(&w, l, true).ok()] }).collect();
    let f = fairness(&w, &|i, back| times[i][back as usize]);
    for (k, &(t, n)) in f.hollow.iter().enumerate() {
        eprintln!("sector {k}: Landing to the Hollow {t:.1} s, {n} lanes ({:.0} m sprinting)", t * 9.0);
        assert_eq!(n, 3, "three lanes from the Landing to the Hollow");
    }
    let t: Vec<f32> = f.hollow.iter().map(|h| h.0).collect();
    let (lo, hi) = (t.iter().copied().fold(f32::MAX, f32::min), t.iter().copied().fold(0.0, f32::max));
    assert!(hi / lo <= 1.10, "within 10%: {t:?}");
    // Every kind of site as far a walk from its own Landing in every sector,
    // within 10% ("the same kinds of sites at the same lane distances").
    eprintln!("each site's walk from its Landing differs across sectors by at most {:.1}% (site {})", (f.spread - 1.0) * 100.0, f.worst_site);
    assert!(f.spread <= 1.10);
}

#[test]
fn cover_cells_keep_to_their_box_budget() {
    let w = kiln();
    let mut worst = (0, 0.0f32, 0.0f32);
    let cell = 32.0f32;
    let dim = (crate::movement::WORLD_SIZE / cell) as usize;
    for cy in 0..dim {
        for cx in 0..dim {
            let (x, y) = ((cx as f32 + 0.5) * cell, (cy as f32 + 0.5) * cell);
            let mut n = 0;
            w.boxes_near(x, y, 0.0, |_| n += 1);
            if n > worst.0 {
                worst = (n, x, y);
            }
        }
    }
    eprintln!("{} boxes; the busiest cover cell has {} (at {:.0}, {:.0})", w.boxes().len(), worst.0, worst.1, worst.2);
    assert!(worst.0 <= 200, "at most 200 boxes in a 32 m cell");
    let _ = HEIGHT;
}

#[test]
fn stairs_climb() {
    use crate::world::{Continent, Parts, TERRAIN_N};
    let mut boxes = Vec::new();
    let (path, zs) = super::build::test_stairs([100.0, 100.0], 9.0, 10.0, 40.0, &mut boxes);
    let w = World::from_parts(Parts {
        id: 9,
        continent: Continent::Kiln,
        heights: vec![1000; TERRAIN_N * TERRAIN_N],
        sites: Vec::new(),
        boxes,
        ground: Vec::new(),
        lanes: Vec::new(),
        points: Vec::new(),
    });
    let l = crate::world::Lane { a: 0, b: 0, path: path.iter().zip(&zs).map(|(p, z)| [p[0] as f32, p[1] as f32, *z as f32]).collect() };
    let up = walk(&w, &l, false);
    assert!(up.is_ok(), "up: {up:?}");
    let down = walk(&w, &l, true);
    assert!(down.is_ok(), "down: {down:?}");
}

#[test]
fn kiln_builds_the_same_bits_twice() {
    let (a, b) = (generate(), generate());
    assert_eq!(a.digest(), b.digest());
}
