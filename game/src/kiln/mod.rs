//! Kiln, the desert continent (docs/kiln.md), as its blockout (K0): the
//! whole continent in rough shapes, built to the design's numbers.
//!
//! Three rifts cut out of one crater run from the Hollow, at the center, most
//! of the way to the Rim, and split the map into three sectors, one per
//! faction: I, the Glasslands (north-east), II, the Dune sea (north-west),
//! III, the Dry Sea (south). The land rises toward the center: outer plains
//! (160-240 m), mesa country (flats near 240 m, mesas to 460 m), the Crown
//! (a plateau at 420 m), then the Hollow, a crater 1.6 km across and 340 m
//! deep with the Heartwell spire (610 m) in the middle. Each sector holds a
//! Landing, a Geode, a Cliffhold, six bases and twelve outposts; each rift a
//! Span. About 110 lanes join the 67 sites, every one walkable (the tests
//! walk them all) and the same length in every sector.
//!
//! Everything a sector has is laid out once, in the sector's own frame
//! (`Frame`: its axis along +x, the rift on its counterclockwise side at
//! +60 degrees), and turned into place, so the sectors are the same but for
//! their outer biomes. Bridges and decks are turned boxes (C1), square to
//! their rift.
//!
//! K1 (the collision kit): lifts carry players to the Heartwell's gallery,
//! the Geodes' crests, the Spans' decks from their pylons, the rift ledges'
//! rims and the Vent outposts' ledges, with drop shafts beside them for the
//! way down; each sector's two Tunnel mouths are joined by a tube under the
//! ground, down ramps through holes in it; each Landing's spawn room has
//! doorways shielded for its faction.
//!
//! K2 (the great sites in graybox, `deep`): the Hollow's Heart chamber under
//! the Heartwell with tunnels from the rift mouths and shafts from the floor,
//! lifts up its terraces; the Spans' galleries, arch perches and wall tunnels
//! up into their bridgeheads; the Cliffholds' lift towers, keep and deep
//! gallery; the Geodes' well chambers and lattice springs. What's still to
//! come (docs/kiln.md, build plan): the bases, outposts and the rest of the
//! Undervein (K3), the living world (K4) and art (K5).
//!
//! Generation uses f64 arithmetic and `libm` only (no platform math), in a
//! fixed order, so every machine builds the same bits.

mod build;
mod deep;
mod land;
mod lanes;
mod under;

use crate::world::{Continent, CoverBox, Lane, Parts, Point, Site, SiteKind, Volume, World, KILN, NO_SECTOR, TERRAIN_N, TERRAIN_RES};

pub use land::{CROWN, DECK, HOLLOW_FLOOR, MESA_FLATS, PLAIN, RIM_TOP, SALT};

/// The center of the map (and of the Hollow).
pub const CENTER: [f64; 2] = [4096.0, 4096.0];
/// The Rim: the outer 400 m of the square is out of play.
pub const RIM: f64 = 400.0;
/// Where the rifts end, from the center.
pub const RIFT_END: f64 = 3550.0;
/// The Hollow's crater: its rim's radius and its floor's.
pub const HOLLOW_R: f64 = 800.0;
pub const HOLLOW_FLOOR_R: f64 = 440.0;
/// The Crown's outer edge, where its escarpment drops to the mesa country.
pub const CROWN_R: f64 = 1400.0;
/// The mesa country's outer edge.
pub const MESA_R: f64 = 2600.0;

/// The steepest a lane's ramp climbs (rise over run): walkable (`MAX_SLOPE` is
/// 1) with room to spare; a future vehicle road wants half this.
pub const RAMP_GRADE: f64 = 0.33;
/// A ramp in a rift wall, to a ledge (infantry's): steeper.
pub const WALL_GRADE: f64 = 0.4;

/// The sectors' kinds of outer plain, in order: I, II, III.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    Glasslands,
    DuneSea,
    DrySea,
}

pub const BIOMES: [Biome; 3] = [Biome::Glasslands, Biome::DuneSea, Biome::DrySea];

/// Degrees to radians.
pub(crate) fn rad(deg: f64) -> f64 {
    deg * std::f64::consts::PI / 180.0
}

pub(crate) fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub(crate) fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

pub(crate) fn len(v: [f64; 2]) -> f64 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

pub(crate) fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

pub(crate) fn add(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

pub(crate) fn scale(a: [f64; 2], k: f64) -> [f64; 2] {
    [a[0] * k, a[1] * k]
}

pub(crate) fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

pub(crate) fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    len(sub(a, b))
}

/// The unit vector `deg` degrees counterclockwise from east.
pub(crate) fn unit(deg: f64) -> [f64; 2] {
    let (s, c) = libm::sincos(rad(deg));
    [c, s]
}

/// Distance from `p` to the segment `a`-`b`, and where along it (0-1) the
/// nearest point is.
pub(crate) fn seg_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, f64) {
    let ab = sub(b, a);
    let l2 = dot(ab, ab);
    let t = if l2 > 0.0 { (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (dist(p, add(a, scale(ab, t))), t)
}

/// A sector's frame: its axis (from the center out through its Landing) at
/// `30 + 120 k` degrees. Local points have the axis along +x and the sector's
/// counterclockwise rift at +60 degrees.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub k: usize,
    cos: f64,
    sin: f64,
}

impl Frame {
    pub fn new(k: usize) -> Self {
        let (sin, cos) = libm::sincos(rad(Self::axis_deg(k)));
        Self { k, cos, sin }
    }

    /// The sector's axis, degrees counterclockwise from east.
    pub fn axis_deg(k: usize) -> f64 {
        30.0 + 120.0 * k as f64
    }

    /// A local point in the world.
    pub fn world(&self, p: [f64; 2]) -> [f64; 2] {
        [CENTER[0] + p[0] * self.cos - p[1] * self.sin, CENTER[1] + p[0] * self.sin + p[1] * self.cos]
    }

    /// A world point in this frame.
    pub fn local(&self, p: [f64; 2]) -> [f64; 2] {
        let v = sub(p, CENTER);
        [v[0] * self.cos + v[1] * self.sin, -v[0] * self.sin + v[1] * self.cos]
    }

    /// The world point `r` m from the center at `deg` degrees from the axis.
    pub fn polar(&self, r: f64, deg: f64) -> [f64; 2] {
        self.world(scale(unit(deg), r))
    }

    /// The next sector's frame (counterclockwise).
    pub fn next(&self) -> Frame {
        Frame::new((self.k + 1) % 3)
    }

    /// The rift on this sector's counterclockwise side (between it and the
    /// next), and on its clockwise side.
    pub fn ccw_rift(&self) -> Rift {
        Rift::new(self.k)
    }

    pub fn cw_rift(&self) -> Rift {
        Rift::new((self.k + 2) % 3)
    }
}

/// Rift `j`, at `90 + 120 j` degrees: the border between sector `j` (on its
/// clockwise side) and sector `j + 1`.
#[derive(Debug, Clone, Copy)]
pub struct Rift {
    pub j: usize,
    /// Out from the center along it.
    pub dir: [f64; 2],
    /// Across it, toward sector `j + 1`.
    pub normal: [f64; 2],
}

impl Rift {
    pub fn new(j: usize) -> Self {
        let dir = unit(90.0 + 120.0 * j as f64);
        Self { j, dir, normal: [-dir[1], dir[0]] }
    }

    /// Half its floor's width at `along` m from the center: 300 where it opens
    /// into the Hollow (600 m wide), 225 at the Spans (450), 128 at its end.
    pub fn half_width(along: f64) -> f64 {
        300.0 - 0.0625 * (along.max(HOLLOW_FLOOR_R) - HOLLOW_R)
    }

    /// Its floor's height at `along`: 80 m where it meets the Hollow's floor,
    /// rising gently to 92 at its end.
    pub fn floor(along: f64) -> f64 {
        land::HOLLOW_FLOOR + 12.0 * ((along - HOLLOW_R) / (RIFT_END - HOLLOW_R)).clamp(0.0, 1.0)
    }

    /// (along, across) of a world point: across is positive toward sector `j + 1`.
    pub fn coords(&self, p: [f64; 2]) -> (f64, f64) {
        let v = sub(p, CENTER);
        (dot(v, self.dir), dot(v, self.normal))
    }

    /// The world point at `along`, `across`.
    pub fn at(&self, along: f64, across: f64) -> [f64; 2] {
        add(CENTER, add(scale(self.dir, along), scale(self.normal, across)))
    }

    /// The direction that crosses it square (toward sector `j + 1`): its
    /// normal, now that decks can be turned (C1); and how much longer a
    /// crossing that way is than a square one (none).
    pub fn crossing(&self) -> ([f64; 2], f64) {
        (self.normal, 1.0)
    }

    /// The points where a crossing centered on the axis at `along` meets its
    /// banks `out` m beyond its floor's edge: (on sector `j`'s side, on
    /// sector `j + 1`'s).
    pub fn crossing_ends(&self, along: f64, out: f64) -> ([f64; 2], [f64; 2]) {
        let (c, stretch) = self.crossing();
        let t = (Self::half_width(along) + out) * stretch;
        let mid = self.at(along, 0.0);
        (sub(mid, scale(c, t)), add(mid, scale(c, t)))
    }
}

/// A site as the generator plans it.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub kind: SiteKind,
    pub sector: u8,
    pub center: [f64; 2],
    pub half: f64,
    /// Its main ground's height, once the land is made.
    pub level: f64,
    /// Named places in it, by kind: a Span's [our bridgehead, the far one,
    /// the floor between its pylons]; a Cliffhold's [keep, floor gate]; ...
    pub anchors: Vec<[f64; 2]>,
    /// The directions (degrees) its lanes leave it in.
    pub dirs: Vec<f64>,
}

/// Sites in a sector, in this order (`site(k, i)`).
pub(crate) mod s {
    pub const LANDING: usize = 0;
    pub const GEODE: usize = 1;
    pub const SPAN: usize = 2;
    pub const CLIFFHOLD: usize = 3;
    /// Rift ledges: on the counterclockwise rift (near the Crown), and on the
    /// clockwise one (near the outer end).
    pub const LEDGE_CCW: usize = 4;
    pub const LEDGE_CW: usize = 5;
    pub const CITADEL: usize = 6;
    pub const SLOT_HOLD: usize = 7;
    /// The sector's two biome bases (Glass vaults, Cenotes, or an Ossuary and
    /// a Reef warren).
    pub const BIOME_A: usize = 8;
    pub const BIOME_B: usize = 9;
    /// Outpost n (1-12) is `O + n`.
    pub const O: usize = 9;
    pub const PER_SECTOR: usize = 22;
}

/// The Hollow is site 0; sector k's site i is `1 + 22 k + i`.
pub(crate) fn site(k: usize, i: usize) -> usize {
    1 + s::PER_SECTOR * (k % 3) + i
}

/// The rift ledges' heights (the ledge on the Crown's rift wall, the one in the
/// outer plains' wall).
pub(crate) const LEDGE_CCW_ALONG: f64 = 1361.0;
pub(crate) const LEDGE_CW_ALONG: f64 = 2700.0;
/// Half a rift ledge's shelf, along the rift.
pub(crate) const LEDGE_HALF: f64 = 66.0;
pub(crate) const SPAN_ALONG: f64 = 2100.0;
pub(crate) const CLIFFHOLD_ALONG: f64 = 3000.0;

/// The outposts, in a sector's frame: (r, degrees from the axis, kind).
pub(crate) const OUTPOSTS: [(f64, f64, SiteKind); 12] = [
    (1150.0, -30.0, SiteKind::SpirePerch),
    (1150.0, 22.0, SiteKind::Vent),
    (1700.0, -40.0, SiteKind::Arch),
    (1650.0, 38.0, SiteKind::WellShrine),
    (2650.0, -14.0, SiteKind::TunnelMouth),
    (2650.0, 12.0, SiteKind::Wreck),
    (3350.0, -44.0, SiteKind::Arch),
    (3400.0, 43.0, SiteKind::Vent),
    (3550.0, -20.0, SiteKind::SpirePerch),
    (3550.0, 18.0, SiteKind::WellShrine),
    (2350.0, -46.0, SiteKind::TunnelMouth),
    (2650.0, 44.0, SiteKind::Wreck),
];

/// The sector's biome bases' kinds.
pub(crate) fn biome_bases(k: usize) -> [SiteKind; 2] {
    match BIOMES[k] {
        Biome::Glasslands => [SiteKind::GlassVault, SiteKind::GlassVault],
        Biome::DuneSea => [SiteKind::Cenote, SiteKind::Cenote],
        Biome::DrySea => [SiteKind::Ossuary, SiteKind::ReefWarren],
    }
}

/// Every site's plan: the Hollow, then each sector's 22.
pub(crate) fn plan_sites() -> Vec<Plan> {
    let mut out = vec![Plan { kind: SiteKind::Hollow, sector: NO_SECTOR, center: CENTER, half: HOLLOW_R, level: land::HOLLOW_FLOOR, anchors: Vec::new(), dirs: Vec::new() }];
    for k in 0..3 {
        let f = Frame::new(k);
        let (ccw, cw) = (f.ccw_rift(), f.cw_rift());
        let sector = k as u8;
        let plan = |kind, center, half| Plan { kind, sector, center, half, level: 0.0, anchors: Vec::new(), dirs: Vec::new() };
        // A rift ledge: a shelf halfway down our wall, jutting a little over the floor.
        let ledge_ccw = ccw.at(LEDGE_CCW_ALONG, -(Rift::half_width(LEDGE_CCW_ALONG) + 6.0));
        let ledge_cw = cw.at(LEDGE_CW_ALONG, Rift::half_width(LEDGE_CW_ALONG) + 6.0);
        let [bio_a, bio_b] = biome_bases(k);
        let mut sites = vec![
            plan(SiteKind::Landing, f.polar(3300.0, 0.0), 150.0),
            plan(SiteKind::Geode, f.polar(2000.0, 0.0), 130.0),
            plan(SiteKind::Span, ccw.at(SPAN_ALONG, 0.0), 250.0),
            // In our wall: from the floor gate up seven terraces to the keep on the rim.
            plan(SiteKind::Cliffhold, ccw.at(CLIFFHOLD_ALONG, -(Rift::half_width(CLIFFHOLD_ALONG) + 40.0)), 150.0),
            plan(SiteKind::RiftLedge, ledge_ccw, 65.0),
            plan(SiteKind::RiftLedge, ledge_cw, 52.0),
            plan(SiteKind::MesaCitadel, f.polar(2250.0, -28.0), 90.0),
            plan(SiteKind::SlotHold, f.polar(2300.0, 24.0), 85.0),
            plan(bio_a, f.polar(3000.0, -30.0), 68.0),
            plan(bio_b, f.polar(2950.0, 28.0), 55.0),
        ];
        for &(r, deg, kind) in &OUTPOSTS {
            sites.push(plan(kind, f.polar(r, deg), 30.0));
        }
        // The Span: its bridgeheads' plateaus (90 m squares at the rift's
        // edge, along its crossing), and the floor under its deck.
        let (cross, _) = ccw.crossing();
        let (near, far) = ccw.crossing_ends(SPAN_ALONG, 6.0);
        sites[s::SPAN].anchors = vec![sub(near, scale(cross, 45.0)), add(far, scale(cross, 45.0)), ccw.at(SPAN_ALONG, 0.0)];
        // The Cliffhold: its summit keep's yard on the plains, its floor gate.
        let w = Rift::half_width(CLIFFHOLD_ALONG);
        sites[s::CLIFFHOLD].anchors = vec![ccw.at(CLIFFHOLD_ALONG + 60.0, -(w + 98.0 + 45.0)), ccw.at(CLIFFHOLD_ALONG, -(w - 30.0))];
        debug_assert_eq!(sites.len(), s::PER_SECTOR);
        out.extend(sites);
    }
    out
}

/// What the generator builds up.
pub(crate) struct Kiln {
    /// Heights in meters, `TERRAIN_N` × `TERRAIN_N`, row by row.
    pub h: Vec<f64>,
    pub ground: Vec<u8>,
    /// Per sample: `MASK_*` flags.
    pub mask: Vec<u8>,
    pub sites: Vec<Plan>,
    pub boxes: Vec<CoverBox>,
    /// Boxes lanes don't clear away: decks, stairs, towers.
    pub fixed: Vec<CoverBox>,
    pub lanes: Vec<Lane>,
    pub points: Vec<Point>,
    /// The routes inside sites (stamped as they're shaped).
    pub internal: Vec<lanes::Route>,
    /// Mesas and buttes placed (center, radius), for keeping scatter off them.
    pub mesas: Vec<([f64; 2], f64)>,
    /// Every lane's path in plan (world xy), for keeping mesas and boxes clear.
    pub corridors: Vec<Vec<[f64; 2]>>,
    /// Holes in the terrain, a bit per height cell (`World`'s layout).
    pub holes: Vec<u64>,
    /// Lifts, drop shafts and springs.
    pub volumes: Vec<Volume>,
    /// The Tunnel mouths' chambers, and the tunnels' lanes (kept apart from
    /// the rest: nothing on the ground over them is cleared for them).
    pub chambers: Vec<deep::Chamber>,
    pub tunnels: Vec<lanes::Route>,
    /// The tubes dug (K3).
    pub bores: Vec<deep::Bored>,
    /// The galleries' windows in the rifts' walls, the reef caves' doorways (K3).
    pub openings: Vec<under::Opening>,
    /// The bases' spawn rooms (K3): site, center (its shaft's corner), its
    /// long axis, its floor.
    pub rooms: Vec<(usize, [f64; 2], [f64; 2], f64)>,
    /// Every spawn room (`deep::room` with a shield), for the world (U4).
    pub spawn_rooms: Vec<crate::world::SpawnRoom>,
    /// What the generator couldn't fit (the tests want none).
    pub notes: Vec<String>,
    /// The clockwork's routes (K4): over a living bridge, down a sandfall,
    /// and when they can be walked (`world::BRIDGE`, `FALLS_A`, `FALLS_B`).
    pub clockwork: Vec<(lanes::Route, u8)>,
    /// Lips along edges (K4, `build::lip`), laid after the tunnels
    /// (`build::lay_lips`): boxes added sooner would steer their planning.
    pub(crate) lips: Vec<build::Lip>,
    /// Rails laid with them (a spiral road's parapet).
    pub(crate) rails: Vec<CoverBox>,
}

/// A road or a ramp is here.
pub(crate) const MASK_ROAD: u8 = 1;
/// A site's ground.
pub(crate) const MASK_PAD: u8 = 2;
/// Keep scattered cover off.
pub(crate) const MASK_CLEAR: u8 = 4;

impl Kiln {
    /// The samples within the world rectangle [min, max], as (x0..=x1, y0..=y1).
    pub fn samples(min: [f64; 2], max: [f64; 2]) -> (usize, usize, usize, usize) {
        let lo = |v: f64| ((v / TERRAIN_RES as f64).floor().max(0.0) as usize).min(TERRAIN_N - 1);
        let hi = |v: f64| ((v / TERRAIN_RES as f64).ceil().max(0.0) as usize).min(TERRAIN_N - 1);
        (lo(min[0]), hi(max[0]), lo(min[1]), hi(max[1]))
    }

    /// The world position of sample (ix, iy).
    pub fn pos(ix: usize, iy: usize) -> [f64; 2] {
        [ix as f64 * TERRAIN_RES as f64, iy as f64 * TERRAIN_RES as f64]
    }

    /// The height at (x, y), bilinear between samples (as `World::terrain`).
    pub fn height(&self, p: [f64; 2]) -> f64 {
        let res = TERRAIN_RES as f64;
        let last = (TERRAIN_N - 1) as f64;
        let fx = (p[0] / res).clamp(0.0, last);
        let fy = (p[1] / res).clamp(0.0, last);
        let (ix, iy) = ((fx as usize).min(TERRAIN_N - 2), (fy as usize).min(TERRAIN_N - 2));
        let (tx, ty) = (fx - ix as f64, fy - iy as f64);
        let at = |x: usize, y: usize| self.h[y * TERRAIN_N + x];
        let a = lerp(at(ix, iy), at(ix + 1, iy), tx);
        let b = lerp(at(ix, iy + 1), at(ix + 1, iy + 1), tx);
        lerp(a, b, ty)
    }

    /// The mean height over a disc (for a pad's level).
    pub fn mean(&self, c: [f64; 2], r: f64) -> f64 {
        let (mut sum, mut n) = (0.0, 0.0);
        let mut k = 0.0;
        while k < 1.0 {
            for i in 0..12 {
                let p = add(c, scale(unit(i as f64 * 30.0 + k * 15.0), r * k));
                sum += self.height(p);
                n += 1.0;
            }
            k += 0.25;
        }
        sum / n
    }
}

/// Builds Kiln.
pub fn generate() -> World {
    build_kiln().into_world()
}

/// Builds Kiln's parts (the tests look at them before they're a `World`).
pub(crate) fn build_kiln() -> Kiln {
    let t0 = std::time::Instant::now();
    let mut last = t0;
    let timing = std::env::var_os("KILN_TIMING").is_some();
    let mut tick = |what: &str| {
        if timing {
            eprintln!("kiln {what}: {:.0} ms", last.elapsed().as_secs_f64() * 1000.0);
        }
        last = std::time::Instant::now();
    };
    let mut kiln = Kiln {
        h: land::base(),
        ground: Vec::new(),
        mask: vec![0; TERRAIN_N * TERRAIN_N],
        sites: plan_sites(),
        boxes: Vec::new(),
        fixed: Vec::new(),
        lanes: Vec::new(),
        points: Vec::new(),
        internal: Vec::new(),
        mesas: Vec::new(),
        corridors: Vec::new(),
        holes: Vec::new(),
        volumes: Vec::new(),
        chambers: Vec::new(),
        tunnels: Vec::new(),
        bores: Vec::new(),
        openings: Vec::new(),
        rooms: Vec::new(),
        spawn_rooms: Vec::new(),
        notes: Vec::new(),
        clockwork: Vec::new(),
        lips: Vec::new(),
        rails: Vec::new(),
    };
    tick("base");
    // Where the lanes will run, before anything is placed beside them.
    let routes = lanes::plan(&kiln.sites);
    for r in &routes {
        let pts: Vec<[f64; 2]> = r.legs.iter().flat_map(|l| l.path.iter().copied()).collect();
        for (s, from_end) in [(r.a, false), (r.b, true)] {
            let c = kiln.sites[s].center;
            let found = if from_end { pts.iter().rev().find(|p| dist(**p, c) > 10.0) } else { pts.iter().find(|p| dist(**p, c) > 10.0) };
            if let Some(p) = found {
                let v = sub(*p, c);
                kiln.sites[s].dirs.push(libm::atan2(v[1], v[0]) * 180.0 / std::f64::consts::PI);
            }
        }
    }
    kiln.corridors = routes.iter().map(|r| r.legs.iter().flat_map(|l| l.path.iter().copied()).collect()).collect();
    land::features(&mut kiln); tick("land::features");
    land::sites(&mut kiln); tick("land::sites");
    build::hollow(&mut kiln); tick("build::hollow");
    let decks = lanes::stamp(&mut kiln, &routes); tick("lanes::stamp");
    build::sites(&mut kiln); tick("build::sites");
    build::spawn_rooms(&mut kiln); tick("build::spawn_rooms");
    build::living_bridges(&mut kiln); tick("build::living_bridges");
    under::build(&mut kiln); tick("under::build");
    build::lay_lips(&mut kiln); tick("build::lay_lips");
    build::sandfalls(&mut kiln); tick("build::sandfalls");
    build::scatter(&mut kiln); tick("build::scatter");
    // (A tunnel's lane only on the surface: to its mouths.)
    let paths: Vec<Vec<[f64; 2]>> = routes
        .iter()
        .chain(kiln.internal.iter())
        .chain(kiln.clockwork.iter().map(|(r, _)| r))
        .map(|r| r.legs.iter().flat_map(|l| l.path.iter().copied()).collect())
        .chain(kiln.tunnels.iter().flat_map(|r| r.legs.iter().filter(|l| l.z.is_none()).map(|l| l.path.clone())))
        .collect();
    build::clear_lanes(&mut kiln, &paths); tick("build::clear_lanes");
    land::ground(&mut kiln); tick("land::ground");
    let mut all = lanes::finish(&kiln, &routes, &decks);
    under::around_shafts(&mut kiln);
    all.extend(lanes::finish(&kiln, &kiln.tunnels, &[]));
    all.extend(lanes::finish(&kiln, &kiln.internal, &[]));
    let clockwork: Vec<lanes::Route> = kiln.clockwork.iter().map(|(r, _)| r.clone()).collect();
    for (mut l, &(_, when)) in lanes::finish(&kiln, &clockwork, &[]).into_iter().zip(&kiln.clockwork) {
        l.when = when;
        all.push(l);
    }
    kiln.lanes = all;
    kiln
}

impl Kiln {
    fn into_world(self) -> World {
        let mut kiln = self;
        let mut boxes = std::mem::take(&mut kiln.fixed);
        boxes.append(&mut kiln.boxes);
        kiln.boxes = boxes;
        kiln.parts()
    }

    fn parts(self) -> World {
    let kiln = self;
    let heights = kiln.h.iter().map(|&v| (v * 100.0).round().clamp(0.0, u16::MAX as f64) as u16).collect();
    let sites = kiln
        .sites
        .iter()
        .map(|p| Site {
            center: [p.center[0] as f32, p.center[1] as f32],
            half: p.half as f32,
            level: p.level as f32,
            base: p.kind.base(),
            kind: p.kind,
            sector: p.sector,
        })
        .collect();
    World::from_parts(Parts {
        id: KILN,
        continent: Continent::Kiln,
        heights,
        sites,
        boxes: kiln.boxes,
        ground: kiln.ground,
        lanes: kiln.lanes,
        points: kiln.points,
        holes: kiln.holes,
        volumes: kiln.volumes,
        rooms: kiln.spawn_rooms,
    })
    }
}

/// How fair the lanes are, from walking times (K0's bar).
#[derive(Debug, Clone)]
pub struct Fairness {
    /// Per sector: its Landing's shortest walk to the Hollow (s), and in how many lanes.
    pub hollow: Vec<(f32, usize)>,
    /// The worst, over every kind of site, of the slowest sector's walk from
    /// its Landing over the fastest's, and which site (`s` index) that is.
    pub spread: f32,
    pub worst_site: usize,
}

/// Fairness from the time (s) to walk each lane (`time(lane, backward)`;
/// None: not walked). Lanes are the world's routes between two sites.
pub fn fairness(world: &World, time: &dyn Fn(usize, bool) -> Option<f32>) -> Fairness {
    let n = world.sites().len();
    let mut edges: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    // (The lattice's own lanes: not the tunnels under them.)
    for (i, l) in world.lanes().iter().enumerate().filter(|(_, l)| l.a != l.b && !l.tunnel(world)) {
        if let Some(t) = time(i, false) {
            edges[l.a as usize].push((l.b as usize, t));
        }
        if let Some(t) = time(i, true) {
            edges[l.b as usize].push((l.a as usize, t));
        }
    }
    let shortest = |from: usize| {
        let mut best = vec![(f32::MAX, 0usize); n];
        best[from] = (0.0, 0);
        let mut heap = std::collections::BinaryHeap::new();
        heap.push((std::cmp::Reverse(0.0f32.to_bits()), from));
        while let Some((std::cmp::Reverse(t), u)) = heap.pop() {
            let t = f32::from_bits(t);
            if t > best[u].0 {
                continue;
            }
            for &(v, dt) in &edges[u] {
                if t + dt < best[v].0 {
                    best[v] = (t + dt, best[u].1 + 1);
                    heap.push((std::cmp::Reverse((t + dt).to_bits()), v));
                }
            }
        }
        best
    };
    let from: Vec<Vec<(f32, usize)>> = (0..3).map(|k| shortest(site(k, s::LANDING))).collect();
    let hollow = from.iter().map(|b| b[0]).collect();
    let (mut spread, mut worst_site) = (1.0f32, 0);
    for i in 0..s::PER_SECTOR {
        if i == s::LANDING {
            continue;
        }
        let t: Vec<f32> = (0..3).map(|k| from[k][site(k, i)].0).collect();
        let (lo, hi) = (t.iter().copied().fold(f32::MAX, f32::min), t.iter().copied().fold(0.0, f32::max));
        if hi / lo.max(1e-3) > spread {
            (spread, worst_site) = (hi / lo.max(1e-3), i);
        }
    }
    Fairness { hollow, spread, worst_site }
}

#[cfg(test)]
mod tests;
