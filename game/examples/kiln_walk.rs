//! Walks one of Kiln's lanes with the game's movement, printing where it is
//! each second: for finding where a route snags.
//!
//!   cargo run --release -p lattice-game --example kiln_walk -- LANE [back|forth] [STEPS between prints]
use lattice_game::movement::{step, Input, MoveState, BUTTON_SPRINT, DT};
use lattice_game::world::{World, KILN};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let w = World::shared(KILN);
    let i: usize = args[0].parse().unwrap();
    let back = args.get(1).is_some_and(|a| a == "back");
    let l = &w.lanes()[i];
    let (a, b) = (w.sites()[l.a as usize], w.sites()[l.b as usize]);
    println!("lane {i}: {:?} {} -> {:?} {}, {:.0} m, {} waypoints", a.kind, l.a, b.kind, l.b, l.length(), l.path.len());
    let s0 = l.waypoint(0, back);
    let mut s = if s0[2] < w.terrain(s0[0], s0[1]) - 2.0 {
        MoveState::standing_at(&w, [s0[0], s0[1]], s0[2])
    } else {
        let s = MoveState::standing(&w, [s0[0], s0[1]]);
        MoveState { z: w.ground(s0[0], s0[1], s0[2].max(s.z) + 1.0, 1.5, lattice_game::clock::Live::OFF).max(s.z), ..s }
    };
    let mut next = 1;
    let who = lattice_game::reach::walker(&w, l);
    let every = args.get(2).and_then(|a| a.parse::<u32>().ok()).unwrap_or(30);
    for t in 0..(30 * 600) {
        let Some((mx, my)) = l.steer([s.pos[0], s.pos[1], s.z], &mut next, back) else {
            println!("done in {:.1} s at ({:.0}, {:.0}, {:.1})", t as f32 * DT, s.pos[0], s.pos[1], s.z);
            return;
        };
        if t % every == 0 {
            let wp = l.waypoint(next.min(l.path.len() - 1), back);
            let v = (s.vel[0] * s.vel[0] + s.vel[1] * s.vel[1]).sqrt();
            println!("{:5.1} s  ({:7.1}, {:7.1}, {:6.1}) v {v:4.1}  -> wp {next} ({:.0}, {:.0}, {:.1})", t as f32 * DT, s.pos[0], s.pos[1], s.z, wp[0], wp[1], wp[2]);
        }
        s = step(&w, s, Input { move_x: mx, move_y: my, yaw: 0, pitch: 0, buttons: BUTTON_SPRINT, tools: 0 }, who);
    }
}
