//! What a client knows about other entities: the near tier's delta
//! baselines and each entity's latest update.

use std::collections::HashMap;

use lattice_game::delta::{self, NearHistory, NearQ};
use lattice_game::msg;
use lattice_game::tier::Tier;

/// What a tracking client knows about other entities.
#[derive(Debug, Default)]
pub struct Tracker {
    known: HashMap<u16, Known>,
    /// Near states by entity and tick: the baselines near deltas refer to.
    near: NearHistory,
    /// Update intervals per tier in server ticks, until drained.
    pub intervals: [Vec<u16>; 3],
    pub bad_blobs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Known {
    /// Server tick of the last update.
    pub tick: u32,
    pub tier: Tier,
    pub pos: [f32; 2],
}

impl Tracker {
    pub(crate) fn on_near(&mut self, data: &[u8]) -> Result<(), lattice_net::wire::DecodeError> {
        let mut got = Vec::new();
        let tick = delta::decode_near(data, &mut self.near, |e, q| got.push((e, q)))?;
        for (e, q) in got {
            self.record(e, Known { tick, tier: Tier::Near, pos: q.pos() });
        }
        Ok(())
    }

    pub(crate) fn on_update(&mut self, server_tick: u32, tier: Tier, blob: &[u8]) {
        let Ok((entity, pos, _)) = msg::decode_blob(blob) else {
            self.bad_blobs += 1;
            return;
        };
        self.record(entity, Known { tick: server_tick, tier, pos });
    }

    fn record(&mut self, entity: u16, now: Known) {
        let (server_tick, tier) = (now.tick, now.tier);
        if let Some(prev) = self.known.insert(entity, now) {
            let gap = server_tick.wrapping_sub(prev.tick);
            if gap > 0 && gap < u16::MAX as u32 {
                self.intervals[tier as usize].push(gap as u16);
            }
        }
    }

    /// The near state received for `entity` at `tick`, if still in history.
    pub fn near_state(&self, entity: u16, tick: u32) -> Option<NearQ> {
        self.near.get(entity, tick)
    }

    pub fn get(&self, entity: u16) -> Option<Known> {
        self.known.get(&entity).copied()
    }

    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}
