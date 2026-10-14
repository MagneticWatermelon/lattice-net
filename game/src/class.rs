//! Classes (GDD §4.3, U6): what a player deploys as (the kit of
//! `msg::encode_deploy`), each with its weapon (`weapon::of`; a kit is the
//! index of both). Abilities follow.

/// The classes, by kit.
pub const ASSAULT: u8 = 0;
pub const MEDIC: u8 = 1;
pub const ENGINEER: u8 = 2;
pub const RECON: u8 = 3;
pub const SKIRMISHER: u8 = 4;
pub const CLASSES: u8 = 5;

pub const NAMES: [&str; CLASSES as usize] = ["assault", "medic", "engineer", "recon", "skirmisher"];

/// A kit a deploy may ask for (else the server takes the assault's).
pub fn valid(kit: u8) -> u8 {
    if kit < CLASSES {
        kit
    } else {
        ASSAULT
    }
}
