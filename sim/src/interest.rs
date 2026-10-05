//! Interest management: what each client receives, in three tiers.
//!
//! | tier | range     | rate                        | per tick | blob  |
//! |------|-----------|-----------------------------|----------|-------|
//! | near | 150 m     | 30 Hz, priority accumulator | 64       | 15 B  |
//! | mid  | 500 m     | 10 Hz, staggered by id      | ~86      | 11 B  |
//! | far  | 1500 m    | 2 Hz, staggered by id       | ~67      | 11 B  |
//!
//! **Mid and far are staggered by entity id alone.** An entity is due when
//! `id % period == tick % period`, so every client gets it on the same tick.
//! That keeps serialize-once cheap (only 1/3 and 1/15 of entities are due each
//! tick), needs no per-client state, and still spreads each client's load
//! evenly over ticks.
//!
//! **The near tier is a small accumulator.** Membership is distance *or*
//! interaction (squad now; hits, targeting and scope cones come with combat).
//! Up to `near_candidates` members are ranked each tick by
//! `base(distance, interaction) × ticks since last sent`, and the top
//! `near_per_tick` are sent. Per client that is one sorted array of at most
//! ~100 `(entity, last sent tick)` pairs.
//!
//! **Promotion.** An entity new to the near set has its age seeded from when it
//! was last sent, derived from its old tier's stagger slot (or "never"). A
//! stale one then outranks everything and is sent at once, so enemies closing
//! in don't stutter.
//!
//! **Budget.** Near fills first, then due mid, then due far. Due far entities
//! that don't fit are carried to the next tick and sent before newly due ones;
//! a far entity skipped twice in a row is a signal to degrade (shrink radii,
//! lower rates), never to cut into the near tier.

use crate::movement::TICK_HZ;

pub const MID_PERIOD: u32 = TICK_HZ / 10;
pub const FAR_PERIOD: u32 = TICK_HZ / 2;
/// Seeded age for an entity this client was never sent: it outranks everything.
pub const NEVER_SENT_AGE: u32 = 2 * FAR_PERIOD;

pub use lattice_game::tier::Tier;

#[derive(Debug, Clone)]
pub struct InterestConfig {
    pub near_radius: f32,
    /// Most entities ranked for the near tier; beyond it they're mid-tier.
    pub near_candidates: usize,
    pub near_per_tick: usize,
    pub mid_radius: f32,
    pub mid_per_tick: usize,
    pub far_radius: f32,
    pub far_per_tick: usize,
    /// Bytes of snapshot messages per client per tick (1.5 Mbps at 30 Hz).
    pub budget_bytes: usize,
    /// Squadmates are always near-tier, at any distance. 0 = no squads.
    pub squad_size: usize,
    /// Stagger periods in ticks (the degradation ladder lengthens them).
    pub mid_period: u32,
    pub far_period: u32,
}

impl Default for InterestConfig {
    fn default() -> Self {
        Self {
            near_radius: 150.0,
            near_candidates: 100,
            near_per_tick: 64,
            mid_radius: 500.0,
            mid_per_tick: 256_usize.div_ceil(MID_PERIOD as usize),
            far_radius: 1500.0,
            far_per_tick: 1000_usize.div_ceil(FAR_PERIOD as usize),
            budget_bytes: 1_500_000 / 8 / TICK_HZ as usize,
            squad_size: 4,
            mid_period: MID_PERIOD,
            far_period: FAR_PERIOD,
        }
    }
}

#[inline]
pub fn due(entity: u16, tick: u32, period: u32) -> bool {
    entity as u32 % period == tick % period
}

/// The most recent tick at or before `tick` on which `entity` is due.
#[inline]
pub fn last_due(entity: u16, tick: u32, period: u32) -> u32 {
    let back = (tick % period + period - entity as u32 % period) % period;
    tick.saturating_sub(back)
}

/// Priority weight before age: nearer is more urgent, interaction more so.
#[inline]
pub fn near_base(distance: f32, squad: bool) -> f32 {
    let b = 1.0 / (1.0 + distance / 20.0);
    if squad {
        b.max(0.5) * 4.0
    } else {
        b
    }
}

/// Age to seed for an entity entering a client's near set: ticks since the
/// client was last sent it, as far as the stagger schedule can tell.
pub fn seed_age(entity: u16, tick: u32, distance: f32, cfg: &InterestConfig, fresh_client: bool) -> u32 {
    let prev = tick.saturating_sub(1);
    if fresh_client {
        NEVER_SENT_AGE
    } else if distance <= cfg.mid_radius {
        tick - last_due(entity, prev, cfg.mid_period)
    } else if distance <= cfg.far_radius {
        tick - last_due(entity, prev, cfg.far_period)
    } else {
        NEVER_SENT_AGE
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NearCandidate {
    pub entity: u16,
    pub base: f32,
    /// Used if the entity wasn't in the near set last tick.
    pub seed_age: u32,
}

/// Per-thread scratch for `NearState::select`: an entity-indexed lookup of
/// last-sent ticks, so matching candidates to last tick's set needs no sort.
#[derive(Debug, Default)]
pub struct SelectScratch {
    sent: Vec<u32>,
    base: Vec<u32>,
    mark: Vec<u32>,
    epoch: u32,
    rank: Vec<(f32, usize)>,
}

/// One client's near-tier accumulator: its near set and when each was last sent.
#[derive(Debug, Default, Clone)]
pub struct NearState {
    /// (entity, last sent tick, acked baseline tick or 0), in candidate order.
    entries: Vec<(u16, u32, u32)>,
    /// Last tick's entries, reused as the next buffer (no allocation per tick).
    spare: Vec<(u16, u32, u32)>,
    started: bool,
}

impl NearState {
    /// False until the first `select` (every candidate then counts as never sent).
    pub fn started(&self) -> bool {
        self.started
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entities in the near set.
    pub fn entities(&self) -> impl Iterator<Item = u16> + '_ {
        self.entries.iter().map(|e| e.0)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn last_sent(&self, entity: u16) -> Option<u32> {
        self.entries.iter().find(|e| e.0 == entity).map(|e| e.1)
    }

    /// The near set: (entity, last sent tick, acked baseline tick or 0).
    pub fn entries(&self) -> &[(u16, u32, u32)] {
        &self.entries
    }

    /// The newest tick of `entity`'s state the client is known to have (0 = none).
    pub fn baseline(&self, entity: u16) -> Option<u32> {
        self.entries.iter().find(|e| e.0 == entity).map(|e| e.2)
    }

    /// The client acked these (entity, tick) states: they become baselines
    /// for entities still in the near set (a newer one wins).
    pub fn apply_acks(&mut self, acks: &[(u16, u32)], sc: &mut SelectScratch) {
        if acks.is_empty() {
            return;
        }
        sc.begin();
        for (i, &(e, _, _)) in self.entries.iter().enumerate() {
            sc.index(e, i as u32, 0);
        }
        for &(e, tick) in acks {
            if let Some(i) = sc.lookup(e) {
                let b = &mut self.entries[i.0 as usize].2;
                *b = (*b).max(tick);
            }
        }
    }

    /// Makes `cands` the near set and appends the up-to-`per_tick` entities to
    /// send this tick to `out` (in no particular order), each with its acked
    /// baseline tick (0 = none: send it whole).
    pub fn select(
        &mut self,
        cands: &[NearCandidate],
        tick: u32,
        per_tick: usize,
        sc: &mut SelectScratch,
        out: &mut Vec<(u16, u32)>,
    ) {
        sc.begin();
        for &(e, sent, base) in &self.entries {
            sc.index(e, sent, base);
        }
        let mut next = std::mem::take(&mut self.spare);
        next.clear();
        sc.rank.clear();
        for (i, c) in cands.iter().enumerate() {
            let (sent, base) = sc.lookup(c.entity).unwrap_or((tick.saturating_sub(c.seed_age), 0));
            next.push((c.entity, sent, base));
            let age = tick.saturating_sub(sent).max(1);
            sc.rank.push((c.base * age as f32, i));
        }
        let n = per_tick.min(sc.rank.len());
        if n < sc.rank.len() {
            sc.rank.select_nth_unstable_by(n, |a, b| b.0.total_cmp(&a.0));
        }
        for &(_, i) in &sc.rank[..n] {
            next[i].1 = tick;
            out.push((next[i].0, next[i].2));
        }
        self.spare = std::mem::replace(&mut self.entries, next);
        self.started = true;
    }
}

impl SelectScratch {
    fn begin(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.mark.fill(0);
            self.epoch = 1;
        }
    }

    fn index(&mut self, e: u16, a: u32, b: u32) {
        let i = e as usize;
        if self.mark.len() <= i {
            self.mark.resize(i + 1, 0);
            self.sent.resize(i + 1, 0);
            self.base.resize(i + 1, 0);
        }
        self.mark[i] = self.epoch;
        self.sent[i] = a;
        self.base[i] = b;
    }

    fn lookup(&self, e: u16) -> Option<(u32, u32)> {
        let i = e as usize;
        (self.mark.get(i) == Some(&self.epoch)).then(|| (self.sent[i], self.base[i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(entity: u16, distance: f32) -> NearCandidate {
        NearCandidate { entity, base: near_base(distance, false), seed_age: NEVER_SENT_AGE }
    }

    #[test]
    fn stagger_spreads_entities_evenly_and_last_due_is_consistent() {
        for period in [MID_PERIOD, FAR_PERIOD] {
            for tick in 100..100 + period {
                let n = (0..3000u16).filter(|&e| due(e, tick, period)).count();
                assert_eq!(n, 3000 / period as usize);
            }
            for e in 0..50u16 {
                for tick in 30..90 {
                    let t = last_due(e, tick, period);
                    assert!(due(e, t, period) && t <= tick && tick - t < period);
                }
            }
        }
        assert_eq!(last_due(7, 2, FAR_PERIOD), 0, "saturates near tick 0");
    }

    #[test]
    fn under_capacity_everyone_is_sent_every_tick() {
        let mut s = NearState::default();
        let (mut sc, mut out) = (SelectScratch::default(), Vec::new());
        for tick in 1..10 {
            let c: Vec<_> = (0..40).map(|e| cand(e, e as f32 * 3.0)).collect();
            out.clear();
            s.select(&c, tick, 64, &mut sc, &mut out);
            assert_eq!(out.len(), 40);
        }
        assert_eq!(s.last_sent(39), Some(9));
    }

    #[test]
    fn over_capacity_everyone_is_sent_and_nearer_ones_more_often() {
        let mut s = NearState::default();
        let (mut sc, mut out) = (SelectScratch::default(), Vec::new());
        let mut sends = [0u32; 100];
        for tick in 1..=300 {
            let c: Vec<_> = (0..100).map(|e| cand(e, e as f32 * 1.5)).collect();
            out.clear();
            s.select(&c, tick, 64, &mut sc, &mut out);
            assert_eq!(out.len(), 64);
            out.iter().for_each(|&(e, _)| sends[e as usize] += 1);
        }
        assert!(sends.iter().all(|&n| n > 0), "the accumulator starves no one: {sends:?}");
        assert!(sends[0] > 2 * sends[99], "nearest {} vs farthest {}", sends[0], sends[99]);
        // Every member is at most a few ticks stale.
        let stalest = (0..100).map(|e| 300 - s.last_sent(e).unwrap()).max().unwrap();
        assert!(stalest <= 6, "stalest near entity is {stalest} ticks old");
    }

    #[test]
    fn a_stale_newcomer_is_sent_at_once() {
        let mut s = NearState::default();
        let (mut sc, mut out) = (SelectScratch::default(), Vec::new());
        // A full house of close, fresh entities...
        for tick in 1001..=1005 {
            let c: Vec<_> = (0..64).map(|e| cand(e, 1.0)).collect();
            out.clear();
            s.select(&c, tick, 64, &mut sc, &mut out);
        }
        // ...then entity 500 walks in from the far tier, last sent 14 ticks ago.
        let mut c: Vec<_> = (0..64).map(|e| cand(e, 1.0)).collect();
        c.push(NearCandidate { entity: 500, base: near_base(149.0, false), seed_age: 14 });
        out.clear();
        s.select(&c, 1006, 64, &mut sc, &mut out);
        assert!(out.iter().any(|o| o.0 == 500), "the stale newcomer outranks a fresh member");
        assert_eq!(s.last_sent(500), Some(1006));
        assert_eq!(s.len(), 65);
    }

    #[test]
    fn acks_become_baselines_and_survive_reselection() {
        let mut s = NearState::default();
        let (mut sc, mut out) = (SelectScratch::default(), Vec::new());
        let c: Vec<_> = (0..10).map(|e| cand(e, 5.0)).collect();
        s.select(&c, 100, 64, &mut sc, &mut out);
        assert!(out.iter().all(|&(_, b)| b == 0), "no baselines yet: {out:?}");
        // The client acked tick 100's message, which carried entities 0..5.
        s.apply_acks(&(0..5).map(|e| (e, 100)).collect::<Vec<_>>(), &mut sc);
        s.apply_acks(&[(3, 90), (77, 100)], &mut sc); // older ack and a stranger: ignored
        out.clear();
        s.select(&c, 101, 64, &mut sc, &mut out);
        for &(e, b) in &out {
            assert_eq!(b, if e < 5 { 100 } else { 0 }, "entity {e}");
        }
        // An entity that leaves the near set loses its baseline.
        out.clear();
        s.select(&c[1..], 102, 64, &mut sc, &mut out);
        s.select(&c, 103, 64, &mut sc, &mut out);
        assert_eq!(s.baseline(0), Some(0));
        assert_eq!(s.baseline(1), Some(100));
    }

    #[test]
    fn seed_age_follows_the_old_tiers_schedule() {
        let cfg = InterestConfig::default();
        let tick = 1000;
        for e in 0..30u16 {
            let mid = seed_age(e, tick, 300.0, &cfg, false);
            assert!((1..=MID_PERIOD).contains(&mid));
            assert!(due(e, tick - mid, MID_PERIOD));
            let far = seed_age(e, tick, 900.0, &cfg, false);
            assert!((1..=FAR_PERIOD).contains(&far));
            assert!(due(e, tick - far, FAR_PERIOD));
        }
        assert_eq!(seed_age(3, tick, 5000.0, &cfg, false), NEVER_SENT_AGE);
        assert_eq!(seed_age(3, tick, 10.0, &cfg, true), NEVER_SENT_AGE, "a new client has seen nothing");
    }
}
