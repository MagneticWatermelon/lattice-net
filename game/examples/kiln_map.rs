//! Draws a world from above, for looking at: the ground by what it's made
//! of, lit from the north-west, with its boxes, lanes, sites and control
//! points. Kiln unless given another world id.
//!
//!   cargo run --release -p lattice-game --example kiln_map -- OUT.png [PX] [X0 Y0 X1 Y1] [--id ID]
//!
//! PX is the image's side in pixels (2048); the box is the part of the map
//! to draw, in meters (all of it).

use lattice_game::world::{Ground, World, KILN, TERRAIN_N};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let id = args.iter().position(|a| a == "--id").map_or(KILN, |i| args[i + 1].parse().expect("--id: a number"));
    let pos: Vec<&String> = args.iter().enumerate().filter(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--id")).map(|(_, a)| a).collect();
    let out = pos.first().map(|s| s.as_str()).unwrap_or("kiln.png");
    let px: usize = pos.get(1).map_or(2048, |s| s.parse().expect("PX"));
    let view: [f32; 4] = if pos.len() >= 6 { [0, 1, 2, 3].map(|i| pos[2 + i].parse().expect("X0 Y0 X1 Y1")) } else { [0.0, 0.0, 8192.0, 8192.0] };
    let t0 = std::time::Instant::now();
    let w = World::shared(id);
    eprintln!("built in {:.2} s: {} boxes, {} sites, {} lanes and routes, {} points", t0.elapsed().as_secs_f32(), w.boxes().len(), w.sites().len(), w.lanes().len(), w.points().len());

    let (wx, wy) = (view[2] - view[0], view[3] - view[1]);
    let (iw, ih) = (px, ((px as f32) * wy / wx).round() as usize);
    let m = wx / iw as f32;
    let mut img = vec![[0u8; 3]; iw * ih];
    let to_px = |x: f32, y: f32| -> (i64, i64) { (((x - view[0]) / m) as i64, (ih as f32 - 1.0 - (y - view[1]) / m) as i64) };
    for py in 0..ih {
        for pxi in 0..iw {
            let (x, y) = (view[0] + (pxi as f32 + 0.5) * m, view[1] + (ih as f32 - 1.0 - py as f32 + 0.5) * m);
            let h = w.terrain(x, y);
            let e = m.max(2.0);
            let (dx, dy) = (w.terrain(x + e, y) - w.terrain(x - e, y), w.terrain(x, y + e) - w.terrain(x, y - e));
            // Light from the north-west, above.
            let n = [-dx / (2.0 * e), -dy / (2.0 * e), 1.0];
            let l = [-0.5f32, 0.5, 0.7];
            let nl = (n[0] * l[0] + n[1] * l[1] + n[2] * l[2]) / ((n[0] * n[0] + n[1] * n[1] + 1.0).sqrt() * (0.5f32 * 0.5 + 0.5 * 0.5 + 0.49).sqrt());
            let shade = (0.35 + 0.75 * nl.max(0.0)).min(1.25);
            let (ix, iy) = (((x / 4.0).round() as usize).min(TERRAIN_N - 1), ((y / 4.0).round() as usize).min(TERRAIN_N - 1));
            let base = match w.ground_at(ix, iy) {
                Some(g) if g == Ground::Sand as u8 => [214, 178, 122],
                Some(g) if g == Ground::Glass as u8 => [96, 128, 124],
                Some(g) if g == Ground::Salt as u8 => [232, 228, 214],
                Some(g) if g == Ground::Ochre as u8 => {
                    // Banded: stripes by height.
                    let band = ((h / 7.0) as i32 % 3) as i32;
                    [[176, 92, 54], [196, 120, 66], [160, 80, 50]][band as usize]
                }
                Some(g) if g == Ground::Basalt as u8 => [70, 66, 66],
                Some(g) if g == Ground::Pale as u8 => [210, 200, 186],
                Some(g) if g == Ground::Road as u8 => [120, 104, 84],
                Some(g) if g == Ground::Pad as u8 => [150, 140, 128],
                Some(g) if g == Ground::Scree as u8 => [150, 120, 96],
                _ => {
                    let t = (h / 400.0).clamp(0.0, 1.0);
                    [(90.0 + 120.0 * t) as u8, (120.0 + 80.0 * t) as u8, (70.0 + 100.0 * t) as u8]
                }
            };
            img[py * iw + pxi] = base.map(|c| (c as f32 * shade).min(255.0) as u8);
        }
    }
    let mut put = |x: i64, y: i64, c: [u8; 3]| {
        if x >= 0 && y >= 0 && (x as usize) < iw && (y as usize) < ih {
            img[y as usize * iw + x as usize] = c;
        }
    };
    // Boxes: filled, darker the taller.
    eprintln!("{} sunsails' stalks", w.boxes().iter().filter(|b| b.kind as u8 == 13).count());
    if std::env::var_os("KILN_SAILS").is_some() {
        for b in w.boxes().iter().filter(|b| b.kind as u8 == 13).step_by(23) {
            eprintln!("sunsail at ({:.0}, {:.0}) from {:.0} to {:.0}", b.center[0], b.center[1], b.bottom, b.top);
        }
    }
    for b in w.boxes() {
        // A sunsail: its membrane's span (half the stalk's height across).
        let r = if b.kind as u8 == 13 { (b.top - b.bottom) * 0.5 } else { 0.0 };
        let (x0, y1) = to_px(b.min[0] - r, b.min[1] - r);
        let (x1, y0) = to_px(b.max[0] + r, b.max[1] + r);
        let tall = ((b.top - b.bottom) / 60.0).clamp(0.0, 1.0);
        let c = match b.kind as u8 {
            7 => [(70.0 - 40.0 * tall) as u8, (90.0 - 40.0 * tall) as u8, (130.0 - 40.0 * tall) as u8],
            8 => [110, 64, 40],
            9 => [80, 220, 210],
            10 => [245, 240, 225],
            // K4: a living bridge's coral, a sunsail's stalk (drawn as its membrane's span).
            12 => [235, 110, 130],
            13 => [90, 200, 70],
            _ => [40, 40, 40],
        };
        for y in y0.min(y1)..=y0.max(y1) {
            for x in x0.min(x1)..=x0.max(x1) {
                put(x, y, c);
            }
        }
    }
    // Lanes: the lattice's veins (internal routes thinner, lighter).
    // (Tunnels, K3: orange, thick, over the rest.)
    for pass in [false, true] {
    for l in w.lanes() {
        for s in l.path.windows(2) {
            let under = w.under(s[0][0], s[0][1], s[0][2]) && w.under(s[1][0], s[1][1], s[1][2]);
            if under != pass {
                continue;
            }
            let c = if under {
                [255, 120, 30]
            } else if l.a == l.b {
                [255, 255, 160]
            } else {
                [40, 200, 255]
            };
            let (a, b) = (to_px(s[0][0], s[0][1]), to_px(s[1][0], s[1][1]));
            let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs())).max(1);
            for i in 0..=n {
                let (x, y) = (a.0 + (b.0 - a.0) * i / n, a.1 + (b.1 - a.1) * i / n);
                put(x, y, c);
                if (l.a != l.b && m > 1.5) || under {
                    put(x + 1, y, c);
                }
                if under {
                    put(x, y + 1, c);
                    put(x + 1, y + 1, c);
                }
            }
        }
    }
    }
    // Sites: a ring; control points: dots.
    for s in w.sites() {
        let r = (s.half / m).max(3.0);
        for i in 0..360 {
            let a = i as f32 * std::f32::consts::TAU / 360.0;
            let (x, y) = to_px(s.center[0] + libm::cosf(a) * r * m, s.center[1] + libm::sinf(a) * r * m);
            put(x, y, [255, 60, 200]);
        }
    }
    for p in w.points() {
        let (x, y) = to_px(p.pos[0], p.pos[1]);
        for dy in -2..=2 {
            for dx in -2..=2 {
                put(x + dx, y + dy, [255, 140, 0]);
            }
        }
    }
    write_png(out, iw, ih, &img);
    eprintln!("wrote {out} ({iw} x {ih}, {m:.2} m a pixel)");
}

/// A PNG without compression (stored deflate blocks): enough to look at.
fn write_png(path: &str, w: usize, h: usize, rgb: &[[u8; 3]]) {
    let mut raw = Vec::with_capacity(h * (1 + 3 * w));
    for y in 0..h {
        raw.push(0);
        for p in &rgb[y * w..(y + 1) * w] {
            raw.extend_from_slice(p);
        }
    }
    let mut z = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        let n = chunk.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = Vec::with_capacity(4 + data.len());
        c.extend_from_slice(kind);
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out).expect("write the PNG");
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}
