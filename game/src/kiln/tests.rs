//! K0's bars (docs/kiln.md, build plan): every lane and every route inside a
//! site walked end to end by the game's own movement, and the lanes' times
//! the same in every sector, within 10%.

use super::*;
use crate::movement::{step, Input, MoveState, HEIGHT};
use crate::world::{SiteKind, World, KILN};
use std::sync::Arc;

fn kiln() -> Arc<World> {
    World::shared(KILN)
}

pub(crate) use crate::reach::walk;

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
            if backward && l.one_way {
                continue;
            }
            if let Err(e) = walk(&w, l, backward) {
                let (a, b) = (w.sites()[l.a as usize], w.sites()[l.b as usize]);
                failed.push(format!("route {i} {:?} {} -> {:?} {} ({}): {e}", a.kind, l.a, b.kind, l.b, if backward { "back" } else { "forth" }));
            }
        }
    }
    for f in &failed {
        eprintln!("{f}");
    }
    let walks: usize = w.lanes().iter().map(|l| if l.one_way { 1 } else { 2 }).sum();
    eprintln!("{walks} walks, {} of them one way (lifts, drop shafts)", w.lanes().iter().filter(|l| l.one_way).count());
    assert!(failed.is_empty(), "{} of {walks} walks failed", failed.len());
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
        holes: Vec::new(),
        volumes: Vec::new(),
    });
    let l = crate::world::Lane { a: 0, b: 0, path: path.iter().zip(&zs).map(|(p, z)| [p[0] as f32, p[1] as f32, *z as f32]).collect(), one_way: false };
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

#[test]
fn tunnels_run_under_the_ground_from_holes() {
    use crate::world::Kind;
    let w = kiln();
    // Each sector's two Tunnel mouths joined by a lane that goes under the
    // ground, its floor at least 7.5 m under it all the way.
    let mouths: Vec<usize> = w.sites().iter().enumerate().filter(|(_, s)| s.kind == SiteKind::TunnelMouth).map(|(i, _)| i).collect();
    let tunnels: Vec<_> = w.lanes().iter().filter(|l| mouths.contains(&(l.a as usize)) && mouths.contains(&(l.b as usize))).collect();
    assert_eq!(tunnels.len(), 3, "one tunnel a sector");
    for l in tunnels {
        let under: Vec<&[f32; 3]> = l.path.iter().filter(|p| w.under(p[0], p[1], p[2]) && !w.hole(p[0], p[1])).collect();
        assert!(under.len() * 8 > 1000, "{} m of it under the ground", under.len() * 8);
        for p in &under {
            assert!(w.terrain(p[0], p[1]) - p[2] >= 7.5, "deep enough at {p:?}");
        }
        // It starts and ends down a hole.
        let holes = l.path.iter().filter(|p| w.hole(p[0], p[1])).count();
        assert!(holes >= 6, "{holes} waypoints in the trenches");
    }
    // Every hole is a trench's (a ramp in it) or a shaft's (a lift or a drop
    // shaft in it, a chamber's floor under it): never a fall to the world's bottom.
    let (mut cells, mut shafts) = (0, 0);
    for cy in 0..crate::world::CELLS {
        for cx in 0..crate::world::CELLS {
            if w.hole_cell(cx, cy) {
                cells += 1;
                let (x, y) = ((cx as f32 + 0.5) * 4.0, (cy as f32 + 0.5) * 4.0);
                let t = w.terrain(x, y);
                let floor = w.ground(x, y, t + 1.0, 1.5);
                if floor > t - 14.0 {
                    continue; // a trench's ramp
                }
                shafts += 1;
                assert!(floor > 1.0 && w.volume_at(x, y, t - 3.0).is_some(), "a shaft at ({x}, {y}): floor {floor}");
            }
        }
    }
    eprintln!("{cells} hole cells, {shafts} of them shafts'");
    assert!(cells >= 6 * 20 && (cells - shafts) % 20 == 0 && shafts % 4 == 0);
    let _ = Kind::Shield;
}

#[test]
fn landing_spawn_rooms_shield_their_own() {
    use crate::world::Kind;
    let w = kiln();
    for (i, s) in w.sites().iter().enumerate().filter(|(_, s)| s.kind == SiteKind::Landing) {
        let shields: Vec<_> = w.boxes().iter().filter(|b| b.kind == Kind::Shield && (b.center[0] - s.center[0]).abs().max((b.center[1] - s.center[1]).abs()) < 80.0).collect();
        assert_eq!(shields.len(), 2, "two doors at Landing {i}");
        for b in shields {
            assert_eq!(b.pass, s.sector, "its faction's");
            // Through the door from outside: its faction in, the others not.
            let out = b.world_dir([1.0, 0.0]);
            let n = if b.half[0] < b.half[1] { out } else { b.world_dir([0.0, 1.0]) };
            for f in 0..3u8 {
                let start = [b.center[0] + n[0] * 4.0, b.center[1] + n[1] * 4.0];
                let mut m = MoveState::standing(&w, start);
                for _ in 0..60 {
                    m = step(&w, m, Input { move_x: (-n[0] * 127.0) as i8, move_y: (-n[1] * 127.0) as i8, ..Default::default() }, f);
                }
                let through = (m.pos[0] - b.center[0]) * n[0] + (m.pos[1] - b.center[1]) * n[1] < -1.0;
                assert_eq!(through, f == s.sector, "faction {f} at Landing {i}'s door: {m:?}");
            }
        }
    }
}

#[test]
fn a_sites_shields_let_its_holder_through() {
    use crate::movement::Mover;
    use crate::world::{Kind, NO_SITE};
    let w = kiln();
    // A Span's gallery spawn room: its door's shield belongs to the Span.
    let span = w.sites().iter().position(|s| s.kind == SiteKind::Span).unwrap();
    // (The door with floor outside it: the one onto the gallery.)
    let (b, n) = w
        .boxes()
        .iter()
        .filter(|b| b.kind == Kind::Shield && b.site == span as u16)
        .flat_map(|b| {
            let n = if b.half[0] < b.half[1] { b.world_dir([1.0, 0.0]) } else { b.world_dir([0.0, 1.0]) };
            [(*b, n), (*b, [-n[0], -n[1]])]
        })
        .find(|(b, n)| {
            let (x, y) = (b.center[0] + n[0] * 4.0, b.center[1] + n[1] * 4.0);
            (w.ground(x, y, b.bottom + 0.8, 1.0) - b.bottom).abs() < 0.6 && (w.ground(b.center[0] - n[0] * 3.0, b.center[1] - n[1] * 3.0, b.bottom + 0.8, 1.0) - b.bottom).abs() < 0.6
        })
        .expect("the Span's spawn room's door onto its gallery");
    assert_ne!(b.site, NO_SITE);
    let through = |faction: u8, owners: &[u8]| {
        let start = [b.center[0] + n[0] * 4.0, b.center[1] + n[1] * 4.0];
        let mut m = MoveState::standing_at(&w, start, b.bottom + 0.3);
        for _ in 0..60 {
            m = step(&w, m, Input { move_x: (-n[0] * 127.0) as i8, move_y: (-n[1] * 127.0) as i8, ..Default::default() }, Mover { faction, owners });
        }
        (m.pos[0] - b.center[0]) * n[0] + (m.pos[1] - b.center[1]) * n[1] < -1.0
    };
    let held = w.sites()[span].sector;
    let other = (held + 1) % 3;
    let mut owners: Vec<u8> = w.sites().iter().map(|s| s.sector).collect();
    assert!(through(held, &owners) && !through(other, &owners), "its sector's faction's at the start");
    owners[span] = other;
    assert!(through(other, &owners) && !through(held, &owners), "the new holder's once it flips");
    owners[span] = crate::territory::NONE;
    assert!((0..3).all(|f| !through(f, &owners)), "no one's while no one holds it");
}

#[test]
fn every_point_is_reached_from_every_landing() {
    // The exporter's reachability check (GDD §16), on Kiln.
    let w = kiln();
    let t = std::time::Instant::now();
    let r = crate::reach::check(&w);
    eprintln!("{} places reached, {} grid steps walked, in {:.1} s; {} Landings", r.places, r.steps, t.elapsed().as_secs_f32(), r.landings);
    assert!(r.broken_routes.is_empty(), "routes that don't walk: {:?}", r.broken_routes);
    assert_eq!(r.landings, 3);
    let buried: Vec<String> = r.buried.iter().map(|&p| format!("point {p} of {:?} at {:?}", w.sites()[w.points()[p].site as usize].kind, w.points()[p].pos)).collect();
    assert!(buried.is_empty(), "points inside something: {buried:#?}");
    let named: Vec<String> = r.unreached.iter().map(|&(p, l, near)| {
        let pt = w.points()[p];
        format!("point {p} of {:?} {} at ({:.0}, {:.0}, {:.1}) from Landing {l}: nearest reached ({:.0}, {:.0}, {:.1})", w.sites()[pt.site as usize].kind, pt.site, pt.pos[0], pt.pos[1], pt.pos[2], near[0], near[1], near[2])
    }).collect();
    assert!(r.unreached.is_empty(), "{} unreached: {named:#?}", r.unreached.len());
}

#[test]
fn every_base_and_great_site_has_a_spawn_room_that_sees_no_point() {
    use crate::world::{Kind, NO_SITE};
    let w = kiln();
    let sites = w.sites();
    let shields: Vec<_> = w.boxes().iter().filter(|b| b.kind == Kind::Shield && b.site != NO_SITE).copied().collect();
    for (i, s) in sites.iter().enumerate() {
        let wants = s.kind.base() || s.kind.major() || s.kind == SiteKind::Landing;
        let has = shields.iter().any(|b| b.site as usize == i);
        assert!(!wants || has, "site {i} ({:?}) has no spawn room", s.kind);
    }
    // From a step inside each door (under its roof), at eye height, no point
    // of its site is in sight (GDD §6.3): the baffles and walls hide them.
    let mut doors = 0;
    for b in shields.iter().filter(|b| {
        let k = sites[b.site as usize].kind;
        k.base() || k == SiteKind::Geode
    }) {
        let n = if b.half[0] < b.half[1] { b.world_dir([1.0, 0.0]) } else { b.world_dir([0.0, 1.0]) };
        let floor = b.bottom + 0.3;
        let inside = [-1.0f32, 1.0].into_iter().map(|s| [b.center[0] + n[0] * s * 1.2, b.center[1] + n[1] * s * 1.2]).find(|q| w.ceiling(q[0], q[1], 0.1, floor + 1.0) < floor + 6.0).expect("a door under a roof on one side");
        let eye = [inside[0], inside[1], floor + 1.6];
        let owners: Vec<u8> = sites.iter().map(|s| s.sector).collect();
        for p in w.points().iter().filter(|p| p.site == b.site) {
            let seen = crate::hit::line_clear(&w, eye, [p.pos[0], p.pos[1], p.pos[2] + 1.0], crate::movement::Mover { faction: sites[b.site as usize].sector, owners: &owners });
            assert!(!seen, "site {} ({:?}): its point at {:?} is in sight from the door at {eye:?}", b.site, sites[b.site as usize].kind, p.pos);
        }
        doors += 1;
    }
    assert_eq!(doors, 2 * (18 + 3), "two doors at each base and Geode");
}
