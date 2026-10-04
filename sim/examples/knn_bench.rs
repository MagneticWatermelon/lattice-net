//! The near tier's k-nearest search, old against new, on the crowds that
//! matter: 10k players in a 25 m pile, a 200 m disk, and uniform.
//!
//!   cargo run --release -p lattice-sim --example knn_bench
//!
//! "old" is assembly's search before `Grid::knn`: a ring walk that pushes every
//! in-range candidate as an (f32, f32, u16) tuple read through the body array,
//! with a partial sort at each finished ring and one after. "new" is
//! `Grid::knn`. Single-threaded: ratios carry over, absolute times don't.

use std::time::Instant;

use lattice_sim::grid::{Grid, Knn, Ring};
use lattice_sim::movement::WORLD_SIZE;

fn rng(seed: u64) -> impl FnMut() -> f32 {
    let mut x = seed;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn disk(n: usize, radius: f32, seed: u64) -> Vec<[f32; 2]> {
    let mut next = rng(seed);
    let c = WORLD_SIZE / 2.0;
    (0..n)
        .map(|_| {
            let (a, d) = (next() * std::f32::consts::TAU, radius * next().sqrt());
            [c + d * a.cos(), c + d * a.sin()]
        })
        .collect()
}

/// The search as assembly did it before `Grid::knn`.
fn old(g: &Grid, pts: &[[f32; 2]], me: usize, r: f32, k: usize, raw: &mut Vec<(f32, f32, u16)>) -> usize {
    raw.clear();
    let q = pts[me];
    let mut scanned = 0;
    g.walk_rings(q, r, |v| match v {
        Ring::Item(j) => {
            scanned += 1;
            if j as usize != me {
                let p = pts[j as usize];
                let d2 = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2);
                if d2 <= r * r {
                    raw.push((d2, d2, j as u16));
                }
            }
            false
        }
        Ring::Done(bound) if raw.len() >= k => {
            raw.select_nth_unstable_by(k - 1, |a, b| a.0.total_cmp(&b.0));
            raw[k - 1].0 <= bound * bound
        }
        Ring::Done(_) => false,
    });
    if raw.len() > k {
        raw.select_nth_unstable_by(k, |a, b| a.0.total_cmp(&b.0));
        raw.truncate(k);
    }
    scanned
}

fn main() {
    let n = 10_000;
    let mut next = rng(7);
    let uniform: Vec<[f32; 2]> = (0..n).map(|_| [next() * WORLD_SIZE, next() * WORLD_SIZE]).collect();
    println!("{:<10} {:>14} {:>14} {:>9} {:>14} {:>14}", "crowd", "old ns/query", "new ns/query", "speedup", "old scanned", "new scanned");
    for (name, pts) in [("pile 25m", disk(n, 25.0, 1)), ("disk 200m", disk(n, 200.0, 2)), ("uniform", uniform)] {
        let mut g = Grid::new(32.0);
        let t = Instant::now();
        g.rebuild(pts.iter().enumerate().map(|(i, &p)| (i as u32, p)));
        let build = t.elapsed();
        let (r, k) = (150.0, 100);

        let mut raw = Vec::new();
        let (mut old_scanned, t) = (0, Instant::now());
        for me in 0..n {
            old_scanned += old(&g, &pts, me, r, k, &mut raw);
        }
        let old_ns = t.elapsed().as_nanos() as f64 / n as f64;

        let mut out = Knn::default();
        let (mut new_scanned, t) = (0, Instant::now());
        for (me, &q) in pts.iter().enumerate() {
            g.knn(q, r, k, |j| j as usize != me, &mut out);
            new_scanned += out.scanned as usize;
            std::hint::black_box(out.keys());
        }
        let new_ns = t.elapsed().as_nanos() as f64 / n as f64;
        println!(
            "{name:<10} {old_ns:>14.0} {new_ns:>14.0} {:>8.1}x {:>14} {:>14}   (grid rebuild {build:?})",
            old_ns / new_ns,
            old_scanned / n,
            new_scanned / n
        );
    }
}
