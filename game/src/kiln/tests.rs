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
    // About 110 lanes, 2-4 a site (more at the great sites): links between
    // sites (a tunnel under a lane links what the lane does).
    let lanes: Vec<_> = w.lanes().iter().filter(|l| l.a != l.b).collect();
    let mut links: Vec<(u16, u16)> = lanes.iter().map(|l| (l.a.min(l.b), l.a.max(l.b))).collect();
    links.sort_unstable();
    links.dedup();
    eprintln!("{} links, {} routes between sites ({} of them tunnels), {} inside sites", links.len(), lanes.len(), lanes.iter().filter(|l| l.tunnel(&w)).count(), w.lanes().len() - lanes.len());
    assert!(links.len() >= 100 && links.len() <= 125, "{}", links.len());
    for (i, s) in sites.iter().enumerate() {
        let n = links.iter().filter(|l| l.0 as usize == i || l.1 as usize == i).count();
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
            // (The clockwork's routes when they're there: a bridge at night.)
            let who = crate::movement::Mover { faction: crate::reach::walker(&w, l), owners: &[], live: crate::clock::open(l.when).0 };
            if let Err(e) = crate::reach::walk_as(&w, l, backward, who) {
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
    let l = crate::world::Lane { a: 0, b: 0, path: path.iter().zip(&zs).map(|(p, z)| [p[0] as f32, p[1] as f32, *z as f32]).collect(), one_way: false, when: 0 };
    let up = walk(&w, &l, false);
    assert!(up.is_ok(), "up: {up:?}");
    let down = walk(&w, &l, true);
    assert!(down.is_ok(), "down: {down:?}");
    // Railed (falls are a choice, K4): from anywhere on it, a walk straight
    // out over its outer side falls nowhere.
    use crate::movement::{fall_height, step, Input, MoveState, SAFE_FALL};
    for (p, z) in path.iter().zip(&zs).skip(2) {
        for k in 0..=8 {
            // (Within the flight: the path is 1.2 m from its edge.)
            let at = [p[0] + (k as f64 - 4.0) * 0.15, p[1] + (k as f64 - 4.0) * 0.15];
            let out = [at[0] - 104.5, at[1] - 104.5];
            let n = (out[0] * out[0] + out[1] * out[1]).sqrt();
            let input = Input { move_x: (127.0 * out[0] / n) as i8, move_y: (127.0 * out[1] / n) as i8, ..Input::default() };
            let mut s = MoveState::standing_at(&w, [at[0] as f32, at[1] as f32], *z as f32);
            for _ in 0..90 {
                let next = step(&w, s, input, 0u8);
                assert!(fall_height(&s, &next) <= SAFE_FALL, "walked off the stair from ({:.1}, {:.1}, {z:.1})", at[0], at[1]);
                s = next;
            }
            assert!(s.z > *z as f32 - 3.0, "fell off the stair from ({:.1}, {:.1}, {z:.1}) to {:.1}", at[0], at[1], s.z);
        }
    }
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
    let tunnels: Vec<_> = w.lanes().iter().filter(|l| l.a != l.b && mouths.contains(&(l.a as usize)) && mouths.contains(&(l.b as usize))).collect();
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
    // Every hole is a trench's (a ramp in it), a shaft's (a lift or a drop
    // shaft in it, a chamber's floor under it) or a gallery's window's
    // (plugged above and below it: `rift_walls_have_galleries`): never a
    // fall to the world's bottom.
    let k = build_kiln();
    let windows: Vec<(usize, usize)> = k.openings.iter().flat_map(|g| g.bays.iter().flat_map(|b| b.cells.iter().copied())).collect();
    let (mut cells, mut shafts) = (0, 0);
    for cy in 0..crate::world::CELLS {
        for cx in 0..crate::world::CELLS {
            if w.hole_cell(cx, cy) && !windows.contains(&(cx, cy)) {
                cells += 1;
                let (x, y) = ((cx as f32 + 0.5) * 4.0, (cy as f32 + 0.5) * 4.0);
                let t = w.terrain(x, y);
                let floor = w.ground(x, y, t + 1.0, 1.5, crate::clock::Live::OFF);
                if w.volume_at(x, y, t - 3.0, crate::clock::Live::OFF).is_none() {
                    assert!(floor > t - 14.0, "a hole at ({x}, {y}) with no ramp or shaft in it");
                    continue; // a trench's ramp
                }
                shafts += 1;
                assert!(floor > 1.0, "a shaft at ({x}, {y}): floor {floor}");
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
            (w.ground(x, y, b.bottom + 0.8, 1.0, crate::clock::Live::OFF) - b.bottom).abs() < 0.6 && (w.ground(b.center[0] - n[0] * 3.0, b.center[1] - n[1] * 3.0, b.bottom + 0.8, 1.0, crate::clock::Live::OFF) - b.bottom).abs() < 0.6
        })
        .expect("the Span's spawn room's door onto its gallery");
    assert_ne!(b.site, NO_SITE);
    let through = |faction: u8, owners: &[u8]| {
        let start = [b.center[0] + n[0] * 4.0, b.center[1] + n[1] * 4.0];
        let mut m = MoveState::standing_at(&w, start, b.bottom + 0.3);
        for _ in 0..60 {
            m = step(&w, m, Input { move_x: (-n[0] * 127.0) as i8, move_y: (-n[1] * 127.0) as i8, ..Default::default() }, Mover { faction, owners, live: crate::clock::Live::OFF });
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
    // (Its doors: not the shields at the foot of its covered exit, K3.)
    let doors_of: Vec<_> = shields.iter().filter(|b| b.bottom > w.terrain(b.center[0], b.center[1]) - 4.0).copied().collect();
    for (i, s) in sites.iter().enumerate() {
        let wants = s.kind.base() || s.kind.major() || s.kind == SiteKind::Landing;
        let has = shields.iter().any(|b| b.site as usize == i);
        assert!(!wants || has, "site {i} ({:?}) has no spawn room", s.kind);
    }
    // From a step inside each door (under its roof), at eye height, no point
    // of its site is in sight (GDD §6.3): the baffles and walls hide them.
    let mut doors = 0;
    for b in doors_of.iter().filter(|b| {
        let k = sites[b.site as usize].kind;
        k.base() || k == SiteKind::Geode
    }) {
        let n = if b.half[0] < b.half[1] { b.world_dir([1.0, 0.0]) } else { b.world_dir([0.0, 1.0]) };
        let floor = b.bottom + 0.3;
        let inside = [-1.0f32, 1.0].into_iter().map(|s| [b.center[0] + n[0] * s * 1.2, b.center[1] + n[1] * s * 1.2]).find(|q| w.ceiling(q[0], q[1], 0.1, floor + 1.0, crate::clock::Live::OFF) < floor + 6.0).expect("a door under a roof on one side");
        let eye = [inside[0], inside[1], floor + 1.6];
        let owners: Vec<u8> = sites.iter().map(|s| s.sector).collect();
        for p in w.points().iter().filter(|p| p.site == b.site) {
            let seen = crate::hit::line_clear(&w, eye, [p.pos[0], p.pos[1], p.pos[2] + 1.0], crate::movement::Mover { faction: sites[b.site as usize].sector, owners: &owners, live: crate::clock::Live::OFF });
            assert!(!seen, "site {} ({:?}): its point at {:?} is in sight from the door at {eye:?}", b.site, sites[b.site as usize].kind, p.pos);
        }
        doors += 1;
    }
    assert_eq!(doors, 2 * (18 + 3), "two doors at each base and Geode");
}

#[test]
#[ignore]
fn dump_tunnels() {
    // KILN_SITE=N cargo test -p lattice-game --release dump_tunnels -- --ignored --nocapture
    let site: usize = std::env::var("KILN_SITE").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    let k = build_kiln();
    for (i, c) in k.chambers.iter().enumerate().filter(|(_, c)| c.site == site) {
        eprintln!("chamber {i}: center ({:.0}, {:.0}) stand ({:.0}, {:.0}) half {} high {} floor {:.1} surface {:.1} trench {} outs {:?}", c.center[0], c.center[1], c.stand[0], c.stand[1], c.half, c.high, c.floor, c.surface, c.trench, c.outs);
    }
    for b in k.bores.iter().filter(|b| k.chambers[b.4 .0].site == site || k.chambers[b.4 .1].site == site) {
        eprintln!("bore {:?}: ({:.0}, {:.0}) -> ({:.0}, {:.0}) z {:.1}-{:.1}", b.4, b.0[0], b.0[1], b.1[0], b.1[1], b.2, b.3);
    }
    for (i, r) in k.tunnels.iter().enumerate().filter(|(_, r)| r.a == site || r.b == site) {
        eprintln!("tunnel {i} {} -> {} one way {}", r.a, r.b, r.one_way);
        for l in &r.legs {
            let pts: Vec<String> = l.path.iter().enumerate().map(|(j, p)| format!("({:.0},{:.0},{})", p[0], p[1], l.z.as_ref().map_or("g".to_string(), |z| format!("{:.1}", z[j])))).collect();
            eprintln!("   {}", pts.join(" "));
        }
    }
    for p in k.points.iter().filter(|p| p.site as usize == site) {
        eprintln!("point {:?}", p.pos);
    }
}

#[test]
fn every_base_has_its_tunnels() {
    use crate::world::{Kind, NO_SITE};
    // K3 (docs/kiln.md, "Tunnels"): under every base, its undercroft and a
    // shaft up from it; its spawn room's covered exit, shielded at its inner
    // end for whoever holds the site; an approach tunnel from outside it.
    let w = kiln();
    let sites = w.sites();
    let owners: Vec<u8> = sites.iter().map(|s| s.sector).collect();
    for (i, s) in sites.iter().enumerate().filter(|(_, s)| s.kind.base()) {
        let under = |p: &[f32; 3]| w.terrain(p[0], p[1]) - p[2] > 8.0;
        let routes: Vec<&crate::world::Lane> = w.lanes().iter().filter(|l| l.a as usize == i && l.b as usize == i).collect();
        // A lift up from under the ground to it.
        let up = routes.iter().any(|l| l.one_way && under(&l.path[0]) && !under(l.path.last().unwrap()));
        assert!(up, "{:?} {i}: no shaft up from its undercroft", s.kind);
        // The covered exit's shield, under the ground, its holder's.
        let foot: Vec<_> = w.boxes().iter().filter(|b| b.kind == Kind::Shield && b.site == i as u16 && b.bottom < w.terrain(b.center[0], b.center[1]) - 8.0).collect();
        assert!(!foot.is_empty() && foot.iter().all(|b| b.site != NO_SITE && b.pass == s.sector), "{:?} {i}: its covered exit's shield", s.kind);
        // Walked by its holder (the route tests walk it so), never by the others.
        let crosses = |l: &crate::world::Lane, b: &crate::world::CoverBox| {
            l.path.windows(2).any(|s| {
                (0..=40).any(|j| {
                    let t = j as f32 / 40.0;
                    let p = [s[0][0] + (s[1][0] - s[0][0]) * t, s[0][1] + (s[1][1] - s[0][1]) * t, s[0][2] + (s[1][2] - s[0][2]) * t];
                    b.covers(p[0], p[1]) && p[2] > b.bottom - 1.0 && p[2] < b.top
                })
            })
        };
        let exits: Vec<_> = routes.iter().filter(|l| foot.iter().any(|b| crosses(l, b))).collect();
        assert!(!exits.is_empty(), "{:?} {i}: no route through its covered exit", s.kind);
        for l in &exits {
            assert!(crate::reach::walk_as(&w, l, false, s.sector.into()).is_ok());
            let other = (s.sector + 1) % 3;
            assert!(crate::reach::walk_as(&w, l, false, crate::movement::Mover { faction: other, owners: &owners, live: crate::clock::Live::OFF }).is_err(), "{:?} {i}: faction {other} through its covered exit", s.kind);
        }
        // An approach tunnel: from the ground well outside it, mostly under the ground.
        let approach = routes.iter().any(|l| {
            let p = l.path[0];
            let (dx, dy) = (p[0] - s.center[0], p[1] - s.center[1]);
            (dx * dx + dy * dy).sqrt() > s.half + 60.0 && l.tunnel(&w)
        });
        assert!(approach, "{:?} {i}: no approach tunnel", s.kind);
    }
}

#[test]
fn the_undervein_runs_under_the_lanes_and_the_rifts() {
    let w = kiln();
    let mut pairs: Vec<(u16, u16)> = w.lanes().iter().filter(|l| l.a != l.b && l.tunnel(&w)).map(|l| (l.a.min(l.b), l.a.max(l.b))).collect();
    pairs.sort_unstable();
    pairs.dedup();
    // About 12 tubes between sites (docs/kiln.md): each sector's three, two under each rift.
    eprintln!("{} tunnels between sites", pairs.len());
    assert_eq!(pairs.len(), 15);
    // Each under a lane: what it links, a lane on the ground links too
    // (but between two Tunnel mouths, K1's: the tube is their lane).
    for &(a, b) in &pairs {
        let mouths = [a, b].iter().all(|&s| w.sites()[s as usize].kind == SiteKind::TunnelMouth);
        assert!(mouths || w.lanes().iter().any(|l| (l.a.min(l.b), l.a.max(l.b)) == (a, b) && !l.tunnel(&w)), "the tunnel from {a} to {b} isn't under a lane");
    }
    // Two under each rift's floor.
    for j in 0..3 {
        let rift = Rift::new(j);
        let under: Vec<_> = pairs
            .iter()
            .filter(|&&(a, b)| {
                w.lanes().iter().filter(|l| (l.a.min(l.b), l.a.max(l.b)) == (a, b)).any(|l| {
                    l.path.iter().any(|p| {
                        let (along, across) = rift.coords([p[0] as f64, p[1] as f64]);
                        across.abs() < Rift::half_width(along) - 30.0 && (p[2] as f64) < Rift::floor(along) - 8.0
                    })
                })
            })
            .collect();
        assert_eq!(under.len(), 2, "rift {j}: {under:?}");
    }
}

#[test]
fn tunnels_have_no_dead_ends() {
    // Every chamber has two ways out at the least: tubes, shafts, a trench.
    let k = build_kiln();
    for (i, c) in k.chambers.iter().enumerate() {
        let ways = c.trench as usize + c.outs.len() + c.shafts + c.doors.iter().filter(|d| d.3).count();
        assert!(ways >= 2, "chamber {i} of site {} at ({:.0}, {:.0}, {:.0}): {ways} way out", c.site, c.center[0], c.center[1], c.floor);
    }
    eprintln!("{} chambers, {} tubes", k.chambers.len(), k.bores.len());
}

/// What you see is what blocks you round an opening: every cell cut out of
/// the face is filled with boxes but for its band (its floor to its
/// headroom). The cells checked.
fn sealed(w: &World, o: &super::under::Opening, what: &str) -> usize {
    let mut cells = 0;
    let solid = |x: f32, y: f32, z: f32| {
        let mut hit = false;
        w.boxes_near(x, y, 0.0, |b| hit |= b.kind == crate::world::Kind::Block && b.covers(x, y) && b.bottom <= z && z <= b.top);
        hit
    };
    let (zg, high) = (o.floor as f32, o.high as f32);
    for b in &o.bays {
        for &(cx, cy) in &b.cells {
            cells += 1;
            let corners = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| w.sample(cx + dx, cy + dy));
            let (lo, hi) = corners.iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
            for (fx, fy) in [(0.1, 0.1), (0.5, 0.5), (0.9, 0.9), (0.1, 0.9), (0.9, 0.1)] {
                let (x, y) = ((cx as f32 + fx) * 4.0, (cy as f32 + fy) * 4.0);
                let mut z = lo;
                while z <= hi {
                    if z < zg - 0.05 || z > zg + high + 0.05 {
                        assert!(solid(x, y, z), "a gap in the face at ({x}, {y}, {z}) round {what}");
                    }
                    z += 0.5;
                }
            }
        }
    }
    cells
}

#[test]
fn the_reef_warren_has_caves() {
    let k = build_kiln();
    let w = kiln();
    let warren = w.sites().iter().position(|s| s.kind == SiteKind::ReefWarren).unwrap();
    let doors: Vec<_> = k.openings.iter().filter(|o| o.door).collect();
    let mut rooms: Vec<usize> = doors.iter().map(|o| o.room).collect();
    rooms.sort_unstable();
    rooms.dedup();
    // A cave in each of its four blocks of reef, two ways out of each.
    assert_eq!(rooms.len(), 4);
    let mut cells = 0;
    for &r in &rooms {
        let c = &k.chambers[r];
        assert_eq!(c.site, warren);
        assert_eq!(doors.iter().filter(|o| o.room == r).count(), 2, "the cave at {:?}", c.center);
        for o in doors.iter().filter(|o| o.room == r) {
            cells += sealed(&w, o, &format!("a doorway of the cave at {:?}", c.center));
        }
        // Its route, in at one doorway and out at the other, walked both ways.
        let near = |p: &[f32; 3]| {
            let (dx, dy) = (p[0] - c.center[0] as f32, p[1] - c.center[1] as f32);
            dx * dx + dy * dy < 0.25
        };
        let lane = w.lanes().iter().find(|l| l.a as usize == warren && l.b as usize == warren && l.path.iter().any(near)).expect("a route through the cave");
        for back in [false, true] {
            let walked = walk(&w, lane, back);
            assert!(walked.is_ok(), "through the cave at {:?}: {walked:?}", c.center);
        }
    }
    eprintln!("{} caves, {} doorways, {cells} cells cut; rooms at {:?}", rooms.len(), doors.len(), rooms.iter().map(|&r| k.chambers[r].center).collect::<Vec<_>>());
}

#[test]
fn rift_walls_have_galleries() {
    use super::under::SILL;
    use crate::movement::Mover;
    let k = build_kiln();
    let w = kiln();
    // Two in each wall of each rift (a crossing's on each side), four windows each.
    let galleries: Vec<_> = k.openings.iter().filter(|o| !o.door).collect();
    assert_eq!(galleries.len(), 12);
    for j in 0..3 {
        let rift = Rift::new(j);
        for side in [-1.0, 1.0] {
            let n = galleries.iter().filter(|g| {
                let (along, across) = rift.coords(k.chambers[g.room].center);
                along > 0.0 && across * side > 0.0 && across.abs() < Rift::half_width(along) + 120.0
            });
            assert_eq!(n.count(), 2, "rift {j}, side {side}");
        }
    }
    let owners: Vec<u8> = w.sites().iter().map(|s| s.sector).collect();
    let (mut cells, mut longest) = (0, 0.0f64);
    for g in &galleries {
        let c = k.chambers[g.room].center;
        let lat = [-g.a[1], g.a[0]];
        let at = |s: f64, l: f64| add(add(c, scale(g.a, s)), scale(lat, l));
        let zg = g.floor as f32;
        cells += sealed(&w, g, &format!("a window of the gallery at {c:?}"));
        for b in &g.bays {
            let mid = (b.span.0 + b.span.1) / 2.0;
            longest = longest.max(b.to);
            // From the sill, the rift's floor in sight, and the other way.
            let sill = at(b.to - 0.8, mid);
            let eye = [sill[0] as f32, sill[1] as f32, zg + 1.6];
            let out = at(b.to + 100.0, mid);
            let floor = [out[0] as f32, out[1] as f32, w.terrain(out[0] as f32, out[1] as f32) + 1.0];
            for (p, q) in [(eye, floor), (floor, eye)] {
                if !crate::hit::line_clear(&w, p, q, Mover { faction: 0, owners: &owners, live: crate::clock::Live::OFF }) {
                    let t = crate::hit::terrain(&w, p, q);
                    let mut boxes = Vec::new();
                    w.boxes_near((p[0] + q[0]) / 2.0, (p[1] + q[1]) / 2.0, 60.0, |bx| {
                        if let Some(f) = crate::hit::solid(p, q, bx) {
                            boxes.push((f, bx.min, bx.max, bx.bottom, bx.top));
                        }
                    });
                    panic!("the floor not in sight from the window at {sill:?} of the gallery at {c:?} (to {:.1}, cells {:?}): terrain {t:?}, boxes {boxes:?}", b.to, b.cells);
                }
            }
            // Walked to from the hall.
            let end = at(b.to - 1.2, mid);
            let door = at(10.0, mid);
            let l = crate::world::Lane { a: 0, b: 0, path: vec![[c[0] as f32, c[1] as f32, zg], [door[0] as f32, door[1] as f32, zg], [end[0] as f32, end[1] as f32, zg]], one_way: false, when: 0 };
            let walked = walk(&w, &l, false);
            assert!(walked.is_ok(), "the window at {sill:?} of the gallery at {c:?}: {walked:?}");
            // The sill stops a walk out of the window (it's no door).
            let past = at(b.to + 3.0, mid);
            let out_l = crate::world::Lane { a: 0, b: 0, path: vec![[c[0] as f32, c[1] as f32, zg], [door[0] as f32, door[1] as f32, zg], [past[0] as f32, past[1] as f32, zg]], one_way: false, when: 0 };
            assert!(walk(&w, &out_l, false).is_err(), "walked out of the window at {sill:?} over its {SILL} m sill");
        }
    }
    eprintln!("{} galleries, {} windows, {cells} cells cut, the longest bay {longest:.0} m from its hall's center", galleries.len(), galleries.iter().map(|g| g.bays.len()).sum::<usize>());
}

#[test]
fn tunnels_run_no_more_than_400_m_between_stops() {
    // A junction or an exit (a trench, a shaft up) every 400 m at the most.
    let k = build_kiln();
    let tubes: Vec<(usize, usize)> = k.bores.iter().map(|b| b.4).collect();
    let all = super::under::stretches(&k, &tubes);
    let longest = all.iter().map(|s| s.length()).fold(0.0, f64::max);
    for s in all.iter().filter(|s| s.length() > super::under::STRETCH) {
        eprintln!("-- {:.0} m", s.length());
        for (&i, d) in s.chambers.iter().zip(&s.along) {
            let c = &k.chambers[i];
            eprintln!("  {d:5.0} #{i} site {} ({:.0}, {:.0}) floor {:.0} surface {:.0} half {} trench {} ground {} pair {:?} outs {}", c.site, c.center[0], c.center[1], c.floor, k.height(c.center), c.half, c.trench, c.ground, c.pair, c.outs.len());
        }
    }
    for s in &all {
        let (a, b) = (&k.chambers[s.chambers[0]], &k.chambers[*s.chambers.last().unwrap()]);
        assert!(s.length() <= super::under::STRETCH, "{:.0} m between stops, ({:.0}, {:.0}, {:.0}) to ({:.0}, {:.0}, {:.0})", s.length(), a.center[0], a.center[1], a.floor, b.center[0], b.center[1], b.floor);
    }
    let exits = k.chambers.iter().filter(|c| c.trench || c.ground).count();
    eprintln!("{} stretches, the longest {longest:.0} m; {exits} exits", all.len());
}

#[test]
fn every_tunnel_fits() {
    // What the generator couldn't place (an undercroft, a shaft, a mouth).
    let k = build_kiln();
    for n in &k.notes {
        eprintln!("{n}");
    }
    assert!(k.notes.is_empty(), "{} things the generator couldn't fit", k.notes.len());
}

/// Falls are a choice (K4): a rider in the air in any lift's or drop
/// shaft's column, steering hard out of it any of eight ways, stays in it (to near a lift's
/// top, to a drop shaft's floor) or crosses into the column beside it, and
/// lands from a drop shaft unhurt.
#[test]
fn columns_hold_their_riders() {
    use crate::movement::{fall_damage, step, Input, MoveState, BUTTON_SPRINT, HELD_DROP, HELD_LIFT};
    use crate::world::{Force, ALWAYS, VENT};
    let w = kiln();
    let mut rides = 0;
    for v in w.volumes().iter().filter(|v| (v.when == ALWAYS || v.when == VENT) && v.max[2] - v.min[2] > 20.0) {
        let margin = match v.force {
            Force::Lift => HELD_LIFT,
            Force::Drop => HELD_DROP,
            _ => continue,
        };
        // (A tall lift is two columns, one on the other: ridden to the top of both.)
        let c = v.center();
        let mut top = v.max[2];
        while let Some(o) = w.volume_at(c[0], c[1], top, crate::clock::Live::OFF).filter(|o| o.force == Force::Lift && v.force == Force::Lift) {
            top = o.max[2];
        }
        for (mx, my) in [(127, 0), (90, 90), (0, 127), (-90, 90), (-127, 0), (-90, -90), (0, -127), (90, -90)] {
            let mut s = MoveState { pos: c, vel: [0.0; 2], z: (v.min[2] + v.max[2]) / 2.0, vz: 0.0, grounded: false, crouched: false };
            let input = Input { move_x: mx, move_y: my, buttons: BUTTON_SPRINT, ..Input::default() };
            for _ in 0..30 * 300 {
                let n = step(&w, s, input, 0u8);
                // (Over its footprint, a turned one's too.)
                let inside = v.contains(n.pos[0], n.pos[1], v.min[2]);
                if v.force == Force::Lift && n.z >= top - margin {
                    break;
                }
                if !inside && w.volume_at(n.pos[0], n.pos[1], n.z, crate::clock::Live::OFF).is_some_and(|o| o.force == Force::Lift || o.force == Force::Drop) {
                    break; // into the column beside it (a shaft's other half)
                }
                assert!(inside || n.z < v.min[2] + 3.0, "{:?} at ({:.0}, {:.0}, {:.0}): steered out at {:.1}", v.force, c[0], c[1], v.min[2], n.z);
                if n.grounded {
                    assert_eq!(fall_damage(&s, &n), 0, "landed hurt from a drop shaft at ({:.0}, {:.0})", c[0], c[1]);
                    break;
                }
                s = n;
            }
            rides += 1;
        }
    }
    eprintln!("{rides} rides held");
    assert!(rides > 100);
}

#[test]
fn the_clockwork_runs() {
    // K4: living bridges at night, sandfalls in turns, vent lifts that rest.
    use crate::clock::{self, Live};
    use crate::movement::Mover;
    use crate::world::{Kind, ALWAYS, BRIDGE, FALLS_A, FALLS_B, VENT};
    let w = kiln();
    let walk_at = |l: &crate::world::Lane, backward: bool, live: Live| crate::reach::walk_as(&w, l, backward, Mover { faction: crate::reach::walker(&w, l), owners: &[], live });
    let timed: Vec<&crate::world::Lane> = w.lanes().iter().filter(|l| l.when != ALWAYS).collect();
    let count = |when: u8| timed.iter().filter(|l| l.when == when).count();
    assert_eq!((count(BRIDGE), count(FALLS_A), count(FALLS_B)), (3, 2, 2), "a bridge over each rift, two sandfalls into each cenote");
    for l in &timed {
        let (a, b) = (l.path[0], l.path[l.path.len() - 1]);
        eprintln!("when {}: ({:.0}, {:.0}, {:.0}) to ({:.0}, {:.0}, {:.0}), {:.0} m", l.when, a[0], a[1], a[2], b[0], b[1], b[2], l.length());
    }
    for l in &timed {
        let (open, shut) = clock::open(l.when);
        for backward in [false, true].into_iter().filter(|&b| !b || !l.one_way) {
            assert!(walk_at(l, backward, open).is_ok(), "{} walks while it's there", l.when);
            let fails = walk_at(l, backward, shut);
            assert!(fails.as_ref().is_err_and(|e| e.contains("fell")), "{}: walked while it isn't there: {fails:?}", l.when);
        }
    }
    // A bridge's deck stops shots at night only, and is never there by day.
    let decks: Vec<_> = w.boxes().iter().filter(|b| b.kind == Kind::Coral).collect();
    assert!(decks.iter().all(|b| b.when == BRIDGE));
    let b = decks.iter().max_by(|a, b| a.half[1].total_cmp(&b.half[1])).unwrap();
    let (over, under) = ([b.center[0], b.center[1], b.top + 3.0], [b.center[0], b.center[1], b.bottom - 3.0]);
    let (night, day) = clock::open(BRIDGE);
    assert!(!crate::hit::line_clear(&w, over, under, Mover { faction: 0, owners: &[], live: night }));
    assert!(crate::hit::line_clear(&w, over, under, Mover { faction: 0, owners: &[], live: day }));
    // The vent lifts: every route up one walks, however its vent's cycle
    // falls (arriving as it starts to rest, or midway), and no one riding
    // it falls hard when it rests.
    let vents: Vec<_> = w.volumes().iter().filter(|v| v.when == VENT).collect();
    let rides: Vec<&crate::world::Lane> = w.lanes().iter().filter(|l| l.path.iter().any(|p| vents.iter().any(|v| v.contains(p[0], p[1], p[2])))).collect();
    eprintln!("{} vent lifts' columns, {} routes up them", vents.len(), rides.len());
    assert!(vents.len() >= 9 && rides.len() >= 9);
    for l in &rides {
        for s0 in (0..clock::VENT_CYCLE).step_by(clock::VENT_CYCLE as usize / 6) {
            if let Err(e) = walk_at(l, false, Live::at(s0)) {
                panic!("route up a vent from step {s0}: {e}");
            }
        }
    }
}

#[test]
fn tunnels_hold_whoever_walks_in_them() {
    // Under the ground only boxes are ground: a gap in a tunnel's walls or
    // floor drops whoever finds it to the bottom of the world (bots found
    // them at the ends of diagonal tubes, where a tube meets its chamber).
    // Round each end of every tube, on a half-meter grid at its floor: no
    // floor beside floor without a wall between (but over a shaft's column).
    use crate::clock::Live;
    let k = build_kiln();
    let w = kiln();
    let floor_at = |p: [f32; 2], f: f32| w.ground(p[0], p[1], f + 0.5, 1.0, Live::OFF) >= f - 0.5;
    let mut leaks = Vec::new();
    for &(from, to, _, _, (a, b)) in &k.bores {
        for (end, c) in [(from, a), (to, b)] {
            let f = k.chambers[c].floor as f32;
            for i in -14..=14 {
                for j in -14..=14 {
                    let p = [end[0] as f32 + i as f32 * 0.5, end[1] as f32 + j as f32 * 0.5];
                    if !floor_at(p, f) || !w.under(p[0], p[1], f) {
                        continue;
                    }
                    for (dx, dy) in [(0.5f32, 0.0f32), (-0.5, 0.0), (0.0, 0.5), (0.0, -0.5)] {
                        let q = [p[0] + dx, p[1] + dy];
                        if floor_at(q, f) || w.volume_at(q[0], q[1], f - 1.0, Live::OFF).is_some() || w.volume_at(q[0], q[1], f + 0.5, Live::OFF).is_some() {
                            continue;
                        }
                        // Walk from the floor toward it (and a little either side),
                        // as movement would: does it end in a fall that hurts?
                        // (On the floor there: a tube's may slope up from the chamber's.)
                        let z = w.ground(p[0], p[1], f + 1.5, 2.0, Live::OFF);
                        let start = crate::movement::MoveState { pos: p, vel: [0.0; 2], z, vz: 0.0, grounded: true, crouched: false };
                        let start = crate::movement::nudge(&w, start, [0.0, 0.0], 0u8);
                        // (Not from inside something: no one stands there.)
                        if (start.pos[0] - p[0]).abs() + (start.pos[1] - p[1]).abs() > 0.05 {
                            continue;
                        }
                        let toward = libm::atan2f(q[1] - p[1], q[0] - p[0]);
                        for turn in [0.0f32, 0.35, -0.35] {
                            let a = toward + turn;
                            let (mx, my) = ((libm::cosf(a) * 127.0) as i8, (libm::sinf(a) * 127.0) as i8);
                            let mut m = start;
                            let mut worst = 0.0f32;
                            for i in 0..120 {
                                let input = if i < 15 { crate::movement::Input { move_x: mx, move_y: my, ..Default::default() } } else { crate::movement::Input::default() };
                                let before = m;
                                m = crate::movement::step(&w, m, input, 0u8);
                                worst = worst.max(crate::movement::fall_height(&before, &m));
                            }
                            if worst > crate::movement::SAFE_FALL || m.z < 1.0 {
                                leaks.push(format!("from ({:.1}, {:.1}, {f:.1}) toward ({:.1}, {:.1}): fell {worst:.1} m to ({:.1}, {:.1}, {:.1})", p[0], p[1], q[0], q[1], m.pos[0], m.pos[1], m.z));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    leaks.sort();
    leaks.dedup();
    for l in leaks.iter().take(if std::env::var_os("KILN_LEAK_ALL").is_some() { usize::MAX } else { 20 }) {
        eprintln!("{l}");
    }
    assert!(leaks.is_empty(), "{} gaps out of the tunnels' ends", leaks.len());
}
