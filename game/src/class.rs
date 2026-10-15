//! Classes (GDD §4.3, U6): what a player deploys as (the kit of
//! `msg::encode_deploy`), with one of its weapons (`WEAPONS_OF`; its first
//! is its own) and its ability (`ability`). What others see of a player is
//! its look: the class and the weapon in a byte (`look`).

/// The classes, by kit.
pub const ASSAULT: u8 = 0;
pub const MEDIC: u8 = 1;
pub const ENGINEER: u8 = 2;
pub const RECON: u8 = 3;
pub const SKIRMISHER: u8 = 4;
/// Heavy armor (GDD §4.3): 300 armor (it takes a third of the damage on its
/// 100 health), slower, two arm weapons, Lock-down; one every 3 minutes.
pub const EXO: u8 = 5;
pub const CLASSES: u8 = 6;

pub const NAMES: [&str; CLASSES as usize] = ["assault", "medic", "engineer", "recon", "skirmisher", "exo"];

/// After deploying an Exo, a player may deploy another this many steps later (3 min).
pub const EXO_COOLDOWN: u32 = 180 * 30;

/// The weapons each class may carry (`weapon::WEAPONS`), its own first.
pub const WEAPONS_OF: [&[u8]; CLASSES as usize] = [
    &[crate::weapon::RIFLE, crate::weapon::LMG],
    &[crate::weapon::CARBINE, crate::weapon::RIFLE],
    &[crate::weapon::SHOTGUN, crate::weapon::CARBINE, crate::weapon::SMG],
    &[crate::weapon::MARKSMAN, crate::weapon::SNIPER, crate::weapon::SMG],
    &[crate::weapon::SMG, crate::weapon::CARBINE, crate::weapon::SHOTGUN],
    &[crate::weapon::EXO_GUNS, crate::weapon::EXO_LAUNCHER],
];

/// A kit a deploy may ask for (else the server takes the assault's).
pub fn valid(kit: u8) -> u8 {
    if kit < CLASSES {
        kit
    } else {
        ASSAULT
    }
}

/// The weapon a deploy as `kit` asking for `weapon` gets: it, if the class
/// may carry it, else the class's own.
pub fn weapon_for(kit: u8, weapon: u8) -> u8 {
    let list = WEAPONS_OF[valid(kit) as usize];
    if list.contains(&weapon) {
        weapon
    } else {
        list[0]
    }
}

/// A player's look, as others are sent it: its class (3 bits) and weapon (5).
pub fn look(kit: u8, weapon: u8) -> u8 {
    (kit & 7) | (weapon & 31) << 3
}

pub fn kit_of(look: u8) -> u8 {
    look & 7
}

pub fn weapon_of(look: u8) -> u8 {
    look >> 3
}
