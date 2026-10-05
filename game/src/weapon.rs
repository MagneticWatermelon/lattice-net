//! The rifle, and how its projectiles fly. The server simulates every
//! projectile with this; the client flies its cosmetic tracers with it, so
//! both draw the same arc.

use crate::movement::{MoveState, TICK_HZ};

/// Muzzle speed, m/s: ~20 m a step.
pub const MUZZLE_SPEED: f32 = 600.0;
pub const GRAVITY: f32 = 9.81;
/// Steps between shots: 10 rounds a second.
pub const FIRE_STEPS: u32 = 3;
/// A projectile flies this many steps (2 s, ~1.2 km) unless it hits something.
pub const RANGE_STEPS: u32 = 60;
/// Each step is flown in this many segments (10 m each at the muzzle).
pub const SUBSTEPS: u32 = 2;
pub const DAMAGE_BODY: u8 = 20;
pub const DAMAGE_HEAD: u8 = 40;
/// Eyes (and the muzzle) above the feet.
pub const EYE_HEIGHT: f32 = 1.6;

/// A shot as its input carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shot {
    /// When in its input's 1/30 s step it fired, in 1/256ths.
    pub frac: u8,
    /// The aim then (the input's own yaw and pitch are at the step's end).
    pub yaw: u16,
    pub pitch: i16,
    /// The near render step then (`msg::render_units`): what the shooter saw.
    pub render: u16,
}

/// A shot's time on the shooter's input timeline, in 1/256 steps: input
/// `seq` covers steps seq - 1 to seq.
pub fn shot_time(seq: u32, frac: u8) -> u64 {
    (seq as u64 - 1) * 256 + frac as u64
}

/// Unit aim vector (game space: x east, y north, z up).
pub fn aim(yaw: u16, pitch: i16) -> [f32; 3] {
    let y = yaw as f32 / 65536.0 * std::f32::consts::TAU;
    let p = pitch as f32 / 32767.0 * std::f32::consts::FRAC_PI_2;
    let (sy, cy) = y.sin_cos();
    let (sp, cp) = p.sin_cos();
    [cp * cy, cp * sy, sp]
}

/// Where shots leave from: the eyes, with the body between two predicted
/// states `frac` of the way (both sides compute it the same way).
pub fn muzzle(before: &MoveState, after: &MoveState, frac: u8) -> [f32; 3] {
    let t = frac as f32 / 256.0;
    let l = |a: f32, b: f32| a + (b - a) * t;
    [l(before.pos[0], after.pos[0]), l(before.pos[1], after.pos[1]), l(before.z, after.z) + EYE_HEIGHT]
}

/// A projectile in flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flight {
    pub pos: [f32; 3],
    /// m/s.
    pub vel: [f32; 3],
}

impl Flight {
    pub fn new(origin: [f32; 3], dir: [f32; 3]) -> Self {
        Self { pos: origin, vel: dir.map(|d| d * MUZZLE_SPEED) }
    }

    /// One segment: 1 / (`SUBSTEPS` × 30) s, semi-implicit Euler.
    pub fn advance(&self) -> Self {
        let dt = 1.0 / (SUBSTEPS * TICK_HZ) as f32;
        let vel = [self.vel[0], self.vel[1], self.vel[2] - GRAVITY * dt];
        let pos = [self.pos[0] + vel[0] * dt, self.pos[1] + vel[1] * dt, self.pos[2] + vel[2] * dt];
        Self { pos, vel }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aim_points_where_yaw_and_pitch_say() {
        let a = aim(0, 0);
        assert!((a[0] - 1.0).abs() < 1e-6 && a[1].abs() < 1e-6 && a[2].abs() < 1e-6, "east");
        let n = aim(16384, 0);
        assert!((n[1] - 1.0).abs() < 1e-4, "north: {n:?}");
        let up = aim(0, 32767);
        assert!((up[2] - 1.0).abs() < 1e-4, "up: {up:?}");
    }

    #[test]
    fn bullets_drop_about_1_2_m_over_300_m() {
        let mut f = Flight::new([0.0, 0.0, 100.0], [1.0, 0.0, 0.0]);
        let mut segments = 0;
        while f.pos[0] < 300.0 {
            f = f.advance();
            segments += 1;
        }
        let drop = 100.0 - f.pos[2];
        assert!((1.1..1.4).contains(&drop), "{drop} m after {segments} segments");
        assert_eq!(segments, 30, "10 m a segment");
    }

    #[test]
    fn shot_times_order_shots() {
        assert_eq!(shot_time(10, 0) + 3 * 256, shot_time(13, 0));
        assert!(shot_time(10, 255) < shot_time(11, 0));
    }
}
