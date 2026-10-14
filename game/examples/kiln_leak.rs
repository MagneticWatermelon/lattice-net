//! Walks out from a point under the ground in 16 directions, printing any walk
//! that falls: for finding gaps in tunnels.
//!
//!   cargo run --release -p lattice-game --example kiln_leak -- X Y Z [SECS]
use lattice_game::movement::{fall_height, step, Input, MoveState};
use lattice_game::world::{World, KILN};

fn main() {
    let a: Vec<f32> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let w = World::shared(KILN);
    let secs = a.get(3).copied().unwrap_or(1.5);
    let s0 = MoveState::standing_at(&w, [a[0], a[1]], a[2]);
    println!("start ({:.1}, {:.1}, {:.2})", s0.pos[0], s0.pos[1], s0.z);
    for d in 0..16 {
        let ang = d as f32 * std::f32::consts::PI / 8.0;
        let mut m = s0;
        let mut worst = 0.0f32;
        for i in 0..(secs * 30.0) as usize + 90 {
            let input = if (i as f32) < secs * 30.0 { Input { move_x: (libm::cosf(ang) * 127.0) as i8, move_y: (libm::sinf(ang) * 127.0) as i8, ..Default::default() } } else { Input::default() };
            let b = m;
            m = step(&w, m, input, 0u8);
            worst = worst.max(fall_height(&b, &m));
        }
        println!("{:5.1} deg -> ({:.1}, {:.1}, {:.2}) fell {:.1}", d as f32 * 22.5, m.pos[0], m.pos[1], m.z, worst);
    }
}
