//! The rifle, and how its projectiles fly. The server simulates every
//! projectile with this; the client flies its cosmetic tracers with it, so
//! both draw the same arc.
//!
//! Accuracy follows PlanetSide 2's model (an assault rifle's numbers): a
//! shot leaves somewhere in a *cone of fire*, tight aiming down sights, wide
//! from the hip, wider moving or in the air, and *bloom* widens it with each
//! shot of a burst. The server decides where every shot goes (`spread`, from
//! the shooter's id and the shot's seq, so the shooter's client predicts the
//! same direction for its tracer). *Recoil* kicks the shooter's view; it's the
//! client's, since shots go where the view points.

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

/// Cone of fire, degrees across its radius: aiming down sights or from the
/// hip, standing or moving (faster than `MOVING` m/s).
pub const CONE_ADS: f32 = 0.1;
pub const CONE_ADS_MOVING: f32 = 0.4;
pub const CONE_HIP: f32 = 2.0;
pub const CONE_HIP_MOVING: f32 = 2.75;
pub const MOVING: f32 = 1.0;
/// In the air the cone is this many times wider.
pub const CONE_AIR: f32 = 2.5;
/// Bloom: each shot widens the next one's cone by this much (degrees), up
/// to the max; it starts recovering `BLOOM_DELAY` steps after a shot, at
/// `BLOOM_RECOVERY` degrees a step.
pub const BLOOM_ADS: f32 = 0.04;
pub const BLOOM_HIP: f32 = 0.1;
pub const BLOOM_MAX_ADS: f32 = 0.5;
pub const BLOOM_MAX_HIP: f32 = 1.5;
pub const BLOOM_DELAY: f32 = 4.0;
pub const BLOOM_RECOVERY: f32 = 0.4;
/// Recoil (the client's view): each shot kicks it up `RECOIL_UP` degrees
/// (the first of a burst `RECOIL_FIRST` times that) and sideways within
/// `RECOIL_SIDE`, a little more to the right; `RECOIL_DELAY` s after the
/// last shot it drifts back down at `RECOIL_RECOVERY` degrees a second.
pub const RECOIL_UP: f32 = 0.32;
pub const RECOIL_FIRST: f32 = 1.75;
pub const RECOIL_SIDE: [f32; 2] = [-0.16, 0.22];
pub const RECOIL_DELAY: f32 = 0.12;
pub const RECOIL_RECOVERY: f32 = 9.0;
/// A shot this long (s) after the last starts a new burst.
pub const BURST_GAP: f32 = 0.35;
/// Aiming down sights: movement at this share of running speed (no sprint),
/// and the view zoomed this many times.
pub const ADS_SPEED: f32 = 0.5;
pub const ADS_ZOOM: f32 = 1.35;

/// A shooter's bloom. The server keeps one per player and the client one
/// for its own shots; fed the same shots in the same order, they give the
/// same cones.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bloom {
    deg: f32,
    /// The last shot (`shot_time`).
    last: Option<u64>,
}

impl Bloom {
    /// The cone (degrees) of a shot at `time` (`shot_time`), aiming down
    /// sights or not, from `state` (the shooter before its step); then the
    /// shot blooms the next one.
    pub fn fire(&mut self, time: u64, ads: bool, state: &MoveState) -> f32 {
        let cone = self.cone(time, ads, state);
        let (add, max) = if ads { (BLOOM_ADS, BLOOM_MAX_ADS) } else { (BLOOM_HIP, BLOOM_MAX_HIP) };
        self.deg = (self.recovered(time) + add).min(max);
        self.last = Some(time);
        cone
    }

    /// The cone a shot at `time` would have (the crosshair shows it).
    pub fn cone(&self, time: u64, ads: bool, state: &MoveState) -> f32 {
        let speed = (state.vel[0] * state.vel[0] + state.vel[1] * state.vel[1]).sqrt();
        let base = match (ads, speed > MOVING) {
            (true, false) => CONE_ADS,
            (true, true) => CONE_ADS_MOVING,
            (false, false) => CONE_HIP,
            (false, true) => CONE_HIP_MOVING,
        };
        (base + self.recovered(time)) * if state.grounded { 1.0 } else { CONE_AIR }
    }

    fn recovered(&self, time: u64) -> f32 {
        match self.last {
            Some(last) if time > last => {
                let steps = (time - last) as f32 / 256.0;
                (self.deg - (steps - BLOOM_DELAY).max(0.0) * BLOOM_RECOVERY).max(0.0)
            }
            _ => self.deg,
        }
    }
}

fn mix(mut h: u64) -> u64 {
    // splitmix64
    h = h.wrapping_add(0x9E37_79B9_7F4A_7C15);
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

/// Where a shot aimed at (`yaw`, `pitch`) with a `cone` (degrees) goes: a
/// point of the cone picked by (`shooter`, `seq`), uniform over its disc.
/// Unit vector.
pub fn spread(yaw: u16, pitch: i16, cone: f32, shooter: u16, seq: u32) -> [f32; 3] {
    let d = aim(yaw, pitch);
    if cone <= 0.0 {
        return d;
    }
    let h = mix((shooter as u64) << 32 | seq as u64);
    let (u1, u2) = ((h >> 40) as f32 / (1u64 << 24) as f32, (h & 0xFF_FFFF) as f32 / (1u64 << 24) as f32);
    let r = (cone.to_radians() * u1.sqrt()).tan();
    let (s, c) = (u2 * std::f32::consts::TAU).sin_cos();
    // Right and up around the aim (straight up or down: any right will do).
    let flat = (d[0] * d[0] + d[1] * d[1]).sqrt();
    let right = if flat > 1e-4 { [d[1] / flat, -d[0] / flat, 0.0] } else { [1.0, 0.0, 0.0] };
    let up = [right[1] * d[2] - right[2] * d[1], right[2] * d[0] - right[0] * d[2], right[0] * d[1] - right[1] * d[0]];
    let v = [0, 1, 2].map(|k| d[k] + right[k] * r * c + up[k] * r * s);
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|x| x / len)
}

/// The aim (`yaw`, `pitch` in wire units) of a unit direction.
pub fn angles(d: [f32; 3]) -> (u16, i16) {
    let yaw = d[1].atan2(d[0]).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0;
    let pitch = d[2].clamp(-1.0, 1.0).asin() / std::f32::consts::FRAC_PI_2 * 32767.0;
    (yaw as u32 as u16, pitch.clamp(-32767.0, 32767.0) as i16)
}

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
    fn the_cone_is_tight_aiming_wide_from_the_hip_and_blooms() {
        let stand = MoveState { grounded: true, ..Default::default() };
        let run = MoveState { vel: [6.0, 0.0], grounded: true, ..Default::default() };
        let air = MoveState { grounded: false, ..Default::default() };
        let b = Bloom::default();
        assert_eq!(b.cone(0, true, &stand), CONE_ADS);
        assert_eq!(b.cone(0, true, &run), CONE_ADS_MOVING);
        assert_eq!(b.cone(0, false, &stand), CONE_HIP);
        assert!(b.cone(0, false, &air) > b.cone(0, false, &run));
        // A burst at the rifle's rate blooms each shot, up to the max...
        let mut b = Bloom::default();
        let cones: Vec<f32> = (0..30).map(|k| b.fire(k * FIRE_STEPS as u64 * 256, true, &stand)).collect();
        assert!(cones.windows(2).all(|w| w[1] >= w[0]), "{cones:?}");
        assert!((cones[1] - CONE_ADS - BLOOM_ADS).abs() < 1e-6);
        assert!((cones[29] - CONE_ADS - BLOOM_MAX_ADS).abs() < 1e-6);
        // ...and a pause lets it recover.
        let later = 30 * FIRE_STEPS as u64 * 256 + 30 * 256;
        assert_eq!(b.cone(later, true, &stand), CONE_ADS);
    }

    #[test]
    fn shots_spread_uniformly_over_the_cone_and_deterministically() {
        let (yaw, pitch) = (12000u16, 2000i16);
        let d = aim(yaw, pitch);
        let mut far = 0;
        for seq in 0..2000 {
            let s = spread(yaw, pitch, 2.0, 7, seq);
            assert_eq!(s, spread(yaw, pitch, 2.0, 7, seq), "same shooter and seq: same shot");
            let angle = (d[0] * s[0] + d[1] * s[1] + d[2] * s[2]).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(angle <= 2.0 + 1e-3, "{angle}");
            far += (angle > 2.0 * std::f32::consts::FRAC_1_SQRT_2) as u32;
        }
        // Uniform over the disc: half the area is past radius / sqrt 2.
        assert!((900..1100).contains(&far), "{far} of 2000 in the outer half");
        assert_ne!(spread(yaw, pitch, 2.0, 7, 1), spread(yaw, pitch, 2.0, 8, 1), "shooters differ");
        let (y, p) = angles(d);
        assert!((y as i32 - yaw as i32).abs() <= 1 && (p as i32 - pitch as i32).abs() <= 1);
    }

    #[test]
    fn shot_times_order_shots() {
        assert_eq!(shot_time(10, 0) + 3 * 256, shot_time(13, 0));
        assert!(shot_time(10, 255) < shot_time(11, 0));
    }
}
