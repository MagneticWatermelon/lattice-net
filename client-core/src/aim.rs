//! How a client's view and keys become an input: the yaw and pitch it
//! carries, and its world-space movement stick. Every client sends the same
//! encoding (the Bevy client and the Unity plugin both call these), so it
//! lives with the rest of the client, not with any one window.
//!
//! Angles are the game's: yaw counterclockwise from east (+x), pitch up from
//! level, both radians.

use std::f32::consts::{FRAC_PI_2, TAU};

/// The input's yaw: a full turn in 65,536 steps.
pub fn yaw_u16(yaw: f32) -> u16 {
    (yaw.rem_euclid(TAU) / TAU * 65536.0) as u32 as u16
}

/// The input's pitch: ±π/2 as ±32,767.
pub fn pitch_i16(pitch: f32) -> i16 {
    (pitch / FRAC_PI_2 * 32767.0).clamp(-32767.0, 32767.0) as i16
}

/// An input's yaw as radians (`yaw_u16`'s inverse).
pub fn yaw_radians(yaw: u16) -> f32 {
    yaw as f32 / 65536.0 * TAU
}

/// An input's pitch as radians (`pitch_i16`'s inverse).
pub fn pitch_radians(pitch: i16) -> f32 {
    pitch as f32 / 32767.0 * FRAC_PI_2
}

/// A world-space movement stick from forward/right amounts in [-1, 1] when
/// facing `yaw`; diagonals aren't faster. (The stick is the input itself, so
/// std's `sin_cos` is fine here: the server moves by what's sent.)
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
    fn sticks_turn_with_the_view() {
        assert_eq!(stick(0.0, 1.0, 0.0), (127, 0), "east");
        assert_eq!(stick(FRAC_PI_2, 1.0, 0.0), (0, 127), "north");
        assert_eq!(stick(0.0, 0.0, 1.0), (0, -127), "facing east, right is south");
        let (x, y) = stick(0.3, 1.0, 1.0);
        assert!(((x as f32).hypot(y as f32) - 127.0).abs() < 1.5, "diagonals are full speed, not faster");
        assert_eq!(stick(1.0, 0.0, 0.0), (0, 0), "no keys, no stick");
        let (x, y) = stick(0.0, 0.5, 0.0);
        assert_eq!((x, y), (64, 0), "a half-pressed stick walks at half speed");
    }

    #[test]
    fn angles_wrap_and_clamp() {
        assert_eq!(yaw_u16(-FRAC_PI_2), 49152);
        assert_eq!(yaw_u16(0.0), 0);
        assert_eq!(yaw_u16(TAU), 0, "a full turn is no turn");
        assert_eq!(yaw_u16(FRAC_PI_2), 16384);
        assert_eq!(pitch_i16(FRAC_PI_2 * 2.0), 32767);
        assert_eq!(pitch_i16(-FRAC_PI_2), -32767);
        assert_eq!(pitch_i16(0.0), 0);
    }
}
