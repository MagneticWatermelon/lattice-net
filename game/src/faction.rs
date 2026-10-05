//! Factions. A player's faction is its entity id mod `FACTIONS`: the server
//! allocates ids per faction, so it costs no bytes on the wire, and every
//! client knows anyone's faction from the id it already has.

pub const FACTIONS: u16 = 3;

/// Full health, and what one life starts with.
pub const MAX_HEALTH: u8 = 100;

/// Game steps (1/30 s) between dying and respawning: 5 s.
pub const RESPAWN_STEPS: u32 = 150;

pub fn faction(entity: u16) -> u8 {
    (entity % FACTIONS) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_cycle_through_factions() {
        assert_eq!((0..6).map(faction).collect::<Vec<_>>(), [0, 1, 2, 0, 1, 2]);
        assert_eq!(faction(u16::MAX), (u16::MAX % 3) as u8);
    }
}
