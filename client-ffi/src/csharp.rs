//! The C# bindings, generated: the game's constants, the codes, every
//! exported struct with its exact layout (explicit field offsets from
//! `offset_of!`), and a table of function pointers (`LtApi`) a loader binds
//! with `GetProcAddress`. Nothing here is typed twice: the structs and
//! signatures come from `abi::STRUCTS` and `api::FNS`, the constants from
//! the crates that define them.

use std::fmt::Write;

use lattice_client_core::{entities, tracers};
use lattice_game::{clock, faction, hit, movement, msg, weapon, world};

use crate::abi::{layout_hash, normalize, CODES, STRUCTS};
use crate::api::{FNS, VERSION};

/// A constant's value, as C# writes it.
enum Value {
    F32(f32),
    F64(f64),
    Int(i64, &'static str),
}

fn constants() -> Vec<(&'static str, Value, &'static str)> {
    use Value::*;
    vec![
        ("TICK_HZ", Int(movement::TICK_HZ as i64, "int"), "Game steps a second."),
        ("DT", F32(movement::DT), "One game step, s."),
        ("WORLD_SIZE", F32(movement::WORLD_SIZE), "The continent's edge, m."),
        ("RUN_SPEED", F32(movement::RUN_SPEED), "m/s."),
        ("SPRINT_SPEED", F32(movement::SPRINT_SPEED), "m/s."),
        ("JUMP_SPEED", F32(movement::JUMP_SPEED), "m/s."),
        ("MAX_SLOPE", F32(movement::MAX_SLOPE), "Rise over run a player can walk up."),
        ("STEP_UP", F32(movement::STEP_UP), "Highest ledge a walking player steps onto, m."),
        ("RADIUS", F32(movement::RADIUS), "The movement collider: a cylinder, m."),
        ("HEIGHT", F32(movement::HEIGHT), "m."),
        ("CROUCH_SPEED", F32(movement::CROUCH_SPEED), "Crouched: m/s,"),
        ("CROUCH_HEIGHT", F32(movement::CROUCH_HEIGHT), "the collider, m,"),
        ("CROUCH_EYE_HEIGHT", F32(weapon::CROUCH_EYE_HEIGHT), "and the eyes above the feet, m."),
        ("EYE_HEIGHT", F32(weapon::EYE_HEIGHT), "Eyes (and the muzzle) above the feet, m."),
        ("MUZZLE_SPEED", F32(weapon::MUZZLE_SPEED), "The rifle's projectile, m/s."),
        ("GRAVITY", F32(weapon::GRAVITY), "On projectiles, m/s²."),
        ("FIRE_STEPS", Int(weapon::FIRE_STEPS as i64, "int"), "Steps between shots."),
        ("RANGE_STEPS", Int(weapon::RANGE_STEPS as i64, "int"), "Steps a projectile flies."),
        ("SUBSTEPS", Int(weapon::SUBSTEPS as i64, "int"), "Segments a projectile flies per step."),
        ("DAMAGE_BODY", Int(weapon::DAMAGE_BODY as i64, "int"), ""),
        ("DAMAGE_HEAD", Int(weapon::DAMAGE_HEAD as i64, "int"), ""),
        ("CONE_ADS", F32(weapon::CONE_ADS), "Cone of fire, degrees: down the sights, standing."),
        ("CONE_ADS_MOVING", F32(weapon::CONE_ADS_MOVING), ""),
        ("CONE_HIP", F32(weapon::CONE_HIP), ""),
        ("CONE_HIP_MOVING", F32(weapon::CONE_HIP_MOVING), ""),
        ("CONE_AIR", F32(weapon::CONE_AIR), "In the air the cone is this many times wider;"),
        ("CONE_CROUCH", F32(weapon::CONE_CROUCH), "crouched, this many times."),
        ("MAGAZINE", Int(weapon::MAGAZINE as i64, "int"), "Rounds in the rifle's magazine,"),
        ("RESERVE", Int(weapon::RESERVE as i64, "int"), "and carried for it (refilled on respawning)."),
        ("RELOAD_STEPS", Int(weapon::RELOAD_STEPS as i64, "int"), "A reload, steps;"),
        ("RELOAD_EMPTY_STEPS", Int(weapon::RELOAD_EMPTY_STEPS as i64, "int"), "from empty (a round to chamber)."),
        ("RECOIL_UP", F32(weapon::RECOIL_UP), "Each shot kicks the view up this much, degrees."),
        ("RECOIL_FIRST", F32(weapon::RECOIL_FIRST), "The first of a burst kicks this many times as hard."),
        ("RECOIL_SIDE_MIN", F32(weapon::RECOIL_SIDE[0]), "Sideways kick range, degrees (positive: right)."),
        ("RECOIL_SIDE_MAX", F32(weapon::RECOIL_SIDE[1]), ""),
        ("RECOIL_DELAY", F32(weapon::RECOIL_DELAY), "After the last shot the view drifts back down after this long, s,"),
        ("RECOIL_RECOVERY", F32(weapon::RECOIL_RECOVERY), "at this many degrees a second."),
        ("BURST_GAP", F32(weapon::BURST_GAP), "A shot this long (s) after the last starts a new burst."),
        ("ADS_SPEED", F32(weapon::ADS_SPEED), "Down the sights: this share of running speed,"),
        ("ADS_ZOOM", F32(weapon::ADS_ZOOM), "and the view zoomed this many times."),
        ("MEND_RADIUS", F32(lattice_game::ability::MEND_RADIUS), "A Mend field heals friends within this of the medic, m (U6)."),
        ("CLOAK_RANGE", F32(lattice_game::ability::CLOAK_RANGE), "Enemies past this aren't sent a cloaked player, m."),
        ("BODY_RADIUS", F32(hit::BODY_RADIUS), "Hitboxes: a body capsule (radius, and its segment's ends above the feet), m."),
        ("BODY_LOW", F32(hit::BODY_LOW), ""),
        ("BODY_HIGH", F32(hit::BODY_HIGH), ""),
        ("HEAD_RADIUS", F32(hit::HEAD_RADIUS), "A head sphere (radius, center above the feet), m."),
        ("HEAD_AT", F32(hit::HEAD_AT), ""),
        ("CROUCH_BODY_HIGH", F32(hit::CROUCH_BODY_HIGH), "Crouched: the body segment's top and the head's center, m."),
        ("CROUCH_HEAD_AT", F32(hit::CROUCH_HEAD_AT), ""),
        ("FACTIONS", Int(faction::FACTIONS as i64, "int"), "A player's faction is its entity id mod this."),
        ("MAX_HEALTH", Int(faction::MAX_HEALTH as i64, "int"), ""),
        ("RESPAWN_STEPS", Int(faction::RESPAWN_STEPS as i64, "int"), "Steps from dying to respawning."),
        ("TERRAIN_RES", F32(world::TERRAIN_RES), "Height sample spacing, m."),
        ("TERRAIN_N", Int(world::TERRAIN_N as i64, "int"), "Height samples per side."),
        ("SITE_BLEND", F32(world::SITE_BLEND), "A site's flat ground blends back to the hills over this, m."),
        ("POINT_RADIUS", F32(lattice_game::territory::POINT_RADIUS), "A control point counts whoever stands within this of it, m,"),
        ("POINT_HEIGHT", F32(lattice_game::territory::POINT_HEIGHT), "and this above or below it, m."),
        ("MAX_NEAR_DELAY", F64(msg::MAX_NEAR_DELAY), "The longest render delays the server accepts, steps: near,"),
        ("MAX_MID_DELAY", F64(msg::MAX_MID_DELAY), "mid and far."),
        ("TELEPORT", F32(entities::TELEPORT), "A drawn jump longer than this is a teleport (a respawn), m."),
        ("TRACER_SEGMENT", F32(tracers::SEG), "Seconds per tracer flight segment."),
        ("TRACER_RANGE", F32(tracers::RANGE_SECS), "Seconds a tracer flies at most."),
        ("TRACER_TRAIL", F32(tracers::TRAIL), "Path behind a tracer's head that shows, m."),
        ("TRACER_CONVERGE", F32(tracers::CONVERGE), "Our tracers converge on the line of sight this far out, m."),
        ("MAX_TRACERS", Int(tracers::MAX_FLYING as i64, "int"), "Tracers flying at once, at most."),
        ("CYCLE_STEPS", Int(clock::CYCLE as i64, "int"), "Kiln's day (K4): a cycle, steps; where each phase starts in it:"),
        ("DUSK_STEP", Int(clock::DUSK as i64, "int"), ""),
        ("NIGHT_STEP", Int(clock::NIGHT as i64, "int"), ""),
        ("DAWN_STEP", Int(clock::DAWN as i64, "int"), ""),
        ("BRIDGE_UP_STEP", Int(clock::BRIDGE_UP as i64, "int"), "The living bridges stand from this step of the cycle,"),
        ("BRIDGE_DOWN_STEP", Int(clock::BRIDGE_DOWN as i64, "int"), "to this one (the minute before it, their warning)."),
        ("VENT_CYCLE", Int(clock::VENT_CYCLE as i64, "int"), "A vent lift runs for VENT_ON steps of every VENT_CYCLE."),
        ("VENT_ON", Int(clock::VENT_ON as i64, "int"), ""),
        ("FALLS_STEPS", Int(clock::FALLS as i64, "int"), "The sandfalls take turns this many steps long."),
        ("SAFE_FALL", F32(movement::SAFE_FALL), "Falls past this hurt, m,"),
        ("FALL_DAMAGE_PER_M", F32(movement::FALL_DAMAGE_PER_M), "this much a meter past it."),
    ]
}

/// SCREAMING_SNAKE or snake_case to PascalCase (`first_lower`: camelCase).
fn case(name: &str, first_lower: bool) -> String {
    let mut out = String::new();
    for (i, part) in name.split('_').filter(|p| !p.is_empty()).enumerate() {
        let lower = part.to_ascii_lowercase();
        let mut chars = lower.chars();
        if let Some(c) = chars.next() {
            if i == 0 && first_lower {
                out.push(c);
            } else {
                out.push(c.to_ascii_uppercase());
            }
            out.extend(chars);
        }
    }
    out
}

/// A field's C# name: camelCase, with C# keywords escaped (`base` -> `@base`).
fn field_name(name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "abstract", "as", "base", "bool", "break", "byte", "case", "catch", "char", "checked", "class", "const", "continue", "decimal",
        "default", "delegate", "do", "double", "else", "enum", "event", "explicit", "extern", "false", "finally", "fixed", "float", "for",
        "foreach", "goto", "if", "implicit", "in", "int", "interface", "internal", "is", "lock", "long", "namespace", "new", "null",
        "object", "operator", "out", "override", "params", "private", "protected", "public", "readonly", "ref", "return", "sbyte",
        "sealed", "short", "sizeof", "stackalloc", "static", "string", "struct", "switch", "this", "throw", "true", "try", "typeof",
        "uint", "ulong", "unchecked", "unsafe", "ushort", "using", "virtual", "void", "volatile", "while",
    ];
    let n = case(name, true);
    if KEYWORDS.contains(&n.as_str()) {
        format!("@{n}")
    } else {
        n
    }
}

fn primitive(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "u8" => "byte",
        "i8" => "sbyte",
        "u16" => "ushort",
        "i16" => "short",
        "u32" => "uint",
        "i32" => "int",
        "u64" => "ulong",
        "i64" => "long",
        "f32" => "float",
        "f64" => "double",
        "()" => "void",
        _ => return None,
    })
}

/// A function argument's or return's C# type.
fn arg_type(ty: &str) -> String {
    let ty = normalize(ty);
    if let Some(p) = primitive(&ty) {
        return p.into();
    }
    for prefix in ["*mut ", "*const "] {
        if let Some(inner) = ty.strip_prefix(prefix) {
            if let Some(p) = primitive(inner) {
                return format!("{p}*");
            }
            if STRUCTS.iter().any(|s| s.name == inner) {
                return format!("{inner}*");
            }
            // Opaque handles (LtSession, LtWorld).
            return "IntPtr".into();
        }
    }
    panic!("no C# type for {ty}");
}

/// `/// <summary>` lines from a doc string, XML-escaped.
fn doc(out: &mut String, indent: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let esc = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let _ = writeln!(out, "{indent}/// <summary>");
    for line in esc.lines() {
        let _ = writeln!(out, "{indent}/// {}", line.trim());
    }
    let _ = writeln!(out, "{indent}/// </summary>");
}

/// The whole generated file.
pub fn generate() -> String {
    let mut o = String::new();
    let hash = layout_hash(FNS);
    let _ = writeln!(o, "// <auto-generated>");
    let _ = writeln!(o, "// From lattice-net's client-ffi: `cargo run -p lattice-client-ffi --bin gen-cs`. Don't edit:");
    let _ = writeln!(o, "// rebuild the plugin and its bindings together (scripts/unity-plugin.sh).");
    let _ = writeln!(o, "// </auto-generated>");
    let _ = writeln!(o, "using System;");
    let _ = writeln!(o, "using System.Runtime.InteropServices;");
    let _ = writeln!(o);
    let _ = writeln!(o, "namespace Lattice.Native");
    let _ = writeln!(o, "{{");

    // Constants.
    doc(&mut o, "    ", "The game's constants, from the crates that define them.");
    let _ = writeln!(o, "    public static class LtConst");
    let _ = writeln!(o, "    {{");
    doc(&mut o, "        ", "The plugin API's version (lt_version).");
    let _ = writeln!(o, "        public const uint Version = {VERSION};");
    doc(&mut o, "        ", "The layout hash of these bindings (lt_layout_hash).");
    let _ = writeln!(o, "        public const ulong LayoutHash = 0x{hash:016X};");
    for (name, value, d) in constants() {
        doc(&mut o, "        ", d);
        let n = case(name, false);
        let _ = match value {
            Value::F32(v) => writeln!(o, "        public const float {n} = {v:?}f;"),
            Value::F64(v) => writeln!(o, "        public const double {n} = {v:?};"),
            Value::Int(v, t) => writeln!(o, "        public const {t} {n} = {v};"),
        };
    }
    let _ = writeln!(o, "    }}");
    let _ = writeln!(o);

    // Codes.
    doc(&mut o, "    ", "The codes the structs carry.");
    let _ = writeln!(o, "    public static class LtCode");
    let _ = writeln!(o, "    {{");
    for (name, value, d) in CODES {
        doc(&mut o, "        ", d);
        let _ = writeln!(o, "        public const int {} = {value};", case(name.trim_start_matches("LT_"), false));
    }
    let _ = writeln!(o, "    }}");

    // Structs.
    for s in STRUCTS {
        let _ = writeln!(o);
        doc(&mut o, "    ", s.doc);
        let _ = writeln!(o, "    [StructLayout(LayoutKind.Explicit, Size = {})]", s.size);
        let _ = writeln!(o, "    public unsafe partial struct {}", s.name);
        let _ = writeln!(o, "    {{");
        for f in s.fields {
            doc(&mut o, "        ", f.doc);
            let ty = normalize(f.ty);
            let name = field_name(f.name);
            if let Some(inner) = ty.strip_prefix('[') {
                let (elem, n) = inner.trim_end_matches(']').split_once(';').expect("an array type");
                let elem = primitive(elem).unwrap_or_else(|| panic!("fixed buffers of {elem}"));
                let _ = writeln!(o, "        [FieldOffset({})] public fixed {elem} {name}[{n}];", f.offset);
            } else {
                let cs = primitive(&ty).map(String::from).unwrap_or_else(|| {
                    assert!(STRUCTS.iter().any(|x| x.name == ty), "no C# type for {ty}");
                    ty.clone()
                });
                let _ = writeln!(o, "        [FieldOffset({})] public {cs} {name};", f.offset);
            }
        }
        let _ = writeln!(o, "    }}");
    }

    // Functions.
    let _ = writeln!(o);
    doc(&mut o, "    ", "The plugin's functions, bound by name from the loaded library.");
    let _ = writeln!(o, "    public unsafe struct LtApi");
    let _ = writeln!(o, "    {{");
    let sig = |f: &crate::api::FnInfo| {
        let mut types: Vec<String> = f.args.iter().map(|(_, t)| arg_type(t)).collect();
        types.push(arg_type(f.ret));
        format!("delegate* unmanaged[Cdecl]<{}>", types.join(", "))
    };
    for f in FNS {
        let args: Vec<String> = f.args.iter().map(|(a, t)| format!("{} {a}", arg_type(t))).collect();
        let mut d = f.doc.trim().to_string();
        if !args.is_empty() {
            d.push_str(&format!("\n({})", args.join(", ")));
        }
        doc(&mut o, "        ", &d);
        // (NonSerialized: Unity's serialization weaver can't read function pointer fields.)
        let _ = writeln!(o, "        [NonSerialized] public {} {};", sig(f), f.name);
    }
    let _ = writeln!(o);
    doc(&mut o, "        ", "Binds every function through `proc` (name to address); returns the names it couldn't find.");
    let _ = writeln!(o, "        public string Bind(Func<string, IntPtr> proc)");
    let _ = writeln!(o, "        {{");
    let _ = writeln!(o, "            var missing = new System.Text.StringBuilder();");
    let _ = writeln!(o, "            IntPtr p;");
    for f in FNS {
        let _ = writeln!(o, "            p = proc(\"{0}\"); if (p == IntPtr.Zero) missing.Append(\" {0}\"); {0} = ({1})(void*)p;", f.name, sig(f));
    }
    let _ = writeln!(o, "            return missing.ToString().Trim();");
    let _ = writeln!(o, "        }}");
    let _ = writeln!(o, "    }}");
    let _ = writeln!(o, "}}");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_turn_into_csharp_case() {
        assert_eq!(case("RUN_SPEED", false), "RunSpeed");
        assert_eq!(case("near_delay_ms", true), "nearDelayMs");
        assert_eq!(case("STATE_TIMED_OUT", false), "StateTimedOut");
        assert_eq!(case("x", true), "x");
        assert_eq!(field_name("base"), "@base", "a C# keyword");
        assert_eq!(field_name("near_delay_ms"), "nearDelayMs");
    }

    #[test]
    fn the_bindings_cover_everything_with_its_layout() {
        let cs = generate();
        for f in FNS {
            assert!(cs.contains(&format!("> {};", f.name)), "{} bound", f.name);
        }
        for s in STRUCTS {
            assert!(cs.contains(&format!("[StructLayout(LayoutKind.Explicit, Size = {})]\n    public unsafe partial struct {}", s.size, s.name)));
        }
        assert!(cs.contains("public const float RunSpeed = 6.0f;"));
        assert!(cs.contains("public const int TickHz = 30;"));
        assert!(cs.contains(&format!("LayoutHash = 0x{:016X}", layout_hash(FNS))));
        assert!(cs.contains("[FieldOffset(0)] public float x;"));
        assert!(cs.contains("public fixed ulong frames[12];"));
        assert!(cs.contains("[NonSerialized] public delegate* unmanaged[Cdecl]<IntPtr, LtIntent*, void> lt_inputs;"));
        assert!(cs.contains("[NonSerialized] public delegate* unmanaged[Cdecl]<byte*, ulong, byte*, ulong, LtConfig*, IntPtr> lt_connect_dev;"));
        // Balanced braces: it parses as far as a brace counter can tell.
        assert_eq!(cs.matches('{').count(), cs.matches('}').count());
    }
}
