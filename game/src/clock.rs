//! Kiln's clock (K4, docs/kiln.md "A living world"): the day and the
//! clockwork are functions of the server's game step alone, so the server
//! and every client agree on them without a byte on the wire.
//!
//! A cycle is two hours: 70 minutes of day, 10 of dusk, 30 of night, 10 of
//! dawn; step 0 is the start of a day (the server's `--clock` starts it
//! elsewhere). The clockwork:
//! - **vent lifts** (`world::VENT`) run 20 s, then rest 10 s, each at its own
//!   phase; resting, the column only slows a fall (`vent_on`);
//! - **living bridges** (`world::BRIDGE`) stand from nightfall to a minute
//!   into dawn (they grow through dusk and wither after that minute: drawn,
//!   not stood on);
//! - **sandfalls** (`world::FALLS_A`, `FALLS_B`) pour in turns, 20 minutes
//!   each: a cenote's two falls, one open while the other's dry.
//!
//! `Live` is the clockwork at a step, carried by a `movement::Mover`.
//! `Live::OFF` is the world with it switched off (no bridge, no sandfall,
//! vents running): the reachability check's, so no point depends on it.

use crate::movement::TICK_HZ;
use crate::world::{ALWAYS, BRIDGE, FALLS_A, FALLS_B, VENT};

pub const SECOND: u32 = TICK_HZ;
pub const MINUTE: u32 = 60 * SECOND;
/// A whole cycle, in steps.
pub const CYCLE: u32 = 120 * MINUTE;
/// Where each phase starts in the cycle (day starts at 0).
pub const DUSK: u32 = 70 * MINUTE;
pub const NIGHT: u32 = 80 * MINUTE;
pub const DAWN: u32 = 110 * MINUTE;
/// Living bridges stand from nightfall until a minute into dawn (that
/// minute their warning: cracking, dimming).
pub const BRIDGE_UP: u32 = NIGHT;
pub const BRIDGE_DOWN: u32 = DAWN + MINUTE;
/// A vent lift's cycle: running for `VENT_ON` of every `VENT_CYCLE`.
pub const VENT_CYCLE: u32 = 30 * SECOND;
pub const VENT_ON: u32 = 20 * SECOND;
/// How fast a resting vent lets its riders sink (m/s): a drop shaft's.
pub const VENT_SINK: f32 = 6.0;
/// The sandfalls take turns this long.
pub const FALLS: u32 = 20 * MINUTE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Day = 0,
    Dusk = 1,
    Night = 2,
    Dawn = 3,
}

/// Where `step` falls in the cycle (steps since the day began).
#[inline]
pub fn cycle(step: u32) -> u32 {
    step % CYCLE
}

pub fn phase(step: u32) -> Phase {
    match cycle(step) {
        c if c < DUSK => Phase::Day,
        c if c < NIGHT => Phase::Dusk,
        c if c < DAWN => Phase::Night,
        _ => Phase::Dawn,
    }
}

/// The step `minutes` into a cycle (the server's `--clock`).
pub fn at_minute(minutes: f64) -> u32 {
    ((minutes.rem_euclid(120.0)) * MINUTE as f64) as u32
}

/// Whether the living bridges stand at `step`.
#[inline]
pub fn bridges(step: u32) -> bool {
    (BRIDGE_UP..BRIDGE_DOWN).contains(&cycle(step))
}

/// Which sandfalls pour at `step`: `FALLS_A` or `FALLS_B`.
#[inline]
pub fn falls(step: u32) -> u8 {
    if (step / FALLS).is_multiple_of(2) {
        FALLS_A
    } else {
        FALLS_B
    }
}

/// Whether a vent lift of phase `phase` (`world::Volume::phase`, steps) runs
/// at `step`.
#[inline]
pub fn vent_on(step: u32, phase: u16) -> bool {
    (step.wrapping_add(phase as u32)) % VENT_CYCLE < VENT_ON
}

/// A step a minute into when what's there `when` is (`world::BRIDGE`: a
/// night; `FALLS_A`, `FALLS_B`: its sandfalls' turn), and a step a minute
/// into a time it isn't; for `ALWAYS` (and `VENT`), the clockwork off.
pub fn open(when: u8) -> (Live, Live) {
    match when {
        BRIDGE => (Live::at(NIGHT + MINUTE), Live::at(MINUTE)),
        FALLS_A => (Live::at(MINUTE), Live::at(FALLS + MINUTE)),
        FALLS_B => (Live::at(FALLS + MINUTE), Live::at(MINUTE)),
        _ => (Live::OFF, Live::OFF),
    }
}

/// The clockwork at a step: what's there and what runs (`Live::at`), or
/// switched off (`Live::OFF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live {
    step: u32,
    off: bool,
}

impl Live {
    /// The clockwork switched off: no living bridge, no sandfall, every vent
    /// running (the world before K4).
    pub const OFF: Live = Live { step: 0, off: true };

    pub const fn at(step: u32) -> Live {
        Live { step, off: false }
    }

    /// Its step, if it's running.
    pub fn step(&self) -> Option<u32> {
        (!self.off).then_some(self.step)
    }

    /// Whether what's there `when` (`world::ALWAYS`, `BRIDGE`, `FALLS_A`,
    /// `FALLS_B`; a vent is always there: `vent_on` says whether it runs)
    /// is there now.
    #[inline]
    pub fn there(&self, when: u8) -> bool {
        match when {
            ALWAYS | VENT => true,
            _ if self.off => false,
            BRIDGE => bridges(self.step),
            FALLS_A | FALLS_B => falls(self.step) == when,
            _ => false,
        }
    }

    /// Whether a vent lift of `phase` runs (always, switched off).
    #[inline]
    pub fn vent_on(&self, phase: u16) -> bool {
        self.off || vent_on(self.step, phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cycle_is_two_hours_of_four_phases() {
        assert_eq!(CYCLE, 2 * 3600 * TICK_HZ);
        assert_eq!(phase(0), Phase::Day);
        assert_eq!(phase(DUSK - 1), Phase::Day);
        assert_eq!(phase(DUSK), Phase::Dusk);
        assert_eq!(phase(NIGHT), Phase::Night);
        assert_eq!(phase(DAWN), Phase::Dawn);
        assert_eq!(phase(CYCLE - 1), Phase::Dawn);
        assert_eq!(phase(CYCLE), Phase::Day);
        assert_eq!(at_minute(80.0), NIGHT);
        assert_eq!(at_minute(-10.0), DAWN);
    }

    #[test]
    fn bridges_stand_from_nightfall_to_a_minute_into_dawn() {
        let up: u32 = (0..CYCLE).filter(|&s| bridges(s)).count() as u32;
        assert_eq!(up, 31 * MINUTE);
        assert!(!bridges(NIGHT - 1) && bridges(NIGHT) && bridges(DAWN + MINUTE - 1) && !bridges(DAWN + MINUTE));
        assert!(Live::at(NIGHT).there(BRIDGE) && !Live::OFF.there(BRIDGE) && Live::OFF.there(ALWAYS));
    }

    #[test]
    fn vents_run_twenty_seconds_in_thirty_and_sandfalls_take_turns() {
        let on = (0..VENT_CYCLE).filter(|&s| vent_on(s, 7)).count() as u32;
        assert_eq!(on, VENT_ON);
        assert!(Live::OFF.vent_on(0) && Live::OFF.there(VENT));
        assert_eq!(falls(0), FALLS_A);
        assert_eq!(falls(FALLS), FALLS_B);
        assert_eq!(falls(2 * FALLS), FALLS_A);
        // Exactly one set pours at any step, and a cycle holds whole turns.
        assert_eq!(CYCLE % (2 * FALLS), 0);
        for s in [0, FALLS - 1, FALLS, 5 * FALLS + 3] {
            assert!(Live::at(s).there(FALLS_A) != Live::at(s).there(FALLS_B));
        }
        assert!(!Live::OFF.there(FALLS_A) && !Live::OFF.there(FALLS_B));
    }
}
