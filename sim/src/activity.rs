//! Distant fights (`lattice_game::activity`): every shot counted into its
//! 128 m cell per faction over a window of `WINDOW` ticks, then encoded once
//! for every recipient. Each client is sent the finished window's cells
//! within its far radius once per window, on its own tick of the window.

use lattice_game::activity::{self, Entry, CELLS};
use lattice_game::faction::{faction, FACTIONS};

/// Ticks per window: 0.5 s at 30 Hz, the far tier's period.
pub const WINDOW: u32 = crate::interest::FAR_PERIOD;

const SLOTS: usize = (CELLS * CELLS) as usize * FACTIONS as usize;

#[derive(Debug, Clone, Copy, Default)]
struct Acc {
    shots: u32,
    /// Sums of the aim's unit vector and of the origins.
    cos: f32,
    sin: f32,
    x: f32,
    y: f32,
}

/// The window being counted and the last finished one.
pub struct Activity {
    acc: Vec<Acc>,
    /// Slots with shots this window.
    touched: Vec<u32>,
    /// The finished window's entries by slot, and which slots hold one.
    done: Vec<Entry>,
    done_slots: Vec<u32>,
    /// Per cell: a bit per faction with an entry in the finished window.
    active: Vec<u8>,
    start_step: u32,
    /// The finished window: its last step and length in steps.
    pub end_step: u32,
    pub len: u8,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            acc: vec![Acc::default(); SLOTS],
            touched: Vec::new(),
            done: vec![[0; activity::ENTRY]; SLOTS],
            done_slots: Vec::new(),
            active: vec![0; (CELLS * CELLS) as usize],
            start_step: 0,
            end_step: 0,
            len: 0,
        }
    }
}

impl Activity {
    /// A shot by `shooter` from `origin`, aimed at `yaw` (`weapon::aim`'s units).
    pub fn add(&mut self, shooter: u16, origin: [f32; 3], yaw: u16) {
        let slot = activity::cell_of([origin[0], origin[1]]) as usize * FACTIONS as usize + faction(shooter) as usize;
        let a = &mut self.acc[slot];
        if a.shots == 0 {
            self.touched.push(slot as u32);
        }
        let (s, c) = (yaw as f32 / 65536.0 * std::f32::consts::TAU).sin_cos();
        a.shots += 1;
        (a.cos, a.sin, a.x, a.y) = (a.cos + c, a.sin + s, a.x + origin[0], a.y + origin[1]);
    }

    /// Ends the window at game step `step`: its cells become what clients
    /// are sent until the next one ends. Returns the entries made.
    pub fn finish(&mut self, step: u32) -> usize {
        for &s in &self.done_slots {
            self.active[s as usize / FACTIONS as usize] = 0;
        }
        self.done_slots.clear();
        for &s in &self.touched {
            let a = std::mem::take(&mut self.acc[s as usize]);
            let (cell, f) = ((s / FACTIONS as u32) as u16, (s % FACTIONS as u32) as u8);
            let n = a.shots as f32;
            self.done[s as usize] = activity::encode_entry(cell, f, a.shots, a.sin.atan2(a.cos), [a.x / n, a.y / n]);
            self.active[cell as usize] |= 1 << f;
            self.done_slots.push(s);
        }
        self.touched.clear();
        self.len = step.saturating_sub(self.start_step).min(255) as u8;
        (self.end_step, self.start_step) = (step, step);
        self.done_slots.len()
    }

    /// The finished window's entries with their cell's center within
    /// `radius` (plus half a cell) of `pos`, with their squared distance.
    pub fn gather(&self, pos: [f32; 2], radius: f32, out: &mut Vec<(f32, Entry)>) {
        if self.done_slots.is_empty() {
            return;
        }
        let r = radius + activity::CELL * 0.5;
        let span = |v: f32| (((v - r) / activity::CELL).floor().max(0.0) as u32, (((v + r) / activity::CELL) as u32).min(CELLS - 1));
        let ((x0, x1), (y0, y1)) = (span(pos[0]), span(pos[1]));
        for cy in y0..=y1 {
            let row = cy * CELLS;
            for cx in x0..=x1 {
                let bits = self.active[(row + cx) as usize];
                if bits == 0 {
                    continue;
                }
                let c = [(cx as f32 + 0.5) * activity::CELL, (cy as f32 + 0.5) * activity::CELL];
                let d2 = (c[0] - pos[0]).powi(2) + (c[1] - pos[1]).powi(2);
                if d2 > r * r {
                    continue;
                }
                for f in 0..FACTIONS as u32 {
                    if bits & 1 << f != 0 {
                        out.push((d2, self.done[((row + cx) * FACTIONS as u32 + f) as usize]));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_game::activity::decode_entry;

    #[test]
    fn a_window_counts_shots_per_cell_and_faction() {
        let mut a = Activity::default();
        // Entities 3 and 6 (faction 0) fire east from two spots of one cell;
        // entity 4 (faction 1) north from the same cell.
        for _ in 0..10 {
            a.add(3, [1000.0, 1000.0, 50.0], 0);
            a.add(6, [1010.0, 1020.0, 50.0], 0);
        }
        a.add(4, [1005.0, 1005.0, 50.0], 16384);
        a.add(4, [5000.0, 5000.0, 50.0], 16384); // far away
        assert_eq!(a.finish(15), 3);
        let mut got = Vec::new();
        a.gather([1200.0, 1000.0], 500.0, &mut got);
        let mut d: Vec<_> = got.iter().map(|g| decode_entry(&g.1)).collect();
        d.sort_by_key(|e| e.faction);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].faction, d[0].shots, d[1].faction, d[1].shots), (0, 20, 1, 1));
        assert!(d[0].yaw.abs() < 0.03 && (d[1].yaw - std::f32::consts::FRAC_PI_2).abs() < 0.03);
        assert!((d[0].at[0] - 1005.0).abs() <= 4.0 && (d[0].at[1] - 1010.0).abs() <= 4.0, "{:?}", d[0].at);
        assert_eq!((a.end_step, a.len), (15, 15));
        // The next window replaces it; an empty one sends nothing.
        a.add(4, [5000.0, 5000.0, 50.0], 0);
        a.finish(30);
        got.clear();
        a.gather([1200.0, 1000.0], 500.0, &mut got);
        assert!(got.is_empty());
        a.gather([5000.0, 4000.0], 1500.0, &mut got);
        assert_eq!(got.len(), 1);
        a.finish(45);
        got.clear();
        a.gather([5000.0, 4000.0], 1500.0, &mut got);
        assert!(got.is_empty());
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    #[test]
    #[ignore = "timing: cargo test --release -p lattice-sim gather_cost -- --ignored --nocapture"]
    fn gather_cost() {
        // Every cell of the map active for every faction: the worst case.
        let mut a = Activity::default();
        for cy in 0..CELLS {
            for cx in 0..CELLS {
                for e in 0..3u16 {
                    a.add(e, [cx as f32 * 128.0 + 60.0, cy as f32 * 128.0 + 60.0, 0.0], 0);
                }
            }
        }
        let t = std::time::Instant::now();
        let made = a.finish(15);
        let finish = t.elapsed();
        let (mut out, n) = (Vec::new(), 10_000);
        let t = std::time::Instant::now();
        for k in 0..n {
            out.clear();
            a.gather([2000.0 + (k % 100) as f32 * 40.0, 4000.0], 1500.0, &mut out);
        }
        eprintln!("finish {made} entries: {finish:?}; gather {} entries: {:?} each", out.len(), t.elapsed() / n);
    }
}
