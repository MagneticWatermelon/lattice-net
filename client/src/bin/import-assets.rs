//! Turns Meshy's output (`assets/meshy_output/*/<name>.glb`) into the game's
//! models (`assets/models/`):
//!
//! - textures down to 1024 px (Meshy's are 2k; the game draws thousands of
//!   props), materials single-sided;
//! - props normalized: x along the long side, y up, z out of the front, the
//!   body filling [-0.5, 0.5] x [0, 1] x [-0.5, 0.5], so the client scales a
//!   prop straight to its cover box (antennas and such may stick out above);
//! - the rifle centered and 1 m long, its muzzle at -x;
//! - the soldier as rigged, plus two lighter meshes for the distance
//!   (vertices clustered on 4 and 8 cm grids: same vertices and skin, fewer
//!   triangles); each animation clip cut down to its skeleton
//!   and keyframes, the two clips that run forward made to run in place, and
//!   the jump's own rise taken out (the game moves the body up).
//!
//! Run from `client/`: `cargo run --release --bin import-assets`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Longest texture side kept.
const TEXTURE_MAX: u32 = 1024;
const JPEG_QUALITY: u8 = 85;

/// A prop: its Meshy name, its game name, and the top of its body in
/// Meshy's units (`None`: the whole model).
const PROPS: [(&str, &str, Option<f32>); 8] = [
    ("crate", "crate", None),
    ("wall_panel", "wall", None),
    ("container", "container", None),
    ("sandbags", "sandbags", None),
    // Asked for as a tower, it came out as a guard post; the antenna stays
    // above its box.
    ("tower", "post", Some(0.25)),
    // The roof's dish and antennas stay above the box.
    ("command", "command", Some(0.19)),
    ("bunker", "bunker", None),
    ("rocks", "rocks", None),
];

/// What a clip needs: nothing, made to run in place, or its rise taken out.
#[derive(Clone, Copy, PartialEq)]
enum Fix {
    None,
    InPlace,
    NoRise,
}

/// Clips: Meshy file, game name, fix.
const CLIPS: [(&str, &str, Fix); 7] = [
    ("anim_idle", "anim_idle", Fix::None),
    ("anim_walk", "anim_walk", Fix::None),
    ("anim_run", "anim_run", Fix::None),
    ("anim_run_aim", "anim_run_aim", Fix::InPlace),
    ("anim_sprint", "anim_sprint", Fix::InPlace),
    ("anim_jump", "anim_jump", Fix::NoRise),
    ("anim_death", "anim_death", Fix::None),
];

struct Glb {
    json: Value,
    bin: Vec<u8>,
}

fn read_glb(path: &Path) -> Glb {
    let b = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&b[0..4], b"glTF", "{}: not a GLB", path.display());
    let jlen = u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&b[20..20 + jlen]).expect("GLB JSON");
    let off = 20 + jlen;
    let blen = u32::from_le_bytes(b[off..off + 4].try_into().unwrap()) as usize;
    Glb { json, bin: b[off + 8..off + 8 + blen].to_vec() }
}

fn write_glb(path: &Path, g: &Glb) {
    let mut j = serde_json::to_vec(&g.json).unwrap();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut bin = g.bin.clone();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + j.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    fs::write(path, out).unwrap();
}

impl Glb {
    fn arr(&self, key: &str) -> Vec<Value> {
        self.json.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
    }

    /// Where accessor `a`'s elements are: (byte offset of the first, stride,
    /// components, count). Float accessors only.
    fn layout(&self, a: usize) -> (usize, usize, usize, usize) {
        let acc = &self.json["accessors"][a];
        assert_eq!(acc["componentType"], 5126, "float accessors only");
        let n = match acc["type"].as_str().unwrap() {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            t => panic!("accessor type {t}"),
        };
        let bv = &self.json["bufferViews"][acc["bufferView"].as_u64().unwrap() as usize];
        let start = bv["byteOffset"].as_u64().unwrap_or(0) as usize + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
        let stride = bv["byteStride"].as_u64().map_or(4 * n, |s| s as usize);
        (start, stride, n, acc["count"].as_u64().unwrap() as usize)
    }

    fn read(&self, a: usize) -> Vec<Vec<f32>> {
        let (start, stride, n, count) = self.layout(a);
        (0..count)
            .map(|i| (0..n).map(|k| f32::from_le_bytes(self.bin[start + i * stride + 4 * k..][..4].try_into().unwrap())).collect())
            .collect()
    }

    fn write(&mut self, a: usize, v: &[Vec<f32>]) {
        let (start, stride, n, count) = self.layout(a);
        assert_eq!(v.len(), count);
        for (i, e) in v.iter().enumerate() {
            for (k, x) in e.iter().enumerate().take(n) {
                self.bin[start + i * stride + 4 * k..][..4].copy_from_slice(&x.to_le_bytes());
            }
        }
        let (mut lo, mut hi) = (vec![f32::MAX; n], vec![f32::MIN; n]);
        for e in v {
            for k in 0..n {
                (lo[k], hi[k]) = (lo[k].min(e[k]), hi[k].max(e[k]));
            }
        }
        self.json["accessors"][a]["min"] = json!(lo);
        self.json["accessors"][a]["max"] = json!(hi);
    }

    /// Rebuilds the binary chunk with only the buffer views in `keep` (in
    /// order), each replaced by `replace`'s bytes if it has some, and
    /// renumbers the views everywhere they're referenced.
    fn rebuild(&mut self, keep: &[usize], replace: &HashMap<usize, Vec<u8>>) {
        let views = self.arr("bufferViews");
        let (mut bin, mut new_views, mut remap) = (Vec::new(), Vec::new(), HashMap::new());
        for &i in keep {
            let mut v = views[i].clone();
            let data = match replace.get(&i) {
                Some(d) => d.clone(),
                None => {
                    let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
                    self.bin[off..off + v["byteLength"].as_u64().unwrap() as usize].to_vec()
                }
            };
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            v["byteOffset"] = json!(bin.len());
            v["byteLength"] = json!(data.len());
            bin.extend_from_slice(&data);
            remap.insert(i, new_views.len());
            new_views.push(v);
        }
        for key in ["accessors", "images"] {
            if let Some(items) = self.json.get_mut(key).and_then(Value::as_array_mut) {
                for item in items {
                    if let Some(bv) = item.get("bufferView").and_then(Value::as_u64) {
                        item["bufferView"] = json!(remap[&(bv as usize)]);
                    }
                }
            }
        }
        self.json["bufferViews"] = json!(new_views);
        self.json["buffers"] = json!([{ "byteLength": bin.len() }]);
        self.bin = bin;
    }

    /// Every texture down to `TEXTURE_MAX`, every material single-sided.
    fn shrink_textures(&mut self) {
        let mut replace = HashMap::new();
        for img in self.arr("images") {
            let bv = img["bufferView"].as_u64().unwrap() as usize;
            let v = &self.json["bufferViews"][bv];
            let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
            let bytes = &self.bin[off..off + v["byteLength"].as_u64().unwrap() as usize];
            let pic = image::load_from_memory(bytes).expect("texture");
            if pic.width().max(pic.height()) <= TEXTURE_MAX {
                continue;
            }
            let small = pic.resize(TEXTURE_MAX, TEXTURE_MAX, image::imageops::FilterType::Lanczos3).into_rgb8();
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&small).unwrap();
            replace.insert(bv, out);
        }
        for img in self.json["images"].as_array_mut().into_iter().flatten() {
            img["mimeType"] = json!("image/jpeg");
        }
        for m in self.json["materials"].as_array_mut().into_iter().flatten() {
            m["doubleSided"] = json!(false);
        }
        let all: Vec<usize> = (0..self.arr("bufferViews").len()).collect();
        self.rebuild(&all, &replace);
    }

    /// The first primitive's attribute accessor, if it has one.
    fn attribute(&self, name: &str) -> Option<usize> {
        self.json["meshes"][0]["primitives"][0]["attributes"][name].as_u64().map(|a| a as usize)
    }

    /// Maps positions by p -> (p - offset) / scale per axis, fixing normals
    /// and tangents to match.
    fn transform(&mut self, offset: [f32; 3], scale: [f32; 3]) {
        let p = self.attribute("POSITION").unwrap();
        let pos: Vec<Vec<f32>> = self.read(p).into_iter().map(|v| (0..3).map(|k| (v[k] - offset[k]) / scale[k]).collect()).collect();
        self.write(p, &pos);
        let unit = |v: Vec<f32>| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
            v.into_iter().map(|x| x / l).collect::<Vec<_>>()
        };
        if let Some(n) = self.attribute("NORMAL") {
            let v: Vec<Vec<f32>> = self.read(n).into_iter().map(|v| unit((0..3).map(|k| v[k] * scale[k]).collect())).collect();
            self.write(n, &v);
        }
        if let Some(t) = self.attribute("TANGENT") {
            let v: Vec<Vec<f32>> = self
                .read(t)
                .into_iter()
                .map(|v| {
                    let mut d = unit((0..3).map(|k| v[k] / scale[k]).collect());
                    d.push(v[3]);
                    d
                })
                .collect();
            self.write(t, &v);
        }
    }

    fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let pos = self.read(self.attribute("POSITION").unwrap());
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &pos {
            for k in 0..3 {
                (lo[k], hi[k]) = (lo[k].min(v[k]), hi[k].max(v[k]));
            }
        }
        (lo, hi)
    }

    /// A lighter level of detail: vertices merged on a grid of `cell`
    /// meters (each cell's first vertex stands for it), triangles that
    /// collapse dropped. The vertices (and their skin weights) stay as they
    /// are, so the same skeleton drives it. Only the mesh is kept.
    fn lod(&self, cell: f32) -> Glb {
        let mut g = Glb { json: self.json.clone(), bin: self.bin.clone() };
        let pos = g.read(g.attribute("POSITION").unwrap());
        let ia = g.json["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize;
        let acc = g.json["accessors"][ia].clone();
        let bv = acc["bufferView"].as_u64().unwrap() as usize;
        let start = g.json["bufferViews"][bv]["byteOffset"].as_u64().unwrap_or(0) as usize + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
        let wide = acc["componentType"] == 5125;
        let index = |k: usize| -> u32 {
            if wide {
                u32::from_le_bytes(g.bin[start + 4 * k..][..4].try_into().unwrap())
            } else {
                u16::from_le_bytes(g.bin[start + 2 * k..][..2].try_into().unwrap()) as u32
            }
        };
        let mut rep: HashMap<[i32; 3], u32> = HashMap::new();
        let merged: Vec<u32> = pos
            .iter()
            .enumerate()
            .map(|(i, p)| *rep.entry([0, 1, 2].map(|k| (p[k] / cell).floor() as i32)).or_insert(i as u32))
            .collect();
        let mut out = Vec::new();
        for t in 0..acc["count"].as_u64().unwrap() as usize / 3 {
            let [a, b, c] = [0, 1, 2].map(|k| merged[index(3 * t + k) as usize]);
            if a != b && b != c && a != c {
                out.extend([a, b, c]);
            }
        }
        let mut bytes = Vec::with_capacity(out.len() * 4);
        for &i in &out {
            if wide {
                bytes.extend_from_slice(&i.to_le_bytes());
            } else {
                bytes.extend_from_slice(&(i as u16).to_le_bytes());
            }
        }
        g.json["accessors"][ia]["count"] = json!(out.len());
        g.json["accessors"][ia]["byteOffset"] = json!(0);
        // Just the mesh: no material, textures or animation.
        for key in ["materials", "textures", "images", "samplers", "animations"] {
            if let Some(o) = g.json.as_object_mut() {
                o.remove(key);
            }
        }
        if let Some(p) = g.json["meshes"][0]["primitives"][0].as_object_mut() {
            p.remove("material");
        }
        let mut views: Vec<usize> = g.arr("accessors").iter().map(|a| a["bufferView"].as_u64().unwrap() as usize).collect();
        views.sort_unstable();
        views.dedup();
        g.rebuild(&views, &HashMap::from([(bv, bytes)]));
        println!("  lod {:.0} cm: {} triangles", cell * 100.0, out.len() / 3);
        g
    }

    /// Keeps only the skeleton and the animations: no meshes, skins or
    /// textures (the soldier's own file has those).
    fn strip_to_animation(&mut self) {
        for key in ["meshes", "materials", "textures", "images", "samplers", "skins"] {
            if let Some(o) = self.json.as_object_mut() {
                o.remove(key);
            }
        }
        for n in self.json["nodes"].as_array_mut().into_iter().flatten() {
            if let Some(o) = n.as_object_mut() {
                o.remove("mesh");
                o.remove("skin");
            }
        }
        // Accessors the animations use, renumbered.
        let mut used = BTreeMap::new();
        for a in self.json["animations"].as_array().unwrap() {
            for s in a["samplers"].as_array().unwrap() {
                for k in ["input", "output"] {
                    let i = s[k].as_u64().unwrap() as usize;
                    let next = used.len();
                    used.entry(i).or_insert(next);
                }
            }
        }
        let accessors = self.arr("accessors");
        let mut kept = vec![Value::Null; used.len()];
        for (&old, &new) in &used {
            kept[new] = accessors[old].clone();
        }
        for a in self.json["animations"].as_array_mut().unwrap() {
            for s in a["samplers"].as_array_mut().unwrap() {
                for k in ["input", "output"] {
                    s[k] = json!(used[&(s[k].as_u64().unwrap() as usize)]);
                }
            }
        }
        self.json["accessors"] = json!(kept);
        let mut views: Vec<usize> = kept.iter().map(|a| a["bufferView"].as_u64().unwrap() as usize).collect();
        views.sort_unstable();
        views.dedup();
        self.rebuild(&views, &HashMap::new());
    }

    /// Takes out the hips' forward drift (a clip made for root motion), so
    /// it runs in place: the drift from the first to the last key, removed
    /// linearly over the clip. Or (`no_rise`) the hips' rise above where
    /// they start, for a jump whose height the game already moves.
    fn fix_hips(&mut self, no_rise: bool) {
        let hips = self.arr("nodes").iter().position(|n| n["name"] == "Hips").expect("a Hips bone");
        let anim = self.json["animations"][0].clone();
        for ch in anim["channels"].as_array().unwrap() {
            if ch["target"]["node"] != json!(hips) || ch["target"]["path"] != "translation" {
                continue;
            }
            let s = &anim["samplers"][ch["sampler"].as_u64().unwrap() as usize];
            let (ti, vi) = (s["input"].as_u64().unwrap() as usize, s["output"].as_u64().unwrap() as usize);
            let t = self.read(ti);
            let mut v = self.read(vi);
            let (t0, t1) = (t[0][0], t[t.len() - 1][0]);
            let (first, last) = (v[0].clone(), v[v.len() - 1].clone());
            for (i, e) in v.iter_mut().enumerate() {
                if no_rise {
                    e[1] = e[1].min(first[1]);
                    continue;
                }
                let f = (t[i][0] - t0) / (t1 - t0).max(1e-6);
                e[0] -= (last[0] - first[0]) * f;
                e[2] -= (last[2] - first[2]) * f;
            }
            self.write(vi, &v);
        }
    }
}

/// The newest Meshy file named `file` under `raw`.
fn find(raw: &Path, file: &str) -> PathBuf {
    let mut hits: Vec<PathBuf> = fs::read_dir(raw)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(file))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    hits.pop().unwrap_or_else(|| panic!("no {file} under {}", raw.display()))
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let (raw, out) = (root.join("meshy_output"), root.join("models"));
    fs::create_dir_all(&out).unwrap();
    let save = |name: &str, g: &Glb| {
        let p = out.join(format!("{name}.glb"));
        write_glb(&p, g);
        println!("{:<14} {:>6} KB", name, fs::metadata(&p).unwrap().len() / 1024);
    };

    for (meshy, name, top) in PROPS {
        let mut g = read_glb(&find(&raw, &format!("{meshy}.glb")));
        g.shrink_textures();
        let (lo, hi) = g.bounds();
        let top = top.unwrap_or(hi[1]);
        let center = [(lo[0] + hi[0]) / 2.0, lo[1], (lo[2] + hi[2]) / 2.0];
        g.transform(center, [hi[0] - lo[0], top - lo[1], hi[2] - lo[2]]);
        save(name, &g);
    }

    // The rifle: centered, 1 m long (the client sizes it).
    let mut g = read_glb(&find(&raw, "rifle.glb"));
    g.shrink_textures();
    let (lo, hi) = g.bounds();
    let len = hi[0] - lo[0];
    g.transform([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0], [len; 3]);
    save("rifle", &g);

    let mut g = read_glb(&find(&raw, "soldier_rigged.glb"));
    g.shrink_textures();
    save("soldier", &g);
    save("soldier_lod1", &g.lod(0.04));
    save("soldier_lod2", &g.lod(0.08));

    for (meshy, name, fix) in CLIPS {
        let mut g = read_glb(&find(&raw, &format!("{meshy}.glb")));
        if fix != Fix::None {
            g.fix_hips(fix == Fix::NoRise);
        }
        g.strip_to_animation();
        save(name, &g);
    }
}
