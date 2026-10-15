//! The weapons (GDD §4.5; a class's own, U6: `class`), and how their
//! projectiles fly. The server simulates every projectile with this; the
//! client flies its cosmetic tracers with it, so both draw the same arc.
//! The assault rifle's numbers are the module's constants (it came first,
//! and the load baselines are fought with it); the rest are `WEAPONS`'.
//!
//! Accuracy follows PlanetSide 2's model (an assault rifle's numbers): a
//! shot leaves somewhere in a *cone of fire*, tight aiming down sights, wide
//! from the hip, wider moving or in the air, and *bloom* widens it with each
//! shot of a burst. The server decides where every shot goes (`spread`, with
//! a secret pick per shot, so no client can predict it and aim to cancel it;
//! the shooter's own tracer leaves along a pick of its own, and everyone
//! else's tracers show the real shot). *Recoil* kicks the shooter's view; it's
//! the client's, since shots go where the view points.

use crate::ability::Ability;
use crate::movement::{MoveState, BUTTON_RELOAD, TICK_HZ};

/// Muzzle speed, m/s: ~20 m a step.
pub const MUZZLE_SPEED: f32 = 600.0;
pub const GRAVITY: f32 = 9.81;
/// Steps between shots: 10 rounds a second.
pub const FIRE_STEPS: u32 = 3;
/// A projectile flies this many steps (2 s, ~1.2 km) unless it hits something.
pub const RANGE_STEPS: u32 = 60;
/// Each step is flown in this many segments (10 m each at the muzzle).
pub const SUBSTEPS: u32 = 2;
pub const DAMAGE_BODY: u8 = 20;
pub const DAMAGE_HEAD: u8 = 40;
/// Eyes (and the muzzle) above the feet, standing and crouched.
pub const EYE_HEIGHT: f32 = 1.6;
pub const CROUCH_EYE_HEIGHT: f32 = 1.15;

/// The rifle's magazine and the rounds carried for it (refilled on
/// respawning), and how long a reload takes, in steps: 2.2 s, or 2.7 s from
/// empty (a round to chamber).
pub const MAGAZINE: u8 = 30;
pub const RESERVE: u16 = 180;
pub const RELOAD_STEPS: u8 = 66;
pub const RELOAD_EMPTY_STEPS: u8 = 81;

/// Eyes above the feet in state `s`.
pub fn eye_height(s: &MoveState) -> f32 {
    if s.crouched {
        CROUCH_EYE_HEIGHT
    } else {
        EYE_HEIGHT
    }
}

/// Cone of fire, degrees across its radius: aiming down sights or from the
/// hip, standing or moving (faster than `MOVING` m/s).
pub const CONE_ADS: f32 = 0.1;
pub const CONE_ADS_MOVING: f32 = 0.4;
pub const CONE_HIP: f32 = 2.0;
pub const CONE_HIP_MOVING: f32 = 2.75;
pub const MOVING: f32 = 1.0;
/// In the air the cone is this many times wider; crouched, this many times
/// (tighter).
pub const CONE_AIR: f32 = 2.5;
pub const CONE_CROUCH: f32 = 0.75;
/// Bloom: each shot widens the next one's cone by this much (degrees), up
/// to the max; it starts recovering `BLOOM_DELAY` steps after a shot, at
/// `BLOOM_RECOVERY` degrees a step.
pub const BLOOM_ADS: f32 = 0.04;
pub const BLOOM_HIP: f32 = 0.1;
pub const BLOOM_MAX_ADS: f32 = 0.5;
pub const BLOOM_MAX_HIP: f32 = 1.5;
pub const BLOOM_DELAY: f32 = 4.0;
pub const BLOOM_RECOVERY: f32 = 0.4;
/// Recoil (the client's view): each shot kicks it up `RECOIL_UP` degrees
/// (the first of a burst `RECOIL_FIRST` times that) and sideways within
/// `RECOIL_SIDE`, a little more to the right; `RECOIL_DELAY` s after the
/// last shot it drifts back down at `RECOIL_RECOVERY` degrees a second.
pub const RECOIL_UP: f32 = 0.32;
pub const RECOIL_FIRST: f32 = 1.75;
pub const RECOIL_SIDE: [f32; 2] = [-0.16, 0.22];
pub const RECOIL_DELAY: f32 = 0.12;
pub const RECOIL_RECOVERY: f32 = 9.0;
/// A shot this long (s) after the last starts a new burst.
pub const BURST_GAP: f32 = 0.35;
/// Aiming down sights: movement at this share of running speed (no sprint),
/// and the view zoomed this many times.
pub const ADS_SPEED: f32 = 0.5;
pub const ADS_ZOOM: f32 = 1.35;

/// A weapon's numbers (GDD §4.5, all to tune). Each class carries one
/// (`class`); its kit is its index in `WEAPONS`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weapon {
    pub name: &'static str,
    /// Muzzle speed, m/s.
    pub speed: f32,
    /// Time between shots, in 1/256 steps (a step is 1/30 s): 768 is 10 a second.
    pub interval: u32,
    /// Damage to the body and to the head up close, and at `falloff[1]` and
    /// beyond; between `falloff[0]` and `falloff[1]` (m from the muzzle) it
    /// falls in a line.
    pub damage: [u8; 2],
    pub far_damage: [u8; 2],
    pub falloff: [f32; 2],
    /// Projectiles a shot fires (a shotgun's pellets), spread over a cone of
    /// this many degrees round the shot's own direction.
    pub pellets: u8,
    pub pellet_cone: f32,
    /// Steps a projectile flies.
    pub range_steps: u32,
    pub magazine: u8,
    pub reserve: u16,
    pub reload_steps: u8,
    pub reload_empty_steps: u8,
    /// The cone of fire, degrees: down the sights standing and moving, from
    /// the hip standing and moving (`Bloom::cone`).
    pub cone: [f32; 4],
    /// Bloom a shot, down the sights and from the hip; and its most.
    pub bloom: [f32; 2],
    pub bloom_max: [f32; 2],
    /// Recoil (the client's view): up a shot, the first of a burst this many
    /// times as much, sideways within these.
    pub recoil_up: f32,
    pub recoil_first: f32,
    pub recoil_side: [f32; 2],
}

/// Weapon ids (`WEAPONS`); a class carries one (`class::WEAPONS_OF`).
pub const RIFLE: u8 = 0;
pub const CARBINE: u8 = 1;
pub const SHOTGUN: u8 = 2;
pub const MARKSMAN: u8 = 3;
pub const SMG: u8 = 4;
pub const LMG: u8 = 5;
pub const SNIPER: u8 = 6;
/// The Exo's arm weapons: twin anti-infantry guns, or a launcher of
/// explosive shells.
pub const EXO_GUNS: u8 = 7;
pub const EXO_LAUNCHER: u8 = 8;

/// The weapons, by id: the assault rifle, the carbine, the shotgun, the
/// marksman rifle, the SMG, the LMG, the sniper rifle, the Exo's guns and
/// launcher.
pub const WEAPONS: [Weapon; 9] = [
    Weapon {
        name: "assault rifle",
        speed: MUZZLE_SPEED,
        interval: FIRE_STEPS * 256,
        damage: [DAMAGE_BODY, DAMAGE_HEAD],
        far_damage: [DAMAGE_BODY, DAMAGE_HEAD],
        falloff: [f32::MAX, f32::MAX],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: RANGE_STEPS,
        magazine: MAGAZINE,
        reserve: RESERVE,
        reload_steps: RELOAD_STEPS,
        reload_empty_steps: RELOAD_EMPTY_STEPS,
        cone: [CONE_ADS, CONE_ADS_MOVING, CONE_HIP, CONE_HIP_MOVING],
        bloom: [BLOOM_ADS, BLOOM_HIP],
        bloom_max: [BLOOM_MAX_ADS, BLOOM_MAX_HIP],
        recoil_up: RECOIL_UP,
        recoil_first: RECOIL_FIRST,
        recoil_side: RECOIL_SIDE,
    },
    // The medic's: quicker, lighter, falling off past 10 m.
    Weapon {
        name: "carbine",
        speed: 550.0,
        interval: 640,
        damage: [18, 36],
        far_damage: [12, 24],
        falloff: [10.0, 50.0],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 50,
        magazine: 30,
        reserve: 210,
        reload_steps: 60,
        reload_empty_steps: 75,
        cone: [0.15, 0.45, 1.75, 2.4],
        bloom: [0.05, 0.12],
        bloom_max: [0.55, 1.6],
        recoil_up: 0.28,
        recoil_first: 1.6,
        recoil_side: [-0.18, 0.2],
    },
    // The engineer's: eight pellets, a kill up close, little past 30 m.
    Weapon {
        name: "shotgun",
        speed: 350.0,
        interval: 20 * 256,
        damage: [14, 20],
        far_damage: [4, 6],
        falloff: [8.0, 30.0],
        pellets: 8,
        pellet_cone: 3.0,
        range_steps: 25,
        magazine: 6,
        reserve: 36,
        reload_steps: 90,
        reload_empty_steps: 108,
        cone: [0.6, 1.0, 1.5, 2.0],
        bloom: [0.3, 0.5],
        bloom_max: [1.0, 1.5],
        recoil_up: 2.2,
        recoil_first: 1.0,
        recoil_side: [-0.5, 0.6],
    },
    // The recon's: four a second, three to the body or a head and a body.
    Weapon {
        name: "marksman rifle",
        speed: 750.0,
        interval: 1920,
        damage: [34, 68],
        far_damage: [34, 68],
        falloff: [f32::MAX, f32::MAX],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 70,
        magazine: 12,
        reserve: 72,
        reload_steps: 75,
        reload_empty_steps: 90,
        cone: [0.02, 0.6, 3.0, 4.0],
        bloom: [0.15, 0.4],
        bloom_max: [0.6, 2.0],
        recoil_up: 1.1,
        recoil_first: 1.0,
        recoil_side: [-0.12, 0.14],
    },
    // The skirmisher's: fifteen a second, good from the hip, weak past 25 m.
    Weapon {
        name: "SMG",
        speed: 400.0,
        interval: 512,
        damage: [15, 30],
        far_damage: [8, 16],
        falloff: [10.0, 25.0],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 35,
        magazine: 40,
        reserve: 240,
        reload_steps: 54,
        reload_empty_steps: 66,
        cone: [0.3, 0.5, 1.25, 1.75],
        bloom: [0.05, 0.08],
        bloom_max: [0.6, 1.2],
        recoil_up: 0.22,
        recoil_first: 1.4,
        recoil_side: [-0.2, 0.25],
    },
    // The assault's other: a hundred rounds, twelve a second, slow to reload,
    // wide from the hip.
    Weapon {
        name: "LMG",
        speed: 620.0,
        interval: 640,
        damage: [18, 36],
        far_damage: [15, 30],
        falloff: [40.0, 80.0],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 70,
        magazine: 100,
        reserve: 300,
        reload_steps: 120,
        reload_empty_steps: 150,
        cone: [0.25, 0.6, 2.2, 3.0],
        bloom: [0.06, 0.12],
        bloom_max: [0.9, 2.2],
        recoil_up: 0.3,
        recoil_first: 1.5,
        recoil_side: [-0.25, 0.3],
    },
    // The recon's other: a bolt-action rifle, a head in one, a body in two.
    Weapon {
        name: "sniper rifle",
        speed: 900.0,
        interval: 42 * 256,
        damage: [80, 200],
        far_damage: [80, 200],
        falloff: [f32::MAX, f32::MAX],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 90,
        magazine: 5,
        reserve: 30,
        reload_steps: 90,
        reload_empty_steps: 105,
        cone: [0.0, 1.0, 6.0, 8.0],
        bloom: [0.5, 1.0],
        bloom_max: [1.0, 4.0],
        recoil_up: 3.5,
        recoil_first: 1.0,
        recoil_side: [-0.3, 0.3],
    },
    // The Exo's twin guns: an arm each, twenty a second between them,
    // falling off past 15 m.
    Weapon {
        name: "twin guns",
        speed: 450.0,
        interval: 384,
        damage: [16, 32],
        far_damage: [9, 18],
        falloff: [15.0, 40.0],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 40,
        magazine: 60,
        reserve: 360,
        reload_steps: 90,
        reload_empty_steps: 105,
        cone: [0.8, 1.0, 1.2, 1.4],
        bloom: [0.04, 0.06],
        bloom_max: [1.5, 2.0],
        recoil_up: 0.15,
        recoil_first: 1.2,
        recoil_side: [-0.2, 0.2],
    },
    // The Exo's launcher: slow shells that burst (`gadget::SHELL`), two a
    // second, little direct damage.
    Weapon {
        name: "launcher",
        speed: 120.0,
        interval: 15 * 256,
        damage: [30, 40],
        far_damage: [30, 40],
        falloff: [f32::MAX, f32::MAX],
        pellets: 1,
        pellet_cone: 0.0,
        range_steps: 60,
        magazine: 8,
        reserve: 48,
        reload_steps: 105,
        reload_empty_steps: 120,
        cone: [0.3, 0.5, 0.8, 1.0],
        bloom: [0.2, 0.3],
        bloom_max: [1.0, 1.5],
        recoil_up: 1.6,
        recoil_first: 1.0,
        recoil_side: [-0.3, 0.3],
    },
];

/// Weapon `id` (an unknown one: the assault rifle).
pub fn of(id: u8) -> &'static Weapon {
    WEAPONS.get(id as usize).unwrap_or(&WEAPONS[0])
}

impl Weapon {
    /// A hit's damage `dist` m from the muzzle, to the head or not.
    pub fn damage_at(&self, dist: f32, head: bool) -> u8 {
        let h = head as usize;
        let t = ((dist - self.falloff[0]) / (self.falloff[1] - self.falloff[0]).max(1e-3)).clamp(0.0, 1.0);
        let (near, far) = (self.damage[h] as f32, self.far_damage[h] as f32);
        libm::roundf(near + (far - near) * t) as u8
    }

    /// Where pellet `k` of a shot along `dir` goes (`pick`: its own random
    /// bits), within `pellet_cone`.
    pub fn pellet(&self, dir: [f32; 3], pick: u64) -> [f32; 3] {
        let (yaw, pitch) = angles(dir);
        spread(yaw, pitch, self.pellet_cone, pick)
    }
}

/// A shooter's bloom. The server keeps one per player and the client one
/// for its own shots; fed the same shots in the same order, they give the
/// same cones. Of its weapon.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bloom {
    deg: f32,
    /// The last shot (`shot_time`).
    last: Option<u64>,
    pub weapon: u8,
}

impl Bloom {
    /// Fresh, for weapon `weapon`.
    pub fn with_weapon(weapon: u8) -> Self {
        Self { weapon, ..Self::default() }
    }

    /// The cone (degrees) of a shot at `time` (`shot_time`), aiming down
    /// sights or not, from `state` (the shooter before its step); then the
    /// shot blooms the next one.
    pub fn fire(&mut self, time: u64, ads: bool, state: &MoveState) -> f32 {
        let cone = self.cone(time, ads, state);
        let w = of(self.weapon);
        let (add, max) = if ads { (w.bloom[0], w.bloom_max[0]) } else { (w.bloom[1], w.bloom_max[1]) };
        self.deg = (self.recovered(time) + add).min(max);
        self.last = Some(time);
        cone
    }

    /// The cone a shot at `time` would have (the crosshair shows it).
    pub fn cone(&self, time: u64, ads: bool, state: &MoveState) -> f32 {
        let speed = (state.vel[0] * state.vel[0] + state.vel[1] * state.vel[1]).sqrt();
        let c = of(self.weapon).cone;
        let base = match (ads, speed > MOVING) {
            (true, false) => c[0],
            (true, true) => c[1],
            (false, false) => c[2],
            (false, true) => c[3],
        };
        let stance = if !state.grounded {
            CONE_AIR
        } else if state.crouched {
            CONE_CROUCH
        } else {
            1.0
        };
        (base + self.recovered(time)) * stance
    }

    fn recovered(&self, time: u64) -> f32 {
        match self.last {
            Some(last) if time > last => {
                let steps = (time - last) as f32 / 256.0;
                (self.deg - (steps - BLOOM_DELAY).max(0.0) * BLOOM_RECOVERY).max(0.0)
            }
            _ => self.deg,
        }
    }
}

/// Where a shot aimed at (`yaw`, `pitch`) with a `cone` (degrees) goes: the
/// point of the cone that `pick` (64 random bits) selects, uniform over its
/// disc. Unit vector. The server's picks are secret (keyed by a secret the
/// clients never see), so a client can't aim to cancel its spread or wait
/// for a lucky shot; a client's own tracer uses a pick of its own.
pub fn spread(yaw: u16, pitch: i16, cone: f32, pick: u64) -> [f32; 3] {
    let d = aim(yaw, pitch);
    if cone <= 0.0 {
        return d;
    }
    let h = pick;
    let (u1, u2) = ((h >> 40) as f32 / (1u64 << 24) as f32, (h & 0xFF_FFFF) as f32 / (1u64 << 24) as f32);
    let r = libm::tanf(cone.to_radians() * u1.sqrt());
    let (s, c) = libm::sincosf(u2 * std::f32::consts::TAU);
    // Right and up around the aim (straight up or down: any right will do).
    let flat = (d[0] * d[0] + d[1] * d[1]).sqrt();
    let right = if flat > 1e-4 { [d[1] / flat, -d[0] / flat, 0.0] } else { [1.0, 0.0, 0.0] };
    let up = [right[1] * d[2] - right[2] * d[1], right[2] * d[0] - right[0] * d[2], right[0] * d[1] - right[1] * d[0]];
    let v = [0, 1, 2].map(|k| d[k] + right[k] * r * c + up[k] * r * s);
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|x| x / len)
}

/// The aim (`yaw`, `pitch` in wire units) of a unit direction.
pub fn angles(d: [f32; 3]) -> (u16, i16) {
    let yaw = libm::atan2f(d[1], d[0]).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 65536.0;
    let pitch = libm::asinf(d[2].clamp(-1.0, 1.0)) / std::f32::consts::FRAC_PI_2 * 32767.0;
    (yaw as u32 as u16, pitch.clamp(-32767.0, 32767.0) as i16)
}

/// A shot as its input carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shot {
    /// When in its input's 1/30 s step it fired, in 1/256ths.
    pub frac: u8,
    /// The aim then (the input's own yaw and pitch are at the step's end).
    pub yaw: u16,
    pub pitch: i16,
    /// The near render step then (`msg::render_units`): what the shooter saw.
    pub render: u16,
}

/// A shot's time on the shooter's input timeline, in 1/256 steps: input
/// `seq` covers steps seq - 1 to seq.
pub fn shot_time(seq: u32, frac: u8) -> u64 {
    (seq as u64 - 1) * 256 + frac as u64
}

/// Unit aim vector (game space: x east, y north, z up).
pub fn aim(yaw: u16, pitch: i16) -> [f32; 3] {
    let y = yaw as f32 / 65536.0 * std::f32::consts::TAU;
    let p = pitch as f32 / 32767.0 * std::f32::consts::FRAC_PI_2;
    let (sy, cy) = libm::sincosf(y);
    let (sp, cp) = libm::sincosf(p);
    [cp * cy, cp * sy, sp]
}

/// Where shots leave from: the eyes, with the body (and its stance) between
/// two predicted states `frac` of the way (both sides compute it the same way).
pub fn muzzle(before: &MoveState, after: &MoveState, frac: u8) -> [f32; 3] {
    let t = frac as f32 / 256.0;
    let l = |a: f32, b: f32| a + (b - a) * t;
    [l(before.pos[0], after.pos[0]), l(before.pos[1], after.pos[1]), l(before.z, after.z) + l(eye_height(before), eye_height(after))]
}

/// A weapon's rounds: a magazine, a reserve, and a reload under way, and
/// whose (the kit: `of`). Both sides run it on each input's step in the
/// same order (`step`): the counts are part of the prediction, like movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ammo {
    pub mag: u8,
    pub reserve: u16,
    /// Steps until the reload under way is done; 0: none.
    pub reload: u8,
    /// The class (`class`) and the weapon it carries (`WEAPONS`).
    pub kit: u8,
    pub weapon: u8,
    /// The class's ability (U6), run by the same steps.
    pub ability: Ability,
}

impl Default for Ammo {
    fn default() -> Self {
        Self::FULL
    }
}

impl Ammo {
    /// As a player spawns with the assault rifle.
    pub const FULL: Ammo = Ammo { mag: MAGAZINE, reserve: RESERVE, reload: 0, kit: 0, weapon: RIFLE, ability: Ability { active: 0, cooldown: 0, energy: 0, held: false } };

    /// As a player spawns as class `kit` carrying `weapon` (and its ability ready).
    pub fn full(kit: u8, weapon: u8) -> Ammo {
        let w = of(weapon);
        Ammo { mag: w.magazine, reserve: w.reserve, reload: 0, kit, weapon, ability: Ability::fresh(kit) }
    }

    /// As a player spawns as class `kit` with its own weapon.
    pub fn of_class(kit: u8) -> Ammo {
        Self::full(kit, crate::class::WEAPONS_OF[crate::class::valid(kit) as usize][0])
    }

    /// Full rounds again (unlimited rounds' refill), the ability as it is.
    pub fn refilled(&self) -> Ammo {
        Ammo { ability: self.ability, ..Ammo::full(self.kit, self.weapon) }
    }

    /// Its weapon.
    pub fn arms(&self) -> &'static Weapon {
        of(self.weapon)
    }

    /// The least time between its shots now (`Weapon::interval`'s units): an
    /// Exo locked down fires faster.
    pub fn interval(&self) -> u32 {
        crate::ability::interval(self.kit, self.ability.on(), self.arms().interval)
    }

    /// What the ability does to movement in a step with `buttons`, taken before it.
    pub fn gear(&self, buttons: u8) -> crate::ability::Gear {
        self.ability.gear(self.kit, buttons)
    }

    /// One input's step: the reload counts down (and, done, fills the
    /// magazine from the reserve); a shot in the step fires if a round is
    /// ready (returns whether it did); then the reload button; then the
    /// ability. The last round starts a reload by itself.
    pub fn step(&mut self, buttons: u8, shot: bool) -> bool {
        self.tick();
        let fired = shot && self.ready();
        if fired {
            self.fire();
        }
        if buttons & BUTTON_RELOAD != 0 {
            self.start_reload();
        }
        self.ability.step(self.kit, buttons, fired);
        fired
    }

    /// The start of a step.
    pub fn tick(&mut self) {
        if self.reload > 0 {
            self.reload -= 1;
            if self.reload == 0 {
                let m = of(self.weapon).magazine;
                let take = (m - self.mag).min(self.reserve.min(m as u16) as u8);
                self.mag += take;
                self.reserve -= take as u16;
            }
        }
    }

    /// A round in the magazine and no reload under way.
    pub fn ready(&self) -> bool {
        self.mag > 0 && self.reload == 0
    }

    /// Whether a shot in the step `steps` from now (1: the next) would have
    /// a round ready: a reload under way may be done by then.
    pub fn ready_in(&self, steps: u32) -> bool {
        let mut a = *self;
        for _ in 0..steps.min(of(self.weapon).reload_empty_steps as u32 + 1) {
            a.tick();
        }
        a.ready()
    }

    /// A shot fired.
    pub fn fire(&mut self) {
        self.mag -= 1;
        if self.mag == 0 {
            self.start_reload();
        }
    }

    /// Starts a reload, unless the magazine is full, the reserve empty, or
    /// one is under way.
    pub fn start_reload(&mut self) {
        let w = of(self.weapon);
        if self.reload == 0 && self.mag < w.magazine && self.reserve > 0 {
            self.reload = if self.mag == 0 { w.reload_empty_steps } else { w.reload_steps };
        }
    }
}

/// A projectile in flight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flight {
    pub pos: [f32; 3],
    /// m/s.
    pub vel: [f32; 3],
}

impl Flight {
    /// The assault rifle's.
    pub fn new(origin: [f32; 3], dir: [f32; 3]) -> Self {
        Self::at(origin, dir, MUZZLE_SPEED)
    }

    /// At `speed` m/s.
    pub fn at(origin: [f32; 3], dir: [f32; 3], speed: f32) -> Self {
        Self { pos: origin, vel: dir.map(|d| d * speed) }
    }

    /// One segment: 1 / (`SUBSTEPS` × 30) s, semi-implicit Euler.
    pub fn advance(&self) -> Self {
        let dt = 1.0 / (SUBSTEPS * TICK_HZ) as f32;
        let vel = [self.vel[0], self.vel[1], self.vel[2] - GRAVITY * dt];
        let pos = [self.pos[0] + vel[0] * dt, self.pos[1] + vel[1] * dt, self.pos[2] + vel[2] * dt];
        Self { pos, vel }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aim_points_where_yaw_and_pitch_say() {
        let a = aim(0, 0);
        assert!((a[0] - 1.0).abs() < 1e-6 && a[1].abs() < 1e-6 && a[2].abs() < 1e-6, "east");
        let n = aim(16384, 0);
        assert!((n[1] - 1.0).abs() < 1e-4, "north: {n:?}");
        let up = aim(0, 32767);
        assert!((up[2] - 1.0).abs() < 1e-4, "up: {up:?}");
    }

    #[test]
    fn bullets_drop_about_1_2_m_over_300_m() {
        let mut f = Flight::new([0.0, 0.0, 100.0], [1.0, 0.0, 0.0]);
        let mut segments = 0;
        while f.pos[0] < 300.0 {
            f = f.advance();
            segments += 1;
        }
        let drop = 100.0 - f.pos[2];
        assert!((1.1..1.4).contains(&drop), "{drop} m after {segments} segments");
        assert_eq!(segments, 30, "10 m a segment");
    }

    #[test]
    fn the_cone_is_tight_aiming_wide_from_the_hip_and_blooms() {
        let stand = MoveState { grounded: true, ..Default::default() };
        let run = MoveState { vel: [6.0, 0.0], grounded: true, ..Default::default() };
        let air = MoveState { grounded: false, ..Default::default() };
        let b = Bloom::default();
        assert_eq!(b.cone(0, true, &stand), CONE_ADS);
        assert_eq!(b.cone(0, true, &run), CONE_ADS_MOVING);
        assert_eq!(b.cone(0, false, &stand), CONE_HIP);
        assert!(b.cone(0, false, &air) > b.cone(0, false, &run));
        // A burst at the rifle's rate blooms each shot, up to the max...
        let mut b = Bloom::default();
        let cones: Vec<f32> = (0..30).map(|k| b.fire(k * FIRE_STEPS as u64 * 256, true, &stand)).collect();
        assert!(cones.windows(2).all(|w| w[1] >= w[0]), "{cones:?}");
        assert!((cones[1] - CONE_ADS - BLOOM_ADS).abs() < 1e-6);
        assert!((cones[29] - CONE_ADS - BLOOM_MAX_ADS).abs() < 1e-6);
        // ...and a pause lets it recover.
        let later = 30 * FIRE_STEPS as u64 * 256 + 30 * 256;
        assert_eq!(b.cone(later, true, &stand), CONE_ADS);
    }

    #[test]
    fn shots_spread_uniformly_over_the_cone() {
        let (yaw, pitch) = (12000u16, 2000i16);
        let d = aim(yaw, pitch);
        // Picks from a 64-bit LCG (any well-spread bits will do).
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut far = 0;
        for _ in 0..2000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let s = spread(yaw, pitch, 2.0, x);
            assert_eq!(s, spread(yaw, pitch, 2.0, x), "the same pick: the same shot");
            let angle = libm::acosf((d[0] * s[0] + d[1] * s[1] + d[2] * s[2]).clamp(-1.0, 1.0)).to_degrees();
            assert!(angle <= 2.0 + 1e-3, "{angle}");
            far += (angle > 2.0 * std::f32::consts::FRAC_1_SQRT_2) as u32;
        }
        // Uniform over the disc: half the area is past radius / sqrt 2.
        assert!((900..1100).contains(&far), "{far} of 2000 in the outer half");
        assert_eq!(spread(yaw, pitch, 0.0, x), d, "no cone: straight down the aim");
        let (y, p) = angles(d);
        assert!((y as i32 - yaw as i32).abs() <= 1 && (p as i32 - pitch as i32).abs() <= 1);
    }

    #[test]
    fn crouching_tightens_the_cone_and_lowers_the_eyes() {
        let stand = MoveState { grounded: true, ..Default::default() };
        let crouch = MoveState { crouched: true, ..stand };
        let b = Bloom::default();
        assert_eq!(b.cone(0, false, &crouch), CONE_HIP * CONE_CROUCH);
        assert_eq!(b.cone(0, true, &crouch), CONE_ADS * CONE_CROUCH);
        assert_eq!(muzzle(&stand, &stand, 77)[2], EYE_HEIGHT);
        assert_eq!(muzzle(&crouch, &crouch, 0)[2], CROUCH_EYE_HEIGHT);
        let half = muzzle(&stand, &crouch, 128)[2];
        assert!(half < EYE_HEIGHT && half > CROUCH_EYE_HEIGHT, "between: {half}");
    }

    #[test]
    fn a_magazine_empties_reloads_by_itself_and_the_reserve_runs_out() {
        let mut a = Ammo::FULL;
        // Thirty shots, one per step: the last starts the (empty) reload.
        for k in 0..MAGAZINE {
            assert!(a.step(0, true), "shot {k}");
        }
        assert_eq!((a.mag, a.reload), (0, RELOAD_EMPTY_STEPS));
        assert!(!a.ready() && !a.ready_in(RELOAD_EMPTY_STEPS as u32 - 1) && a.ready_in(RELOAD_EMPTY_STEPS as u32));
        // Shots while reloading don't fire.
        for _ in 0..RELOAD_EMPTY_STEPS - 1 {
            assert!(!a.step(0, true));
        }
        assert!(a.step(0, true), "the reload done at the start of the step: it fires");
        assert_eq!((a.mag, a.reserve), (MAGAZINE - 1, RESERVE - MAGAZINE as u16));
        // A reload by the button: the shorter one, topping the magazine up.
        a.step(BUTTON_RELOAD, false);
        assert_eq!(a.reload, RELOAD_STEPS);
        for _ in 0..RELOAD_STEPS {
            a.step(BUTTON_RELOAD, false); // held: no second reload
        }
        assert_eq!((a.mag, a.reserve, a.reload), (MAGAZINE, RESERVE - MAGAZINE as u16 - 1, 0));
        // A full magazine doesn't reload; an empty reserve can't.
        a.step(BUTTON_RELOAD, false);
        assert_eq!(a.reload, 0);
        let mut dry = Ammo { mag: 2, reserve: 0, reload: 0, kit: 0, weapon: 0, ability: Ability::default() };
        assert!(dry.step(0, true) && dry.step(0, true) && !dry.step(0, true));
        assert_eq!((dry.mag, dry.reload), (0, 0), "nothing to reload with");
        // The last rounds of the reserve: a partial magazine.
        let mut low = Ammo { mag: 0, reserve: 7, reload: 1, kit: 0, weapon: 0, ability: Ability::default() };
        low.tick();
        assert_eq!((low.mag, low.reserve), (7, 0));
    }

    #[test]
    fn every_weapon_kills_and_the_shotgun_up_close() {
        for (k, w) in WEAPONS.iter().enumerate() {
            let k = k as u8;
            // Kill up close at its rate from a full magazine: in under a second
            // (the sniper rifle's second shot, a launcher's burst, take longer).
            let hits = 100u32.div_ceil(w.damage_at(0.0, false) as u32 * w.pellets as u32);
            let secs = (hits - 1) as f32 * w.interval as f32 / 256.0 / 30.0;
            let most = match k { SNIPER => 1.5, EXO_LAUNCHER => 2.0, _ => 0.8 };
            assert!(secs < most && hits <= w.magazine as u32, "{}: {hits} shots, {secs:.2} s", w.name);
            assert!(w.damage_at(1000.0, true) == w.far_damage[1] && w.damage_at(0.0, true) == w.damage[1]);
            let mut a = Ammo::full(0, k);
            for _ in 0..w.magazine {
                assert!(a.step(0, true), "{}", w.name);
            }
            assert_eq!(a.reload, w.reload_empty_steps, "{}: empty, a reload", w.name);
        }
        let sg = &WEAPONS[2];
        assert!(sg.damage_at(5.0, false) as u32 * sg.pellets as u32 >= 100, "a shotgun kills up close");
        assert!((sg.damage_at(19.0, false) as f32 - 9.0).abs() <= 1.0, "halfway between 14 and 4");
        // Pellets: within the cone round the shot, each its own.
        let d = aim(1000, 300);
        let mut x = 7u64;
        for _ in 0..200 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let p = sg.pellet(d, x);
            let angle = libm::acosf((d[0] * p[0] + d[1] * p[1] + d[2] * p[2]).clamp(-1.0, 1.0)).to_degrees();
            assert!(angle <= sg.pellet_cone + 0.05, "{angle}");
        }
        assert_eq!(of(99).name, "assault rifle");
    }

    #[test]
    fn shot_times_order_shots() {
        assert_eq!(shot_time(10, 0) + 3 * 256, shot_time(13, 0));
        assert!(shot_time(10, 255) < shot_time(11, 0));
    }
}
