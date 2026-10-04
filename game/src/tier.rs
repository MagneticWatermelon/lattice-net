//! Interest tiers: how often and how precisely a client hears about an entity.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Near = 0,
    Mid = 1,
    Far = 2,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Near, Tier::Mid, Tier::Far];

    pub fn from_u8(v: u8) -> Option<Tier> {
        Self::ALL.get(v as usize).copied()
    }
}
