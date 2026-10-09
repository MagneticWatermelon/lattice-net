//! Hashes what the shared rules compute over a long seeded run, so two
//! platforms can be compared bit for bit: `scripts/determinism.sh` runs this
//! on Linux (the server's) and on Windows (the client's) and compares.
//!
//!   cargo run --release -p lattice-game --example determinism [seed]
//!
//! - **world:** the terrain and every cover box the client builds itself;
//! - **movement:** players walking, sprinting, jumping and aiming down
//!   sights over the map for thousands of steps, pushed now and then and
//!   dead for a while: the prediction the client and server must agree on;
//! - **weapon:** aim, spread, bloom, flights and their hits;
//! - **codecs:** what both sides quantize and decode (near states, blobs,
//!   distant fights, directions, render steps).

use lattice_game::activity;
use lattice_game::delta::{self, NearEntry, NearHistory, NearQ};
use lattice_game::events;
use lattice_game::hit;
use lattice_game::movement::{dead_input, nudge, step, Input, MoveState, BUTTON_ADS, BUTTON_JUMP, BUTTON_SPRINT};
use lattice_game::msg;
use lattice_game::weapon::{aim, angles, muzzle, spread, Bloom, Flight};
use lattice_game::world::World;
use lattice_net::wire::Writer;

/// FNV-1a over everything fed to it.
struct Hash(u64);

impl Hash {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn f32(&mut self, v: f32) {
        self.bytes(&v.to_bits().to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.bytes(&v.to_bits().to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn state(&mut self, s: &MoveState) {
        for v in [s.pos[0], s.pos[1], s.vel[0], s.vel[1], s.z, s.vz] {
            self.f32(v);
        }
        self.u64(s.grounded as u64);
    }
}

/// xorshift64*: integer only, so it's the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// In [0, 1), from 24 bits: exact in f32.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn main() {
    let seed: u64 = std::env::args().nth(1).map_or(1, |s| s.parse().expect("seed: a number"));
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let world = World::shared(1);

    let mut h = Hash::new();
    for y in (0..8192).step_by(16) {
        for x in (0..8192).step_by(16) {
            h.f32(world.terrain(x as f32 + 0.37, y as f32 + 0.61));
        }
    }
    for b in world.boxes() {
        for v in [b.min[0], b.min[1], b.max[0], b.max[1], b.bottom, b.top] {
            h.f32(v);
        }
        h.u64(b.kind as u64 * 4 + b.facing as u64);
    }
    println!("world {:016x}", h.0);

    // Players start at random spots and at every site (bases and outposts:
    // walls, gates, slopes), and wander with random inputs.
    let mut h = Hash::new();
    let mut starts: Vec<[f32; 2]> = world.sites().iter().map(|s| s.center).collect();
    while starts.len() < 64 {
        starts.push([200.0 + rng.unit() * 7800.0, 200.0 + rng.unit() * 7800.0]);
    }
    for start in starts.into_iter().take(64) {
        let mut s = MoveState::standing(&world, start);
        let mut input = Input::default();
        let mut dead = 0;
        for k in 0..3000 {
            if k % 20 == 0 {
                let buttons = [0, BUTTON_SPRINT, BUTTON_JUMP, BUTTON_ADS, BUTTON_SPRINT | BUTTON_JUMP][rng.below(5) as usize];
                let (mx, my) = (rng.below(255) as i32 - 127, rng.below(255) as i32 - 127);
                input = Input { move_x: mx as i8, move_y: my as i8, yaw: rng.next() as u16, pitch: (rng.below(65535) as i32 - 32767) as i16, buttons };
            }
            if k % 500 == 499 {
                dead = 150; // dies; the body settles, then it's back
            }
            s = if dead > 0 {
                dead -= 1;
                step(&world, s, dead_input(input))
            } else {
                step(&world, s, input)
            };
            if k % 97 == 0 {
                s = nudge(&world, s, [rng.unit() - 0.5, rng.unit() - 0.5]);
            }
            h.state(&s);
        }
    }
    println!("movement {:016x}", h.0);

    // Shots from those spots: aim, spread (secret picks are just numbers
    // here), bloom over bursts, flights against the terrain, cover and a
    // player standing 30 m off.
    let mut h = Hash::new();
    let mut bloom = Bloom::default();
    for k in 0..4000u64 {
        let (yaw, pitch) = (rng.next() as u16, (rng.below(20000) as i32 - 10000) as i16);
        let a = aim(yaw, pitch);
        let (y2, p2) = angles(a);
        h.u64(y2 as u64);
        h.u64(p2 as u16 as u64);
        let at = [300.0 + rng.unit() * 7400.0, 300.0 + rng.unit() * 7400.0];
        let s = MoveState::standing(&world, at);
        let cone = bloom.fire(k * 768, k % 3 == 0, &s);
        h.f32(cone);
        let dir = spread(yaw, pitch, cone, rng.next());
        let origin = muzzle(&s, &s, (k % 256) as u8);
        let mut f = Flight::new(origin, dir);
        let target = [origin[0] + a[0] * 30.0, origin[1] + a[1] * 30.0, s.z];
        for _ in 0..40 {
            let next = f.advance();
            if let Some(t) = hit::terrain(&world, f.pos, next.pos) {
                h.f32(t);
                break;
            }
            if let Some((t, head)) = hit::player(f.pos, next.pos, target) {
                h.f32(t);
                h.u64(head as u64);
                break;
            }
            let mid = [(f.pos[0] + next.pos[0]) / 2.0, (f.pos[1] + next.pos[1]) / 2.0];
            let mut cover = None;
            world.boxes_near(mid[0], mid[1], 6.0, |b| {
                if let Some(t) = hit::aabb(f.pos, next.pos, [b.min[0], b.min[1], b.bottom], [b.max[0], b.max[1], b.top]) {
                    cover = Some(cover.map_or(t, |c: f32| c.min(t)));
                }
            });
            if let Some(t) = cover {
                h.f32(t);
                break;
            }
            f = next;
        }
        for v in [f.pos[0], f.pos[1], f.pos[2], f.vel[0], f.vel[1], f.vel[2]] {
            h.f32(v);
        }
        h.u64(hit::line_clear(&world, origin, target) as u64);
    }
    println!("weapon {:016x}", h.0);

    // What both sides quantize and decode.
    let mut h = Hash::new();
    let mut hist = NearHistory::default();
    for tick in 0..400u32 {
        let mut entries: Vec<NearEntry> = (0..40u16)
            .map(|e| {
                let at = [rng.unit() * 8190.0, rng.unit() * 8190.0];
                let mut s = MoveState::standing(&world, at);
                s.vel = [rng.unit() * 12.0 - 6.0, rng.unit() * 12.0 - 6.0];
                let q = NearQ::new(&s, rng.next() as u16, (rng.below(60000) as i32 - 30000) as i16, rng.below(101) as u8, rng.below(2) == 0);
                let base = (tick > 0 && e % 2 == 0).then(|| hist.get(e, tick - 1).map(|b| (1u8, b))).flatten();
                NearEntry { entity: e, state: q, base }
            })
            .collect();
        let mut w = Writer::with_capacity(4096);
        delta::encode_near(tick, &mut entries, &mut w);
        let data = w.into_inner();
        h.bytes(&data);
        delta::decode_near(&data, &mut hist, |e, q| {
            h.u64(e as u64);
            for v in [q.pos()[0], q.pos()[1], q.vel()[0], q.vel()[1], q.z(), q.yaw()] {
                h.f32(v);
            }
        })
        .expect("decodes");
        for e in 0..10u16 {
            let s = MoveState::standing(&world, [rng.unit() * 8190.0, rng.unit() * 8190.0]);
            let blob = msg::encode_blob(e, &s, rng.next() as u16, (rng.below(60000) as i32 - 30000) as i16, rng.below(101) as u8);
            let b = msg::decode_blob(&blob).expect("decodes");
            for v in [b.pos[0], b.pos[1], b.z, b.yaw, b.pitch] {
                h.f32(v);
            }
        }
        let shots = rng.below(5000) as u32;
        h.u64(activity::shots_from_code(activity::shots_code(shots)) as u64);
        let at = [rng.unit() * 8190.0, rng.unit() * 8190.0];
        let entry = activity::encode_entry(activity::cell_of(at), (tick % 3) as u8, shots, rng.unit() * std::f32::consts::TAU, at);
        let c = activity::decode_entry(&entry);
        for v in [c.yaw, c.at[0], c.at[1]] {
            h.f32(v);
        }
        h.u64(events::direction(at, [rng.unit() * 8190.0, rng.unit() * 8190.0]) as u64);
        let r = tick as f64 * 1.5 + rng.unit() as f64;
        h.f64(msg::render_age(tick + 7, msg::render_units(r)));
    }
    println!("codecs {:016x}", h.0);
}
