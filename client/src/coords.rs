//! Game space is x east, y north, z up (meters). Bevy is y up, -z forward:
//! game (x, y, z) is Bevy (x, z, -y). Angles: game yaw is counterclockwise
//! from +x (east); pitch is up from level, both radians.
//!
//! How the view and keys become an input (`yaw_u16`, `pitch_i16`, `stick`)
//! is every client's, so it lives in `lattice_client_core::aim`.

use std::f32::consts::FRAC_PI_2;

use bevy::math::{EulerRot, Quat, Vec3};

pub use lattice_client_core::aim::{pitch_i16, stick, yaw_u16};

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
}
