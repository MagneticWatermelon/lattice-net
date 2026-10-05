//! Distant fights: ambient shots from the server's per-cell firing activity
//! (`lattice_game::activity`). Each window's cells arrive once; for each, the
//! shots we didn't already draw exactly (near-tier tracers, our own) are
//! spread over the next window as ambient shots. Each leaves from a drawn
//! player of that faction in that cell when there is one, along its drawn
//! aim, else from around the cell's mean origin along its mean aim.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use lattice_game::activity::{cell_of, CellFire};
use lattice_game::faction::faction;
use lattice_game::movement::TICK_HZ;
use lattice_game::weapon::EYE_HEIGHT;
use lattice_game::world::World;

use crate::entities::Entities;

/// Most ambient shots a second (the rest of a huge battle is scaled down):
/// each is a tracer to fly and draw.
pub const MAX_AMBIENT_PER_SEC: f32 = 600.0;
/// Exact shots kept to subtract (steps).
const EXACT_MEMORY: u32 = 90;

/// A shot to draw for a distant fight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientShot {
    /// The muzzle (eye height).
    pub origin: [f32; 3],
    /// Unit aim.
    pub dir: [f32; 3],
    pub faction: u8,
    /// The drawn player it leaves from, if any.
    pub shooter: Option<u16>,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Instant,
    faction: u8,
    shooter: Option<u16>,
    /// The cell's mean origin and aim, for when no player is drawn.
    near: [f32; 2],
    yaw: f32,
}

#[derive(Debug, Default)]
pub struct Distant {
    /// Shots we drew exactly: (cell × 3 + faction, the server step it was
    /// counted at), oldest first.
    exact: VecDeque<(u32, u32)>,
    pending: Vec<Pending>,
    rng: u64,
}

fn key(pos: [f32; 2], f: u8) -> u32 {
    cell_of(pos) as u32 * 3 + f as u32
}

impl Distant {
    pub fn new(seed: u64) -> Self {
        Self { rng: seed | 1, ..Self::default() }
    }

    fn rand(&mut self) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// A shot from `pos` by `entity` that we draw exactly, counted by the
    /// server at `step`.
    pub fn exact(&mut self, pos: [f32; 2], entity: u16, step: u32) {
        self.exact.push_back((key(pos, faction(entity)), step));
        while self.exact.front().is_some_and(|&(_, s)| s.wrapping_add(EXACT_MEMORY) < step) {
            self.exact.pop_front();
        }
    }

    /// A finished window (its last step and length in steps). Schedules its
    /// ambient shots over the next window from `now` when `keep` (a bot only
    /// counts them). Returns the ambient shots it makes.
    pub fn on_window(&mut self, step: u32, len: u8, cells: &[CellFire], ents: Option<&Entities>, now: Instant, keep: bool) -> u64 {
        let start = step.wrapping_sub(len as u32);
        let mut drawn: HashMap<u32, u32> = HashMap::new();
        for &(k, s) in &self.exact {
            if s.wrapping_sub(start) as i32 > 0 && s.wrapping_sub(step) as i32 <= 0 {
                *drawn.entry(k).or_default() += 1;
            }
        }
        let want: Vec<u32> = cells.iter().map(|c| c.shots.saturating_sub(drawn.get(&(c.cell as u32 * 3 + c.faction as u32)).copied().unwrap_or(0))).collect();
        let total: u32 = want.iter().sum();
        let secs = len.max(1) as f32 / TICK_HZ as f32;
        let scale = (MAX_AMBIENT_PER_SEC * secs / total.max(1) as f32).min(1.0);
        // Who could have fired: drawn, alive players by cell and faction.
        let mut who: HashMap<u32, Vec<u16>> = HashMap::new();
        if keep && total > 0 {
            if let Some(e) = ents {
                for id in e.ids() {
                    if let Some(s) = e.newest(id).filter(|s| !s.dead) {
                        who.entry(key([s.pos[0], s.pos[1]], faction(id))).or_default().push(id);
                    }
                }
            }
        }
        let mut made = 0u64;
        for (c, &n) in cells.iter().zip(&want) {
            // Stochastic rounding keeps the expected count.
            let n = (n as f32 * scale + self.rand()) as u32;
            made += n as u64;
            if !keep {
                continue;
            }
            let shooters = who.get(&(c.cell as u32 * 3 + c.faction as u32));
            for _ in 0..n {
                let at = now + Duration::from_secs_f32(self.rand() * secs);
                let shooter = shooters.map(|v| v[(self.rand() * v.len() as f32) as usize % v.len()]);
                self.pending.push(Pending { at, faction: c.faction, shooter, near: c.at, yaw: c.yaw });
            }
        }
        made
    }

    /// The ambient shots due by `now`, drawn at render step `render`.
    pub fn drain(&mut self, now: Instant, render: Option<f64>, ents: Option<&Entities>, world: Option<&World>, out: &mut Vec<AmbientShot>) {
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].at > now {
                i += 1;
                continue;
            }
            let p = self.pending.swap_remove(i);
            let drawn = p.shooter.zip(render).zip(ents).and_then(|((id, r), e)| e.render_one(id, r)).filter(|s| !s.dead);
            let (j1, j2) = (self.rand() - 0.5, self.rand() - 0.5);
            let shot = match drawn {
                Some(s) => AmbientShot {
                    origin: [s.pos[0], s.pos[1], s.pos[2] + EYE_HEIGHT],
                    dir: dir(s.yaw + j1 * 0.06, s.pitch + j2 * 0.04),
                    faction: p.faction,
                    shooter: p.shooter,
                },
                None => {
                    let (x, y) = (p.near[0] + j1 * 20.0, p.near[1] + j2 * 20.0);
                    let z = world.map_or(0.0, |w| w.terrain(x, y)) + EYE_HEIGHT;
                    let (j3, j4) = (self.rand() - 0.5, self.rand());
                    AmbientShot { origin: [x, y, z], dir: dir(p.yaw + j3 * 0.4, j4 * 0.06 - 0.01), faction: p.faction, shooter: None }
                }
            };
            out.push(shot);
        }
    }

    /// Ambient shots scheduled but not yet due.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

fn dir(yaw: f32, pitch: f32) -> [f32; 3] {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    [cp * cy, cp * sy, sp]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_game::activity::decode_entry;
    use lattice_game::activity::encode_entry;

    #[test]
    fn shots_drawn_exactly_are_not_drawn_again() {
        let mut d = Distant::new(7);
        let now = Instant::now();
        let at = [2000.0, 2000.0];
        let cell = cell_of(at);
        // Entity 3 (faction 0): 12 of its 30 shots we drew as tracers, in the
        // window (100, 115]; 5 more before it don't count.
        for s in 101..113 {
            d.exact(at, 3, s);
        }
        for s in 90..95 {
            d.exact(at, 3, s);
        }
        let cells = [decode_entry(&encode_entry(cell, 0, 30, 0.0, at)), decode_entry(&encode_entry(cell, 1, 4, 0.0, at))];
        let made = d.on_window(115, 15, &cells, None, now, true);
        assert_eq!(made, 18 + 4);
        let mut out = Vec::new();
        d.drain(now, None, None, None, &mut out);
        let mut later = Vec::new();
        d.drain(now + Duration::from_millis(500), None, None, None, &mut later);
        assert!(out.len() < 4, "spread over the window: {} at once", out.len());
        assert_eq!(out.len() + later.len(), 22);
        assert!(later.iter().all(|s| s.shooter.is_none() && (s.origin[0] - at[0]).abs() <= 14.0 && s.dir[0] > 0.9));
    }

    #[test]
    fn a_huge_battle_is_scaled_down() {
        let mut d = Distant::new(1);
        let cells: Vec<_> = (0..40u16).map(|c| decode_entry(&encode_entry(c * 3, 0, 1000, 0.0, [c as f32 * 384.0 + 10.0, 10.0]))).collect();
        let made = d.on_window(1000, 15, &cells, None, Instant::now(), false) as f32;
        assert!((made - MAX_AMBIENT_PER_SEC / 2.0).abs() < 40.0, "{made}");
        assert_eq!(d.pending(), 0, "only counted");
    }
}
