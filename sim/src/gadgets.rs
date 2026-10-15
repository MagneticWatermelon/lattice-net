//! The server's gadgets (U6, `lattice_game::gadget`): the classes' tools
//! (G and X: frags, smoke, C4, mines, ammo packs, recon darts), the
//! engineer's turret (its ability), and the use key (E: heal, revive,
//! repair, hack). Run once a tick after the shots (`SimServer::gadgets_phase`);
//! what they do to players goes through `hurt` and `news` like a shot's.
//! What clients are told is `gadget::News`, one message a client a tick.

use lattice_game::clock::Live;
use lattice_game::events;
use lattice_game::faction::{faction, MAX_HEALTH};
use lattice_game::gadget::{self, Gadget, News};
use lattice_game::movement::{self, MoveState};
use lattice_game::{class, hit, weapon};

use super::{Event, SimServer};

/// Who a piece of news is for.
#[derive(Debug, Clone, Copy)]
pub(super) enum Scope {
    /// Every client (gadgets that stay: a joining one hears of them too).
    All,
    /// Clients whose players are within this of here.
    Near([f32; 3], f32),
    /// One player's client.
    To(u16),
}

/// How far a thrown thing's flight, a burst or a turret's fire is told.
const NEAR_NEWS: f32 = 500.0;

/// A tool key pressed (or the engineer's ability), as movement found it:
/// whose, which (0: G, 1: X, 2: the turret), at which step, from where and along what.
#[derive(Debug, Clone, Copy)]
pub(super) struct Press {
    pub slot: u8,
    pub step: u32,
    pub eye: [f32; 3],
    pub dir: [f32; 3],
}

/// The kinds that stay a while: every client is told of them.
fn lasting(kind: u8) -> bool {
    matches!(kind, gadget::C4 | gadget::MINE | gadget::AMMO_PACK | gadget::DART | gadget::TURRET)
}

/// A press seen by movement (`InputQueue::advance`): its eye and aim then.
pub(super) fn press(s: &MoveState, slot: u8, step: u32, yaw: u16, pitch: i16) -> Press {
    Press { slot, step, eye: [s.pos[0], s.pos[1], s.z + weapon::eye_height(s)], dir: weapon::aim(yaw, pitch) }
}

/// A life's tools, full (deploys, respawns).
pub(super) fn tools_full(kit: u8) -> [u8; 2] {
    [gadget::tool(kit, 0).1, gadget::tool(kit, 1).1]
}

/// No one's use key is on anything.
pub(super) const NO_TARGET: u32 = u32::MAX;
/// A use key's target that is a gadget: its id plus this.
const GADGET_TARGET: u32 = 0x1_0000;

impl SimServer {
    /// The tick's gadgets: presses, flight, fuses and triggers, ammo packs,
    /// darts' spotting, turrets, the use key; then the news.
    pub(super) fn gadgets_phase(&mut self) {
        // The presses movement saw, and who holds the use key.
        let mut presses = Vec::new();
        for e in 0..self.bodies.len() {
            let q = self.inputs[e].get_mut().unwrap();
            if !q.presses.is_empty() {
                presses.extend(q.presses.drain(..).map(|p| (e as u16, p)));
            }
            self.bodies[e].use_held = q.use_held;
        }
        presses.sort_by_key(|&(e, p)| (p.step, e));
        for (e, p) in presses {
            self.tool(e, p);
        }
        self.fly_gadgets();
        self.use_keys();
        if self.step >= self.spotted_step + 15 {
            self.spotted_step = self.step;
            self.spot();
        }
        self.post_gadget_news();
    }

    /// One press: a tool thrown or set down, C4 set off, a turret built.
    fn tool(&mut self, e: u16, p: Press) {
        let b = &self.bodies[e as usize];
        if !b.alive || b.dead() || b.away {
            return;
        }
        let (kit, side) = (b.kit, faction(e));
        if p.slot == 2 {
            if kit == class::ENGINEER {
                self.build_turret(e, p);
            }
            return;
        }
        let (kind, _) = gadget::tool(kit, p.slot as usize);
        if kind == gadget::NONE {
            return;
        }
        // C4 lying there: the key sets it off.
        if kind == gadget::C4 {
            let mine: Vec<usize> = (0..self.gadgets.len()).filter(|&i| self.gadgets[i].kind == gadget::C4 && self.gadgets[i].owner == e && self.gadgets[i].resting).collect();
            if !mine.is_empty() {
                for i in mine.into_iter().rev() {
                    let g = self.gadgets.swap_remove(i);
                    self.burst(&g, gadget::GONE_BURST);
                }
                return;
            }
        }
        let left = &mut self.bodies[e as usize].tools[p.slot as usize];
        if *left == 0 {
            return;
        }
        *left -= 1;
        // One ammo pack at a time.
        if kind == gadget::AMMO_PACK {
            self.retire(|g| g.kind == gadget::AMMO_PACK && g.owner == e, gadget::GONE_REPLACED);
        }
        let yaw = p.dir[1].atan2(p.dir[0]);
        let pitch = p.dir[2].clamp(-1.0, 1.0).asin();
        let id = self.next_gadget_id();
        let g = Gadget::thrown(id, kind, e, side, p.eye, yaw, pitch, p.step);
        self.counters.gadgets += 1;
        self.add_gadget(g);
    }

    fn next_gadget_id(&mut self) -> u32 {
        self.next_gadget = self.next_gadget.wrapping_add(1).max(1);
        self.next_gadget
    }

    fn add_gadget(&mut self, g: Gadget) {
        let scope = if lasting(g.kind) { Scope::All } else { Scope::Near(g.pos, NEAR_NEWS) };
        self.gadget_news.push((scope, News::Spawn(g)));
        self.gadgets.push(g);
    }

    /// Removes the gadgets `which` picks, telling everyone why.
    fn retire(&mut self, which: impl Fn(&Gadget) -> bool, why: u8) {
        let mut i = 0;
        while i < self.gadgets.len() {
            if which(&self.gadgets[i]) {
                let g = self.gadgets.swap_remove(i);
                let scope = if lasting(g.kind) { Scope::All } else { Scope::Near(g.pos, NEAR_NEWS) };
                self.gadget_news.push((scope, News::Gone { id: g.id, why }));
            } else {
                i += 1;
            }
        }
    }

    /// The engineer's turret: on the ground `TURRET_AHEAD` in front (where
    /// there's room for it: not into a wall), its last one gone.
    fn build_turret(&mut self, e: u16, p: Press) {
        let s = self.bodies[e as usize].state;
        let flat = (p.dir[0] * p.dir[0] + p.dir[1] * p.dir[1]).sqrt().max(1e-3);
        let (dx, dy) = (p.dir[0] / flat, p.dir[1] / flat);
        let want = [s.pos[0] + dx * gadget::TURRET_AHEAD, s.pos[1] + dy * gadget::TURRET_AHEAD];
        let live = Live::at(self.step);
        let side = faction(e);
        let mover = movement::Mover { faction: side, owners: &self.owners[0], live };
        // Room: a clear line from the engineer's chest to the spot's.
        let (from, to) = ([s.pos[0], s.pos[1], s.z + 1.0], [want[0], want[1], s.z + 1.0]);
        let at = if hit::line_clear(&self.world, from, to, mover) { want } else { s.pos };
        let z = self.world.ground(at[0], at[1], s.z, 1.0, live);
        self.retire(|g| g.kind == gadget::TURRET && g.owner == e, gadget::GONE_REPLACED);
        let id = self.next_gadget_id();
        let g = Gadget::placed(id, gadget::TURRET, e, side, [at[0], at[1], z], self.step);
        self.counters.gadgets += 1;
        self.add_gadget(g);
    }

    /// Every gadget's step: flight to now, fuses, triggers, ammo packs, turrets.
    fn fly_gadgets(&mut self) {
        let step = self.step;
        let mut gone: Vec<(usize, u8)> = Vec::new();
        let world = self.world.clone();
        for (i, g) in self.gadgets.iter_mut().enumerate() {
            g.step_to(&world, step);
            let age = step.saturating_sub(g.born);
            match g.kind {
                gadget::FRAG if age >= gadget::FRAG_FUSE => gone.push((i, gadget::GONE_BURST)),
                gadget::SMOKE if age >= gadget::SMOKE_FUSE + gadget::SMOKE_STEPS => gone.push((i, gadget::GONE_EXPIRED)),
                gadget::C4 if age >= gadget::C4_STEPS => gone.push((i, gadget::GONE_BURST)),
                gadget::MINE if age >= gadget::MINE_STEPS => gone.push((i, gadget::GONE_EXPIRED)),
                gadget::AMMO_PACK if age >= gadget::PACK_STEPS => gone.push((i, gadget::GONE_EXPIRED)),
                gadget::DART if age >= gadget::DART_STEPS => gone.push((i, gadget::GONE_EXPIRED)),
                _ => {}
            }
        }
        // Mines: an enemy's feet within the trigger, once armed.
        for (i, g) in self.gadgets.iter().enumerate() {
            if g.kind != gadget::MINE || !g.resting || step < g.born + gadget::MINE_ARM || gone.iter().any(|&(j, _)| j == i) {
                continue;
            }
            let mut hit = false;
            self.grid.for_each_within([g.pos[0], g.pos[1]], gadget::MINE_TRIGGER, |j, _, _| {
                let b = &self.bodies[j as usize];
                hit |= faction(j as u16) != g.faction && b.alive && !b.dead() && (b.state.z - g.pos[2]).abs() < 1.5;
            });
            if hit {
                gone.push((i, gadget::GONE_BURST));
            }
        }
        gone.sort_unstable_by_key(|g| std::cmp::Reverse(g.0));
        gone.dedup_by_key(|x| x.0);
        for (i, why) in gone {
            let g = self.gadgets.swap_remove(i);
            if why == gadget::GONE_BURST && gadget::blast(g.kind).is_some() {
                self.burst(&g, why);
            } else {
                // An ammo pack's done: its engineer may drop another.
                if g.kind == gadget::AMMO_PACK {
                    if let Some(b) = self.bodies.get_mut(g.owner as usize).filter(|b| b.kit == class::ENGINEER) {
                        b.tools[0] = b.tools[0].max(1);
                    }
                }
                let scope = if lasting(g.kind) { Scope::All } else { Scope::Near(g.pos, NEAR_NEWS) };
                self.gadget_news.push((scope, News::Gone { id: g.id, why }));
            }
        }
        // Ammo packs, on their beat.
        let beat = step / gadget::PACK_PERIOD;
        if beat != self.pack_beat {
            self.pack_beat = beat;
            let packs: Vec<(u8, [f32; 3])> = self.gadgets.iter().filter(|g| g.kind == gadget::AMMO_PACK && g.resting).map(|g| (g.faction, g.pos)).collect();
            for (side, at) in packs {
                let mut near = Vec::new();
                self.grid.for_each_within([at[0], at[1]], gadget::PACK_RADIUS, |j, _, _| near.push(j as u16));
                for j in near {
                    let b = &self.bodies[j as usize];
                    if faction(j) != side || !b.alive || b.dead() || (b.state.z - at[2]).abs() > 2.0 {
                        continue;
                    }
                    let q = self.inputs[j as usize].get_mut().unwrap();
                    let w = q.ammo.arms();
                    if q.ammo.reserve < w.reserve {
                        q.ammo.reserve = (q.ammo.reserve + w.magazine as u16).min(w.reserve);
                        let b = &mut self.bodies[j as usize];
                        b.supplies = b.supplies.wrapping_add(1);
                        self.counters.supplies += 1;
                    }
                }
            }
        }
        self.turrets();
    }

    /// A gadget bursting (`gadget::blast`): who its heart sees within its
    /// reach is hurt (not its side, but its thrower), turrets too.
    pub(super) fn burst(&mut self, g: &Gadget, why: u8) {
        self.blast(g.kind, g.pos, g.owner, g.faction);
        if g.kind != gadget::SHELL {
            let scope = if lasting(g.kind) { Scope::All } else { Scope::Near(g.pos, NEAR_NEWS) };
            self.gadget_news.push((scope, News::Gone { id: g.id, why }));
        }
    }

    /// A burst of `kind` at `pos`, set off by `owner` of `side`.
    pub(super) fn blast(&mut self, kind: u8, pos: [f32; 3], owner: u16, side: u8) {
        let Some((reach, _)) = gadget::blast(kind) else { return };
        self.counters.blasts += 1;
        self.gadget_news.push((Scope::Near(pos, NEAR_NEWS), News::Blast { kind, pos }));
        let live = Live::at(self.step);
        let heart = [pos[0], pos[1], pos[2] + 0.3];
        let mut hurt = Vec::new();
        {
            let mover = movement::Mover { faction: side, owners: &self.owners[0], live };
            self.grid.for_each_within([pos[0], pos[1]], reach, |j, _, _| {
                let j = j as u16;
                let b = &self.bodies[j as usize];
                if !b.alive || b.dead() || (faction(j) == side && j != owner) {
                    return;
                }
                let chest = [b.state.pos[0], b.state.pos[1], b.state.z + 1.0];
                let d = ((chest[0] - pos[0]).powi(2) + (chest[1] - pos[1]).powi(2) + (chest[2] - pos[2]).powi(2)).sqrt();
                let dmg = gadget::blast_damage(kind, d);
                if dmg > 0 && hit::line_clear(&self.world, heart, chest, mover) {
                    hurt.push((j, dmg));
                }
            });
        }
        let cause = if kind == gadget::SHELL { weapon::EXO_LAUNCHER } else { gadget::cause_of(kind) };
        for (j, dmg) in hurt {
            if let Some(killed) = self.hurt(j, dmg, owner, false, cause) {
                if self.bodies.get(owner as usize).is_some_and(|b| b.alive) {
                    self.news(owner, j, dmg, false, killed);
                }
            }
        }
        // Turrets: twice the damage (a blast is what kills them).
        let mut broken = Vec::new();
        for (i, t) in self.gadgets.iter_mut().enumerate() {
            if t.kind != gadget::TURRET || t.faction == side {
                continue;
            }
            let m = t.middle();
            let d = ((m[0] - pos[0]).powi(2) + (m[1] - pos[1]).powi(2) + (m[2] - pos[2]).powi(2)).sqrt();
            let dmg = gadget::blast_damage(kind, d) as u16 * 2;
            if dmg > 0 {
                t.health = t.health.saturating_sub(dmg);
                self.gadget_news.push((Scope::All, News::State { id: t.id, owner: t.owner, faction: t.faction, health: t.health }));
                if t.health == 0 {
                    broken.push(i);
                }
            }
        }
        for i in broken.into_iter().rev() {
            let t = self.gadgets.swap_remove(i);
            self.gadget_news.push((Scope::All, News::Gone { id: t.id, why: gadget::GONE_DESTROYED }));
        }
    }

    /// A shot hit turret `id` (`shots::Outcome::Gadget`) for `damage`.
    pub(super) fn shoot_turret(&mut self, id: u32, damage: u8, shooter: u16) {
        let Some(i) = self.gadgets.iter().position(|g| g.id == id) else { return };
        let t = &mut self.gadgets[i];
        t.health = t.health.saturating_sub(damage as u16);
        let (destroyed, health, owner, side) = (t.health == 0, t.health, t.owner, t.faction);
        self.gadget_news.push((Scope::All, News::State { id, owner, faction: side, health }));
        // (A hit marker; its end is the gadget's news, not a kill.)
        self.outbox.push((shooter, Event::Hit { target: events::NOBODY, damage, head: false, killed: false }));
        if destroyed {
            let t = self.gadgets.swap_remove(i);
            self.gadget_news.push((Scope::All, News::Gone { id: t.id, why: gadget::GONE_DESTROYED }));
        }
    }

    /// Turrets' fire: each, on its beat, at the nearest enemy its gun sees
    /// within range. A cloaked recon it doesn't see at all: the cloak is how
    /// a recon gets close enough to hack one.
    fn turrets(&mut self) {
        let step = self.step;
        let live = Live::at(step);
        let mut shots: Vec<(u32, u16, u16, [f32; 3], u8)> = Vec::new();
        for t in self.gadgets.iter() {
            if t.kind != gadget::TURRET || (step + t.id) % gadget::TURRET_PERIOD >= self.steps_this_tick {
                continue;
            }
            let gun = t.gun();
            let mover = movement::Mover { faction: t.faction, owners: &self.owners[0], live };
            let mut best: Option<(f32, u16, [f32; 3])> = None;
            self.grid.for_each_within([gun[0], gun[1]], gadget::TURRET_RANGE, |j, _, _| {
                let j = j as u16;
                let b = &self.bodies[j as usize];
                if faction(j) == t.faction || !b.alive || b.dead() {
                    return;
                }
                let chest = [b.state.pos[0], b.state.pos[1], b.state.z + if b.state.crouched { 0.8 } else { 1.1 }];
                let d = ((chest[0] - gun[0]).powi(2) + (chest[1] - gun[1]).powi(2) + (chest[2] - gun[2]).powi(2)).sqrt();
                let cloaked = b.ability && b.kit == class::RECON;
                if cloaked || best.is_some_and(|(bd, _, _)| bd <= d) {
                    return;
                }
                if hit::line_clear(&self.world, gun, chest, mover) {
                    best = Some((d, j, chest));
                }
            });
            if let Some((_, j, chest)) = best {
                shots.push((t.id, t.owner, j, chest, t.faction));
            }
        }
        for (id, owner, j, chest, _) in shots {
            let gun = self.gadgets.iter().find(|g| g.id == id).map(|g| g.gun()).unwrap_or(chest);
            let d = [chest[0] - gun[0], chest[1] - gun[1], chest[2] - gun[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-3);
            let (yaw, pitch) = weapon::angles(d.map(|v| v / len));
            let pick = self.rng.next_u64();
            let dir = weapon::spread(yaw, pitch, gadget::TURRET_CONE, pick);
            let (yaw, pitch) = weapon::angles(dir);
            self.gadget_news.push((Scope::Near(gun, NEAR_NEWS), News::Shot { id, yaw, pitch }));
            self.counters.turret_shots += 1;
            // A hit if the spread line still crosses its body.
            let b = &self.bodies[j as usize];
            let end = [gun[0] + dir[0] * gadget::TURRET_RANGE * 1.2, gun[1] + dir[1] * gadget::TURRET_RANGE * 1.2, gun[2] + dir[2] * gadget::TURRET_RANGE * 1.2];
            let feet = [b.state.pos[0], b.state.pos[1], b.state.z];
            if let Some((_, head)) = hit::player_sized(gun, end, feet, b.state.crouched, b.kit == class::EXO) {
                let dmg = if head { gadget::TURRET_DAMAGE * 2 } else { gadget::TURRET_DAMAGE };
                if let Some(killed) = self.hurt(j, dmg, owner, head, gadget::cause_of(gadget::TURRET)) {
                    if self.bodies.get(owner as usize).is_some_and(|b| b.alive) {
                        self.news(owner, j, dmg, head, killed);
                    }
                }
            }
        }
    }

    /// The use key, held: a medic heals a friend or revives one fallen; an
    /// engineer repairs a turret or an Exo of its side; a recon hacks an
    /// enemy's turret. The nearest such within `USE_RANGE` in front.
    fn use_keys(&mut self) {
        let step = self.step;
        let beat = step / gadget::USE_PERIOD != self.use_beat;
        self.use_beat = step / gadget::USE_PERIOD;
        let elapsed = self.steps_this_tick;
        for e in 0..self.bodies.len() {
            let b = self.bodies[e];
            if !b.alive || b.dead() || b.away || !b.use_held {
                if b.using != gadget::USING_NONE || b.use_target != NO_TARGET {
                    let b = &mut self.bodies[e];
                    (b.using, b.progress, b.use_target) = (gadget::USING_NONE, 0, NO_TARGET);
                }
                continue;
            }
            let e = e as u16;
            let side = faction(e);
            let eye = [b.state.pos[0], b.state.pos[1], b.state.z + weapon::eye_height(&b.state)];
            let dir = weapon::aim(b.yaw, b.pitch);
            let facing = |p: [f32; 3]| {
                let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                (len <= gadget::USE_RANGE + 1.0 && (len < 0.8 || (d[0] * dir[0] + d[1] * dir[1] + d[2] * dir[2]) / len > 0.55)).then_some(len)
            };
            // (what, target: an entity, or a gadget id + `GADGET_TARGET`)
            let mut pick: Option<(f32, u8, u32)> = None;
            let mut consider = |d: Option<f32>, what: u8, target: u32| {
                if let Some(d) = d {
                    if pick.is_none_or(|(bd, _, _)| d < bd) {
                        pick = Some((d, what, target));
                    }
                }
            };
            match b.kit {
                class::MEDIC => {
                    self.grid.for_each_within(b.state.pos, gadget::USE_RANGE + 1.0, |j, _, _| {
                        let o = &self.bodies[j as usize];
                        if j as u16 == e || faction(j as u16) != side || !o.alive || o.away {
                            return;
                        }
                        let at = [o.state.pos[0], o.state.pos[1], o.state.z + if o.dead() { 0.3 } else { 1.0 }];
                        if o.dead() {
                            consider(facing(at), gadget::USING_REVIVE, j);
                        } else if o.health < MAX_HEALTH && o.kit != class::EXO {
                            consider(facing(at), gadget::USING_HEAL, j);
                        }
                    });
                }
                class::ENGINEER => {
                    for g in &self.gadgets {
                        if g.kind == gadget::TURRET && g.faction == side && g.health < gadget::TURRET_HEALTH {
                            consider(facing(g.middle()), gadget::USING_REPAIR, GADGET_TARGET + g.id);
                        }
                    }
                    self.grid.for_each_within(b.state.pos, gadget::USE_RANGE + 1.0, |j, _, _| {
                        let o = &self.bodies[j as usize];
                        if j as u16 != e && faction(j as u16) == side && o.alive && !o.dead() && o.kit == class::EXO && o.health < MAX_HEALTH {
                            consider(facing([o.state.pos[0], o.state.pos[1], o.state.z + 1.0]), gadget::USING_REPAIR, j);
                        }
                    });
                }
                class::RECON => {
                    for g in &self.gadgets {
                        if g.kind == gadget::TURRET && g.faction != side {
                            consider(facing(g.middle()), gadget::USING_HACK, GADGET_TARGET + g.id);
                        }
                    }
                }
                _ => {}
            }
            let Some((_, what, target)) = pick else {
                let b = &mut self.bodies[e as usize];
                (b.using, b.progress, b.use_target) = (gadget::USING_NONE, 0, NO_TARGET);
                continue;
            };
            {
                let b = &mut self.bodies[e as usize];
                if b.using != what || b.use_target != target {
                    (b.using, b.progress, b.use_target) = (what, 0, target);
                }
                b.progress = b.progress.saturating_add(elapsed as u16);
            }
            let progress = self.bodies[e as usize].progress as u32;
            match what {
                gadget::USING_HEAL if beat => {
                    let o = &mut self.bodies[target as usize];
                    o.health = (o.health + gadget::HEAL).min(MAX_HEALTH);
                    self.counters.heals += 1;
                }
                gadget::USING_REPAIR if beat => {
                    if target >= GADGET_TARGET {
                        if let Some(t) = self.gadgets.iter_mut().find(|g| g.id == target - GADGET_TARGET) {
                            t.health = (t.health + gadget::REPAIR_TURRET).min(gadget::TURRET_HEALTH);
                            self.gadget_news.push((Scope::All, News::State { id: t.id, owner: t.owner, faction: t.faction, health: t.health }));
                        }
                    } else {
                        let o = &mut self.bodies[target as usize];
                        o.health = (o.health + gadget::REPAIR_EXO).min(MAX_HEALTH);
                    }
                    self.counters.repairs += 1;
                }
                gadget::USING_REVIVE if progress >= gadget::REVIVE_STEPS => {
                    self.revive(target as u16, e);
                    let b = &mut self.bodies[e as usize];
                    (b.using, b.progress, b.use_target) = (gadget::USING_NONE, 0, NO_TARGET);
                }
                gadget::USING_HACK if progress >= gadget::HACK_STEPS => {
                    if let Some(t) = self.gadgets.iter_mut().find(|g| g.id == target - GADGET_TARGET) {
                        (t.faction, t.owner) = (side, e);
                        self.gadget_news.push((Scope::All, News::State { id: t.id, owner: t.owner, faction: t.faction, health: t.health }));
                        self.counters.hacks += 1;
                    }
                    let b = &mut self.bodies[e as usize];
                    (b.using, b.progress, b.use_target) = (gadget::USING_NONE, 0, NO_TARGET);
                }
                _ => {}
            }
        }
    }

    /// `e`, fallen, stands again where it lies with `REVIVE_HEALTH`: a life
    /// event (its client rebases); its medic earns XP.
    fn revive(&mut self, e: u16, medic: u16) {
        let b = &mut self.bodies[e as usize];
        if !b.alive || !b.dead() || b.away {
            return;
        }
        (b.health, b.respawn_at, b.deploy) = (gadget::REVIVE_HEALTH, None, None);
        b.life = b.life.wrapping_add(1);
        self.damage_log[e as usize].clear();
        self.counters.revives += 1;
        self.reward(medic, events::REWARD_REVIVE, events::XP_REVIVE, e);
    }

    /// Who the darts see: every enemy within `DART_RADIUS` of a dart of its
    /// own side, told to each of that side's clients when it changes.
    fn spot(&mut self) {
        let mut seen: [Vec<u16>; 3] = Default::default();
        for g in self.gadgets.iter().filter(|g| g.kind == gadget::DART && g.resting) {
            self.grid.for_each_within([g.pos[0], g.pos[1]], gadget::DART_RADIUS, |j, _, _| {
                let b = &self.bodies[j as usize];
                if faction(j as u16) != g.faction && b.alive && !b.dead() && (b.state.z - g.pos[2]).abs() < gadget::DART_RADIUS {
                    seen[g.faction as usize].push(j as u16);
                }
            });
        }
        for (f, list) in seen.iter_mut().enumerate() {
            list.sort_unstable();
            list.dedup();
            list.truncate(255);
            if *list != self.spotted[f] {
                self.spotted[f] = list.clone();
                for e in 0..self.client_of.len() {
                    if self.client_of[e].is_some() && faction(e as u16) == f as u8 {
                        self.gadget_news.push((Scope::To(e as u16), News::Spotted(list.clone())));
                    }
                }
            }
        }
    }

    /// A client joining hears of every gadget that stays (and what its side spots).
    pub(super) fn gadgets_to(&mut self, e: u16) {
        for g in self.gadgets.iter().filter(|g| lasting(g.kind)) {
            self.gadget_news.push((Scope::To(e), News::Spawn(*g)));
        }
    }

    /// A player gone: its gadgets with it.
    pub(super) fn gadgets_gone(&mut self, e: u16) {
        self.retire(|g| g.owner == e, gadget::GONE_EXPIRED);
    }

    /// The tick's gadget news, one message a client.
    fn post_gadget_news(&mut self) {
        if self.gadget_news.is_empty() {
            return;
        }
        let news = std::mem::take(&mut self.gadget_news);
        self.reliable_out.resize_with(self.shard_count(), Vec::new);
        for e in 0..self.client_of.len() {
            let Some(client) = self.client_of[e] else { continue };
            let b = &self.bodies[e];
            let here = [b.state.pos[0], b.state.pos[1], b.state.z];
            let mine: Vec<News> = news
                .iter()
                .filter(|(s, _)| match *s {
                    Scope::All => true,
                    Scope::To(to) => to == e as u16,
                    Scope::Near(p, r) => !b.away && (here[0] - p[0]).powi(2) + (here[1] - p[1]).powi(2) <= r * r,
                })
                .map(|(_, n)| n.clone())
                .collect();
            for chunk in mine.chunks(255) {
                self.counters.gadget_news += chunk.len() as u64;
                self.reliable_out[self.net.shard_of_client(client)].push((client, gadget::encode_news(chunk)));
            }
        }
    }
}
