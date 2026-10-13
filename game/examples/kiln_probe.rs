//! Looks at a spot of Kiln, for finding what snags a route (with
//! kiln_walk, which walks one):
//!
//!   cargo run --release -p lattice-game --example kiln_probe -- line X0 Y0 X1 Y1 N   heights along a line
//!   ... -- grid X Y [R]     the height samples around a point, north up
//!   ... -- cliffs X Y       which cells around it are cliffs (C) or steeper than walkable (s)
//!   ... -- boxes X Y [R]    the boxes near it
//!   ... -- rift J ALONG A0 A1 N   heights across rift J at ALONG, from across A0 to A1
//!   ... -- sites            every site: index, kind, sector, center, level
//!   ... -- volumes          every force volume, and the shields
//!   ... -- lanes A B         the lanes between sites A and B, every 10th waypoint
use lattice_game::world::{World, KILN};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: Vec<f32> = args.iter().skip(1).map(|s| s.parse().expect("a number")).collect();
    let w = World::shared(KILN);
    match args.first().map(String::as_str) {
        Some("line") => {
            for i in 0..=n[4] as usize {
                let t = i as f32 / n[4];
                let (x, y) = (n[0] + (n[2] - n[0]) * t, n[1] + (n[3] - n[1]) * t);
                let ground = w.ground_at((x / 4.0).round() as usize, (y / 4.0).round() as usize);
                println!("{x:7.1} {y:7.1}  {:7.2}  ground {ground:?}  cliff {}", w.terrain(x, y), w.cliff(x, y));
            }
        }
        Some("grid") => {
            let (cx, cy, r) = ((n[0] / 4.0).round() as usize, (n[1] / 4.0).round() as usize, n.get(2).copied().unwrap_or(5.0) as usize);
            for y in (cy - r..=cy + r).rev() {
                let line: Vec<String> = (cx - r..=cx + r).map(|x| format!("{:6.1}", w.sample(x, y))).collect();
                println!("{:5} {}", y * 4, line.join(""));
            }
        }
        Some("cliffs") => {
            for j in (-5..=5).rev() {
                let line: String = (-8..=8)
                    .map(|i| {
                        let (x, y) = (n[0] + i as f32 * 2.0, n[1] + j as f32 * 2.0);
                        let g = w.gradient(x, y);
                        if i == 0 && j == 0 { '@' } else if w.cliff(x, y) { 'C' } else if g[0] * g[0] + g[1] * g[1] > 1.0 { 's' } else { '.' }
                    })
                    .collect();
                println!("{line}");
            }
        }
        Some("boxes") => {
            println!("terrain {:.2}", w.terrain(n[0], n[1]));
            let mut seen = Vec::new();
            w.boxes_near(n[0], n[1], n.get(2).copied().unwrap_or(8.0), |b| if !seen.contains(b) { seen.push(*b) });
            for b in seen {
                println!("{:?} x {:.1}..{:.1} y {:.1}..{:.1} z {:.2}..{:.2}", b.kind, b.min[0], b.max[0], b.min[1], b.max[1], b.bottom, b.top);
            }
        }
        Some("rift") => {
            let r = lattice_game::kiln::Rift::new(n[0] as usize);
            for i in 0..=n[4] as usize {
                let across = n[2] + (n[3] - n[2]) * i as f32 / n[4];
                let p = r.at(n[1] as f64, across as f64);
                let (x, y) = (p[0] as f32, p[1] as f32);
                let ground = w.ground_at((x / 4.0).round() as usize, (y / 4.0).round() as usize);
                println!("across {across:7.1} ({x:7.1}, {y:7.1})  {:7.2}  ground {ground:?}  cliff {}", w.terrain(x, y), w.cliff(x, y));
            }
        }
        Some("sites") => {
            for (i, s) in w.sites().iter().enumerate() {
                println!("{i:3} {:?} sector {} at ({:.0}, {:.0}) level {:.1}", s.kind, s.sector, s.center[0], s.center[1], s.level);
            }
        }
        Some("lanes") => {
            for (i, l) in w.lanes().iter().enumerate().filter(|(_, l)| (l.a as f32, l.b as f32) == (n[0], n[1])) {
                println!("lane {i}: {} waypoints, one way {}", l.path.len(), l.one_way);
                for p in l.path.iter().step_by(10) {
                    println!("  ({:.1}, {:.1}, {:.1})  terrain {:.1}", p[0], p[1], p[2], w.terrain(p[0], p[1]));
                }
            }
        }
        Some("volumes") => {
            for v in w.volumes() {
                println!("{:?} x {:.1}..{:.1} y {:.1}..{:.1} z {:.1}..{:.1} vel {:?}", v.force, v.min[0], v.max[0], v.min[1], v.max[1], v.min[2], v.max[2], v.vel);
            }
            for b in w.boxes().iter().filter(|b| b.pass != lattice_game::world::SOLID) {
                println!("shield for {} at ({:.1}, {:.1}) z {:.1}..{:.1}", b.pass, b.center[0], b.center[1], b.bottom, b.top);
            }
        }
        _ => eprintln!("kiln_probe line X0 Y0 X1 Y1 N | grid X Y [R] | cliffs X Y | boxes X Y [R] | rift J ALONG A0 A1 N"),
    }
}
