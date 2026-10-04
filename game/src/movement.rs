//! Deterministic player movement, shared by the server and the bots' prediction.
//!
//! Both sides run exactly this code on exactly these f32s (no FMA contraction in
//! Rust, `sqrt` is correctly rounded), so replaying an input stream reproduces the
//! server's state bit-for-bit. Any mismatch a bot sees is a real misprediction.

pub const TICK_HZ: u32 = 30;
pub const DT: f32 = 1.0 / TICK_HZ as f32;
/// Continent edge length in meters. The world spans [0, WORLD_SIZE] on x and y.
pub const WORLD_SIZE: f32 = 8192.0;
pub const RUN_SPEED: f32 = 6.0;
pub const SPRINT_SPEED: f32 = 9.0;
/// Max change of velocity per second (m/s²).
const ACCEL: f32 = 60.0;

pub const BUTTON_SPRINT: u8 = 1;

/// One tick of player intent. `move_*` is a world-space stick in [-127, 127].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Input {
    pub move_x: i8,
    pub move_y: i8,
    pub yaw: u16,
    pub buttons: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MoveState {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
}

pub fn step(s: MoveState, input: Input) -> MoveState {
    let mut wish = [input.move_x as f32 / 127.0, input.move_y as f32 / 127.0];
    let len = (wish[0] * wish[0] + wish[1] * wish[1]).sqrt();
    if len > 1.0 {
        wish = [wish[0] / len, wish[1] / len];
    }
    let speed = if input.buttons & BUTTON_SPRINT != 0 { SPRINT_SPEED } else { RUN_SPEED };

    let mut dv = [wish[0] * speed - s.vel[0], wish[1] * speed - s.vel[1]];
    let dv_len = (dv[0] * dv[0] + dv[1] * dv[1]).sqrt();
    let max_dv = ACCEL * DT;
    if dv_len > max_dv {
        let k = max_dv / dv_len;
        dv = [dv[0] * k, dv[1] * k];
    }

    let mut vel = [s.vel[0] + dv[0], s.vel[1] + dv[1]];
    let mut pos = [s.pos[0] + vel[0] * DT, s.pos[1] + vel[1] * DT];
    for i in 0..2 {
        if pos[i] < 0.0 || pos[i] > WORLD_SIZE {
            pos[i] = pos[i].clamp(0.0, WORLD_SIZE);
            vel[i] = 0.0;
        }
    }
    MoveState { pos, vel }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerates_to_run_speed_and_stops() {
        let fwd = Input { move_x: 127, ..Default::default() };
        let mut s = MoveState { pos: [100.0, 100.0], vel: [0.0; 2] };
        for _ in 0..TICK_HZ {
            s = step(s, fwd);
        }
        assert!((s.vel[0] - RUN_SPEED).abs() < 1e-4, "{s:?}");
        assert!(s.pos[0] > 104.0 && s.pos[0] < 106.0, "{s:?}");
        for _ in 0..TICK_HZ {
            s = step(s, Input::default());
        }
        assert_eq!(s.vel, [0.0, 0.0]);
    }

    #[test]
    fn diagonal_is_not_faster() {
        let diag = Input { move_x: 127, move_y: 127, buttons: BUTTON_SPRINT, ..Default::default() };
        let mut s = MoveState { pos: [100.0, 100.0], vel: [0.0; 2] };
        for _ in 0..3 * TICK_HZ {
            s = step(s, diag);
        }
        let speed = (s.vel[0] * s.vel[0] + s.vel[1] * s.vel[1]).sqrt();
        assert!((speed - SPRINT_SPEED).abs() < 1e-3, "{speed}");
    }

    #[test]
    fn clamps_to_world() {
        let back = Input { move_x: -127, move_y: -127, ..Default::default() };
        let mut s = MoveState { pos: [0.05, 0.05], vel: [0.0; 2] };
        for _ in 0..10 {
            s = step(s, back);
        }
        assert_eq!(s.pos, [0.0, 0.0]);
    }
}
