//! Gadgets as a client sees them (U6, `lattice_game::gadget`): each one the
//! server tells of (`News::Spawn`) flies on by the same `Gadget::step` as the
//! server's, to the render step, so its arc and its resting place are the
//! server's without being sent. Bursts and turrets' shots are effects to
//! show; the enemies our side's darts see are a list.

use lattice_game::gadget::{Gadget, News};
use lattice_game::weapon;
use lattice_game::world::World;

/// Effects kept at most until drained (a bot never drains them).
const MAX_FX: usize = 256;

/// A gadget, flown to the render step: its state at `g.at`, and where it
/// was the step before (it's drawn between the two).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tracked {
    pub g: Gadget,
    prev: [f32; 3],
    /// A turret's last shot: its aim (an input's yaw and pitch), and the
    /// render step it came at.
    pub aim: Option<(u16, i16)>,
    pub shot_at: f64,
}

/// Something a gadget did, to show (`Gadgets::drain_fx`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fx {
    /// A burst (`gadget::blast`): of what (`gadget::FRAG`, ... `SHELL`), where.
    Blast { kind: u8, pos: [f32; 3] },
    /// A turret firing: from its gun along `dir`; whose side.
    Shot { gun: [f32; 3], dir: [f32; 3], faction: u8 },
}

#[derive(Debug, Default)]
pub struct Gadgets {
    pub list: Vec<Tracked>,
    fx: Vec<Fx>,
    /// The enemies our side's darts see now.
    pub spotted: Vec<u16>,
    /// The render step they were last flown to.
    render: f64,
}

impl Gadgets {
    /// A gadgets message's news.
    pub fn on_news(&mut self, news: Vec<News>) {
        for n in news {
            match n {
                News::Spawn(g) => {
                    let t = Tracked { g, prev: g.pos, aim: None, shot_at: f64::NEG_INFINITY };
                    match self.list.iter_mut().find(|t| t.g.id == g.id) {
                        Some(o) => *o = t,
                        None => self.list.push(t),
                    }
                }
                News::Gone { id, .. } => self.list.retain(|t| t.g.id != id),
                News::Blast { kind, pos } => self.push_fx(Fx::Blast { kind, pos }),
                News::Shot { id, yaw, pitch } => {
                    let render = self.render;
                    if let Some(t) = self.list.iter_mut().find(|t| t.g.id == id) {
                        (t.aim, t.shot_at) = (Some((yaw, pitch)), render);
                        let fx = Fx::Shot { gun: t.g.gun(), dir: weapon::aim(yaw, pitch), faction: t.g.faction };
                        self.push_fx(fx);
                    }
                }
                News::State { id, owner, faction, health } => {
                    if let Some(t) = self.list.iter_mut().find(|t| t.g.id == id) {
                        (t.g.owner, t.g.faction, t.g.health) = (owner, faction, health);
                    }
                }
                News::Spotted(ids) => self.spotted = ids,
            }
        }
    }

    fn push_fx(&mut self, fx: Fx) {
        if self.fx.len() < MAX_FX {
            self.fx.push(fx);
        }
    }

    /// Flies every gadget on to the render step `render` (fractional).
    pub fn fly(&mut self, world: &World, render: f64) {
        self.render = self.render.max(render);
        let to = self.render.ceil().max(0.0) as u32;
        for t in &mut self.list {
            while t.g.at < to {
                t.prev = t.g.pos;
                t.g.step(world);
            }
        }
    }

    /// Where `t` is drawn at the render step: between its last two steps.
    pub fn drawn(&self, t: &Tracked) -> [f32; 3] {
        let k = (1.0 - (t.g.at as f64 - self.render)).clamp(0.0, 1.0) as f32;
        [0, 1, 2].map(|i| t.prev[i] + (t.g.pos[i] - t.prev[i]) * k)
    }

    /// The render step they were last flown to.
    pub fn render(&self) -> f64 {
        self.render
    }

    pub fn drain_fx(&mut self, out: &mut Vec<Fx>) {
        out.append(&mut self.fx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_game::gadget::{self, FRAG};

    #[test]
    fn a_gadget_flies_as_the_servers_and_is_drawn_between_steps() {
        let w = World::shared(lattice_game::world::KILN);
        let (x, y) = (2048.0f32, 2048.0f32);
        let z = w.terrain(x, y);
        let server = Gadget::thrown(9, FRAG, 3, 0, [x, y, z + 1.6], 0.3, 0.2, 100);
        let mut g = Gadgets::default();
        g.on_news(vec![News::Spawn(server)]);
        g.fly(&w, 104.5);
        let mut s = server;
        s.step_to(&w, 105);
        assert_eq!(g.list[0].g, s, "the same flight");
        let mut before = server;
        before.step_to(&w, 104);
        let d = g.drawn(&g.list[0]);
        let mid = [0, 1, 2].map(|i| (before.pos[i] + s.pos[i]) / 2.0);
        assert!((0..3).all(|i| (d[i] - mid[i]).abs() < 1e-4), "halfway: {d:?} vs {mid:?}");
        g.on_news(vec![News::Blast { kind: FRAG, pos: s.pos }, News::Gone { id: 9, why: gadget::GONE_BURST }]);
        let mut fx = Vec::new();
        g.drain_fx(&mut fx);
        assert_eq!((g.list.len(), fx.len()), (0, 1));
    }
}
