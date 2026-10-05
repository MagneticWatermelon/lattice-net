//! Game space is x east, y north, z up (meters). Bevy is y up, -z forward:
//! game (x, y, z) is Bevy (x, z, -y). Angles: game yaw is counterclockwise
//! from +x (east); pitch is up from level, both radians.

use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::math::{EulerRot, Quat, Vec3};

pub fn to_bevy(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, z, -y)
}

/// Facing game yaw `yaw` (radians).
pub fn yaw_rotation(yaw: f32) -> Quat {
    Quat::from_rotation_y(yaw - FRAC_PI_2)
}

/// Looking along game yaw `yaw`, pitched `pitch` up.
pub fn look_rotation(yaw: f32, pitch: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw - FRAC_PI_2, pitch, 0.0)
}

/// The input's yaw: a full turn in 65,536 steps.
pub fn yaw_u16(yaw: f32) -> u16 {
    (yaw.rem_euclid(TAU) / TAU * 65536.0) as u32 as u16
}

/// The input's pitch: ±π/2 as ±32,767.
pub fn pitch_i16(pitch: f32) -> i16 {
    (pitch / FRAC_PI_2 * 32767.0).clamp(-32767.0, 32767.0) as i16
}

/// A world-space movement stick from forward/right amounts in [-1, 1] when
/// facing `yaw`; diagonals aren't faster.
pub fn stick(yaw: f32, forward: f32, right: f32) -> (i8, i8) {
    let (s, c) = yaw.sin_cos();
    let (x, y) = (c * forward + s * right, s * forward - c * right);
    let len = (x * x + y * y).sqrt();
    if len < 1e-3 {
        return (0, 0);
    }
    let k = 127.0 / len.max(1.0);
    ((x * k).round() as i8, (y * k).round() as i8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facing_matches_the_game() {
        // Yaw 0 faces east (+x); in Bevy that's +x too. Yaw 90 faces north
        // (+y), which is Bevy -z.
        let fwd = |q: Quat| q * Vec3::NEG_Z;
        assert!(fwd(yaw_rotation(0.0)).distance(Vec3::X) < 1e-5);
        assert!(fwd(yaw_rotation(FRAC_PI_2)).distance(Vec3::NEG_Z) < 1e-5);
        assert!(fwd(look_rotation(0.0, 0.5)).y > 0.4, "pitch up looks up");
        assert_eq!(to_bevy(1.0, 2.0, 3.0), Vec3::new(1.0, 3.0, -2.0));
    }

    #[test]
    fn sticks_turn_with_the_view() {
        assert_eq!(stick(0.0, 1.0, 0.0), (127, 0), "east");
        assert_eq!(stick(FRAC_PI_2, 1.0, 0.0), (0, 127), "north");
        assert_eq!(stick(0.0, 0.0, 1.0), (0, -127), "facing east, right is south");
        let (x, y) = stick(0.3, 1.0, 1.0);
        assert!(((x as f32).hypot(y as f32) - 127.0).abs() < 1.5, "diagonals are full speed, not faster");
        assert_eq!(yaw_u16(-FRAC_PI_2), 49152);
        assert_eq!(pitch_i16(FRAC_PI_2 * 2.0), 32767);
    }
}
