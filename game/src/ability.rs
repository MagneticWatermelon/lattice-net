//! The classes' abilities (GDD §4.3, U6), on their own button
//! (`BUTTON_ABILITY`). Each player's runs on its inputs' steps on both sides,
//! like the rounds it rides with (`weapon::Ammo::ability`): whether it's
//! running, its cooldown and its energy are predicted, and so is what it does
//! to the holder's movement (`Gear`). What it does to others (damage taken
//! under a Bulwark, the Mend field's healing, who's sent a cloaked player) is
//! the server's.
//!
//! - Assault, **Bulwark**: pressed, 6 s of taking 40% less damage, slower and
//!   no sprint; again 25 s after it started.
//! - Medic, **Mend field**: pressed, 8 s of healing every friend within 6 m
//!   of the medic (the medic too); again 40 s after.
//! - Recon, **Cloak**: pressed, on (no sprint) until pressed again, a shot,
//!   or its energy (10 s) runs out; it recharges at half the rate, from a
//!   second after it's dropped. Enemies past 15 m aren't sent the cloaked.
//! - Skirmisher, **jump jets**: held, thrust upward (and better air control)
//!   for up to 1.8 s of fuel, which refills at half the rate from half a
//!   second after the jets stop.
//! - Engineer: its turret is still to come; the button does nothing.
//! - Exo, **Lock-down**: pressed, it plants itself (no moving or jumping)
//!   and fires 40% faster, until pressed again (a second between). Always
//!   slower than a soldier and never sprinting; it takes a third of the
//!   damage (`taken`).

use crate::class::{ASSAULT, EXO, MEDIC, RECON, SKIRMISHER};
use crate::movement::TICK_HZ;

/// The ability's button: pressed (Bulwark, the Mend field, the cloak on or
/// off) or held (jets).
pub const BUTTON_ABILITY: u8 = 64;

const S: u16 = TICK_HZ as u16;
pub const BULWARK_STEPS: u16 = 6 * S;
/// From its start until it can be used again.
pub const BULWARK_COOLDOWN: u16 = 25 * S;
/// What a Bulwark lets through of the damage taken, in tenths.
pub const BULWARK_TAKEN: u32 = 6;
/// Its holder's speed, of what it would be.
pub const BULWARK_SPEED: f32 = 0.7;
pub const MEND_STEPS: u16 = 8 * S;
pub const MEND_COOLDOWN: u16 = 40 * S;
/// Who it heals: friends (and the medic) within this of the medic, m.
pub const MEND_RADIUS: f32 = 6.0;
/// Health it gives each of them every `MEND_PERIOD` steps (8 a second).
pub const MEND_HEAL: u8 = 4;
pub const MEND_PERIOD: u16 = S / 2;
/// Energy is in half steps of use: it drains 2 a step, refills 1.
pub const CLOAK_ENERGY: u16 = 2 * 10 * S;
/// Energy needed to cloak (a second's).
pub const CLOAK_MIN: u16 = 2 * S;
/// After the cloak drops, before it refills or can go on again.
pub const CLOAK_DELAY: u16 = S;
/// Enemies past this aren't sent a cloaked player, m.
pub const CLOAK_RANGE: f32 = 15.0;
pub const JET_ENERGY: u16 = 2 * 54;
/// After the jets stop, before they refill.
pub const JET_DELAY: u16 = S / 2;
/// The jets' push (gravity's 15 is taken off it), the fastest they climb
/// at, and the air control they give (m/s²).
pub const JET_ACCEL: f32 = 32.0;
pub const JET_SPEED: f32 = 6.5;
pub const JET_AIR_ACCEL: f32 = 30.0;
/// An Exo's speed, of a soldier's.
pub const EXO_SPEED: f32 = 0.75;
/// Between Lock-down's going on and off, steps.
pub const LOCK_DELAY: u16 = S;
/// Locked down, a shot comes this much of its interval after the last (3/5).
pub const LOCK_RATE: [u32; 2] = [3, 5];

/// What an ability does to its holder's movement for a step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gear {
    /// The jets fire.
    pub jets: bool,
    /// Its speed, of what it would be.
    pub speed: f32,
    /// No sprinting.
    pub no_sprint: bool,
    /// No jumping.
    pub no_jump: bool,
}

impl Gear {
    pub const NONE: Gear = Gear { jets: false, speed: 1.0, no_sprint: false, no_jump: false };
}

impl Default for Gear {
    fn default() -> Self {
        Self::NONE
    }
}

/// A player's ability, as its inputs run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ability {
    /// Steps it has left running (the jets, the cloak: 1 while on).
    pub active: u16,
    /// Steps until it can start again (jets, cloak: until they refill).
    pub cooldown: u16,
    /// Energy left (the cloak's, the jets'), in half steps of use.
    pub energy: u16,
    /// Its button was down the step before (a press is its going down).
    pub held: bool,
}

impl Ability {
    /// As a life starts with kit `kit`: ready, full.
    pub fn fresh(kit: u8) -> Ability {
        Ability { energy: max_energy(kit), ..Default::default() }
    }

    /// What it does to movement this step: from its state before the step
    /// and the step's buttons (`step` comes after, with the same buttons).
    pub fn gear(&self, kit: u8, buttons: u8) -> Gear {
        match kit {
            SKIRMISHER if buttons & BUTTON_ABILITY != 0 && self.energy >= 2 => Gear { jets: true, ..Gear::NONE },
            ASSAULT if self.active > 0 => Gear { speed: BULWARK_SPEED, no_sprint: true, ..Gear::NONE },
            RECON if self.active > 0 => Gear { no_sprint: true, ..Gear::NONE },
            EXO if self.active > 0 => Gear { speed: 0.0, no_sprint: true, no_jump: true, ..Gear::NONE },
            EXO => Gear { speed: EXO_SPEED, no_sprint: true, ..Gear::NONE },
            _ => Gear::NONE,
        }
    }

    /// Whether it's on now: running (Bulwark, the Mend field), cloaked, or
    /// the jets firing.
    pub fn on(&self) -> bool {
        self.active > 0
    }

    /// One input's step, after movement and the shot: `fired` if the step
    /// fired a round (it drops the cloak). Returns whether it started this
    /// step (Bulwark, the Mend field, the cloak going on, the jets lighting).
    pub fn step(&mut self, kit: u8, buttons: u8, fired: bool) -> bool {
        let down = buttons & BUTTON_ABILITY != 0;
        let pressed = down && !self.held;
        self.held = down;
        let max = max_energy(kit);
        let refill = |a: &mut Ability| {
            if a.cooldown > 0 {
                a.cooldown -= 1;
            } else {
                a.energy = (a.energy + 1).min(max);
            }
        };
        match kit {
            ASSAULT | MEDIC => {
                self.active = self.active.saturating_sub(1);
                self.cooldown = self.cooldown.saturating_sub(1);
                if pressed && self.cooldown == 0 {
                    (self.active, self.cooldown) = if kit == ASSAULT { (BULWARK_STEPS, BULWARK_COOLDOWN) } else { (MEND_STEPS, MEND_COOLDOWN) };
                    return true;
                }
                false
            }
            RECON => {
                if self.active > 0 {
                    if pressed || fired || self.energy < 2 {
                        (self.active, self.cooldown) = (0, CLOAK_DELAY);
                    } else {
                        self.energy -= 2;
                    }
                    return false;
                }
                refill(self);
                if pressed && self.cooldown == 0 && self.energy >= CLOAK_MIN {
                    self.active = 1;
                    return true;
                }
                false
            }
            EXO => {
                self.cooldown = self.cooldown.saturating_sub(1);
                if pressed && self.cooldown == 0 {
                    self.active = if self.active > 0 { 0 } else { 1 };
                    self.cooldown = LOCK_DELAY;
                    return self.active > 0;
                }
                false
            }
            SKIRMISHER => {
                if down && self.energy >= 2 {
                    let lit = self.active == 0;
                    (self.energy, self.active, self.cooldown) = (self.energy - 2, 1, JET_DELAY);
                    return lit;
                }
                // (Held dry, they stay dry: no sputtering on what trickles back.)
                self.active = 0;
                if !down {
                    refill(self);
                }
                false
            }
            _ => false,
        }
    }
}

/// Kit `kit`'s ability's energy, full (0: it has none).
pub fn max_energy(kit: u8) -> u16 {
    match kit {
        RECON => CLOAK_ENERGY,
        SKIRMISHER => JET_ENERGY,
        _ => 0,
    }
}

/// Kit `kit`'s ability's name (the HUD's).
pub fn name(kit: u8) -> &'static str {
    match kit {
        ASSAULT => "Bulwark",
        MEDIC => "Mend field",
        RECON => "Cloak",
        SKIRMISHER => "Jump jets",
        EXO => "Lock-down",
        _ => "",
    }
}

/// A shot's interval (`weapon::Weapon::interval`) for kit `kit` with its
/// ability on or not: an Exo locked down fires faster.
pub fn interval(kit: u8, on: bool, interval: u32) -> u32 {
    if kit == EXO && on {
        interval * LOCK_RATE[0] / LOCK_RATE[1]
    } else {
        interval
    }
}

/// Damage `amount` as it lands on someone of kit `kit` whose ability is
/// `on` or not: under a Bulwark, 60% of it; on an Exo's armor, a third
/// (rounded up).
pub fn taken(kit: u8, on: bool, amount: u8) -> u8 {
    if kit == ASSAULT && on {
        ((amount as u32 * BULWARK_TAKEN).div_ceil(10)) as u8
    } else if kit == EXO {
        (amount as u32).div_ceil(3) as u8
    } else {
        amount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(kit: u8, a: &mut Ability, buttons: u8, steps: u32) -> u32 {
        (0..steps).map(|_| a.step(kit, buttons, false) as u32).sum()
    }

    #[test]
    fn bulwark_runs_then_cools_down() {
        let mut a = Ability::fresh(ASSAULT);
        assert_eq!(a.gear(ASSAULT, 0), Gear::NONE);
        assert!(a.step(ASSAULT, BUTTON_ABILITY, false), "a press starts it");
        assert_eq!(a.gear(ASSAULT, 0).speed, BULWARK_SPEED);
        assert_eq!(taken(ASSAULT, a.on(), 20), 12);
        // Held, or pressed again while it cools down: nothing more.
        assert_eq!(run(ASSAULT, &mut a, BUTTON_ABILITY, 10) + run(ASSAULT, &mut a, 0, 1) + run(ASSAULT, &mut a, BUTTON_ABILITY, 1), 0);
        run(ASSAULT, &mut a, 0, BULWARK_STEPS as u32);
        assert!(!a.on() && taken(ASSAULT, a.on(), 20) == 20, "over after 6 s");
        run(ASSAULT, &mut a, 0, (BULWARK_COOLDOWN - BULWARK_STEPS) as u32 - 14);
        assert!(!a.step(ASSAULT, BUTTON_ABILITY, false), "not yet");
        a.step(ASSAULT, 0, false);
        assert!(a.step(ASSAULT, BUTTON_ABILITY, false), "again 25 s after it started");
    }

    #[test]
    fn an_exo_locks_down_and_takes_a_third() {
        let mut a = Ability::fresh(EXO);
        assert_eq!(a.gear(EXO, 0).speed, EXO_SPEED);
        assert!(a.gear(EXO, 0).no_sprint && !a.gear(EXO, 0).no_jump);
        assert!(a.step(EXO, BUTTON_ABILITY, false), "locked down");
        let g = a.gear(EXO, 0);
        assert!(g.speed == 0.0 && g.no_jump);
        assert_eq!(interval(EXO, a.on(), 640), 384);
        run(EXO, &mut a, 0, 3);
        assert!(!a.step(EXO, BUTTON_ABILITY, false) && a.on(), "not straight back up");
        run(EXO, &mut a, 0, LOCK_DELAY as u32);
        a.step(EXO, BUTTON_ABILITY, false);
        assert!(!a.on(), "up again");
        assert_eq!((taken(EXO, false, 20), taken(EXO, false, 200)), (7, 67));
    }

    #[test]
    fn jets_burn_fuel_and_refill() {
        let mut a = Ability::fresh(SKIRMISHER);
        assert!(a.gear(SKIRMISHER, BUTTON_ABILITY).jets);
        let lit = run(SKIRMISHER, &mut a, BUTTON_ABILITY, 200);
        assert_eq!((lit, a.energy), (1, 0), "lit once, held till empty");
        assert!(!a.gear(SKIRMISHER, BUTTON_ABILITY).jets, "dry");
        run(SKIRMISHER, &mut a, 0, (JET_DELAY + JET_ENERGY) as u32);
        assert_eq!(a.energy, JET_ENERGY, "full again");
    }

    #[test]
    fn a_shot_drops_the_cloak() {
        let mut a = Ability::fresh(RECON);
        assert!(a.step(RECON, BUTTON_ABILITY, false));
        assert!(a.on() && a.gear(RECON, 0).no_sprint);
        run(RECON, &mut a, 0, 30);
        assert_eq!(a.energy, CLOAK_ENERGY - 60);
        a.step(RECON, 0, true);
        assert!(!a.on(), "a shot uncloaks");
        assert!(!a.step(RECON, BUTTON_ABILITY, false), "not straight back on");
        run(RECON, &mut a, 0, CLOAK_DELAY as u32);
        assert!(a.step(RECON, BUTTON_ABILITY, false));
        assert!(!a.step(RECON, 0, false) && a.on());
        a.step(RECON, BUTTON_ABILITY, false);
        assert!(!a.on(), "pressed again, off");
    }
}
