//! Turns Meshy's output (`assets/meshy_output/*/<name>.glb`) into the game's
//! models (`assets/models/`):
//!
//! - textures down to 1024 px (Meshy's are 2k; the game draws thousands of
//!   props), materials single-sided;
//! - props normalized: x along the long side, y up, z out of the front, the
//!   body filling [-0.5, 0.5] x [0, 1] x [-0.5, 0.5], so the client scales a
//!   prop straight to its cover box (antennas and such may stick out above);
//! - the rifle centered and 1 m long, its muzzle at -x, simplified to ~8k
//!   triangles (held) and ~40k (`rifle_view`, the first-person one);
//! - the soldier as rigged (textures at 2048 px: it's seen up close), plus
//!   three lighter meshes for the distance (meshoptimizer's simplifier on its
//!   index buffer: same vertices and skin, a quarter, a fifteenth and a
//!   fiftieth of the triangles); each animation clip cut down to its skeleton
//!   and keyframes, the two clips that run forward made to run in place, and
//!   the jump's own rise taken out (the game moves the body up).
//!
//! Run from `tools/import-assets/`: `cargo run --release`.
//!
//! `cargo run --release -- decimate IN.glb OUT.glb TRIANGLES` makes a model
//! that's too detailed to rig (Meshy rigs up to 300k faces) or to draw
//! hundreds of times light enough: meshoptimizer's simplifier, which keeps
//! each vertex's UVs, so the model's own textures (shrunk to 2048 px) still
//! fit it.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Longest texture side kept.
const TEXTURE_MAX: u32 = 1024;
/// The rifle's source: long along x, muzzle at -x.
const RIFLE: &str = "rifle_user.glb";
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

    /// Every texture down to `max` px, every material single-sided.
    fn shrink_textures(&mut self, max: u32) {
        let mut replace = HashMap::new();
        for img in self.arr("images") {
            let bv = img["bufferView"].as_u64().unwrap() as usize;
            let v = &self.json["bufferViews"][bv];
            let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
            let bytes = &self.bin[off..off + v["byteLength"].as_u64().unwrap() as usize];
            let pic = image::load_from_memory(bytes).expect("texture");
            if pic.width().max(pic.height()) <= max {
                continue;
            }
            let small = pic.resize(max, max, image::imageops::FilterType::Lanczos3).into_rgb8();
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

    /// The first primitive's triangle indices.
    fn indices(&self) -> Vec<u32> {
        let ia = self.json["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize;
        let acc = &self.json["accessors"][ia];
        let bv = &self.json["bufferViews"][acc["bufferView"].as_u64().unwrap() as usize];
        let start = bv["byteOffset"].as_u64().unwrap_or(0) as usize + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
        let n = acc["count"].as_u64().unwrap() as usize;
        match acc["componentType"].as_u64() {
            Some(5125) => (0..n).map(|k| u32::from_le_bytes(self.bin[start + 4 * k..][..4].try_into().unwrap())).collect(),
            Some(5123) => (0..n).map(|k| u16::from_le_bytes(self.bin[start + 2 * k..][..2].try_into().unwrap()) as u32).collect(),
            t => panic!("index type {t:?}"),
        }
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

    /// A lighter level of detail with about `tris` triangles: the
    /// simplifier picks a subset of the triangles' corners (normals and UVs
    /// weigh in); the vertices (and their skin weights) stay as they are, so
    /// the same skeleton drives it. Only the mesh is kept.
    fn lod(&self, tris: usize) -> Glb {
        let mut g = Glb { json: self.json.clone(), bin: self.bin.clone() };
        let pos = g.read(g.attribute("POSITION").unwrap());
        let extras: Vec<Vec<Vec<f32>>> = ["NORMAL", "TEXCOORD_0"].iter().filter_map(|n| g.attribute(n)).map(|a| g.read(a)).collect();
        let indices = g.indices();
        let bytes: Vec<u8> = pos.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let adapter = meshopt::VertexDataAdapter::new(&bytes, 12, 0).unwrap();
        let mut attrs = Vec::new();
        for i in 0..pos.len() {
            for e in &extras {
                attrs.extend_from_slice(&e[i]);
            }
        }
        let weights: Vec<f32> = extras.iter().flat_map(|e| std::iter::repeat_n(0.5, e[0].len())).collect();
        let locks = vec![false; pos.len()];
        let mut error = 0.0;
        let mut out = meshopt::simplify_with_attributes_and_locks(
            &indices, &adapter, &attrs, &weights, weights.len(), &locks, tris * 3, 0.2, meshopt::SimplifyOptions::Permissive, Some(&mut error),
        );
        if out.len() > tris * 3 * 13 / 10 {
            // Still too many (seams): the sloppy simplifier ignores topology.
            out = meshopt::simplify_sloppy(&indices, &adapter, tris * 3, 0.2, Some(&mut error));
        }
        let ia = g.json["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize;
        let bv = g.json["accessors"][ia]["bufferView"].as_u64().unwrap() as usize;
        let bytes: Vec<u8> = out.iter().flat_map(|i| i.to_le_bytes()).collect();
        g.json["accessors"][ia]["count"] = json!(out.len());
        g.json["accessors"][ia]["byteOffset"] = json!(0);
        g.json["accessors"][ia]["componentType"] = json!(5125);
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
        println!("  lod: {} triangles (error {:.4} of its size)", out.len() / 3, error);
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("decimate") {
        let [_, input, output, tris] = &args[..] else {
            panic!("usage: import-assets decimate IN.glb OUT.glb TRIANGLES");
        };
        decimate(Path::new(input), Path::new(output), tris.parse().expect("TRIANGLES"));
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/assets");
    let (raw, out) = (root.join("meshy_output"), root.join("models"));
    fs::create_dir_all(&out).unwrap();
    let save = |name: &str, g: &Glb| {
        let p = out.join(format!("{name}.glb"));
        write_glb(&p, g);
        println!("{:<14} {:>6} KB", name, fs::metadata(&p).unwrap().len() / 1024);
    };

    for (meshy, name, top) in PROPS {
        let mut g = read_glb(&find(&raw, &format!("{meshy}.glb")));
        g.shrink_textures(TEXTURE_MAX);
        let (lo, hi) = g.bounds();
        let top = top.unwrap_or(hi[1]);
        let center = [(lo[0] + hi[0]) / 2.0, lo[1], (lo[2] + hi[2]) / 2.0];
        g.transform(center, [hi[0] - lo[0], top - lo[1], hi[2] - lo[2]]);
        save(name, &g);
    }

    // The rifle: centered, 1 m long (the client sizes it).
    // The rifle (made from concept views in Meshy, ~800k triangles): ~8k
    // in soldiers' hands, ~40k for the first-person view.
    let full = read_glb(&find(&raw, RIFLE));
    for (name, tris, error, texture) in [("rifle", 8_000, 0.15, TEXTURE_MAX), ("rifle_view", 40_000, 0.05, 2048)] {
        let mut g = full.simplified(tris, error);
        g.shrink_textures(texture);
        let (lo, hi) = g.bounds();
        let len = hi[0] - lo[0];
        g.transform([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0], [len; 3]);
        save(name, &g);
    }

    let mut g = read_glb(&find(&raw, "soldier_rigged.glb"));
    g.shrink_textures(2048);
    let tris = g.indices().len() / 3;
    println!("soldier: {tris} triangles");
    save("soldier", &g);
    save("soldier_lod1", &g.lod(tris / 4));
    save("soldier_lod2", &g.lod(tris / 15));
    save("soldier_lod3", &g.lod(tris / 50));

    for (meshy, name, fix) in CLIPS {
        let mut g = read_glb(&find(&raw, &format!("{meshy}.glb")));
        if fix != Fix::None {
            g.fix_hips(fix == Fix::NoRise);
        }
        g.strip_to_animation();
        save(name, &g);
    }
}

/// `decimate`: simplifies IN's mesh to about `tris` triangles with
/// meshoptimizer (normals and UVs weigh in, so seams and shading hold up and
/// the textures still fit), drops the vertices nothing uses, and shrinks the
/// textures to 2048 px. One mesh, one primitive, unrigged.
fn decimate(input: &Path, output: &Path, tris: usize) {
    let mut g = read_glb(input).simplified(tris, 0.05);
    g.shrink_textures(2048);
    write_glb(output, &g);
    println!("  {} ({} KB)", output.display(), fs::metadata(output).unwrap().len() / 1024);
}

impl Glb {
    /// This (single, unrigged) mesh simplified to about `tris` triangles
    /// with meshoptimizer, stopping early rather than deviating more than
    /// `max_error` of its size: normals and UVs weigh in, so seams and
    /// shading hold up and the textures still fit; unused vertices are
    /// dropped.
    fn simplified(&self, tris: usize, max_error: f32) -> Glb {
        let mut g = Glb { json: self.json.clone(), bin: self.bin.clone() };
        let attrs: Vec<(&str, usize)> = ["POSITION", "NORMAL", "TEXCOORD_0"].iter().filter_map(|&n| g.attribute(n).map(|a| (n, a))).collect();
        let data: Vec<Vec<Vec<f32>>> = attrs.iter().map(|&(_, a)| g.read(a)).collect();
        let pos = &data[0];
        let indices = g.indices();
        println!("  simplifying {} triangles, {} vertices", indices.len() / 3, pos.len());
        let bytes: Vec<u8> = pos.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect();
        let adapter = meshopt::VertexDataAdapter::new(&bytes, 12, 0).unwrap();
        // Normal and UV per vertex, weighted against position error.
        let (mut extra, mut weights) = (Vec::new(), Vec::new());
        for (k, &(name, _)) in attrs.iter().enumerate().skip(1) {
            let w = if name == "NORMAL" { 0.5 } else { 1.0 };
            weights.extend(std::iter::repeat_n(w, data[k][0].len()));
        }
        for i in 0..pos.len() {
            for d in &data[1..] {
                extra.extend_from_slice(&d[i]);
            }
        }
        let locks = vec![false; pos.len()];
        let mut error = 0.0;
        let mut out = meshopt::simplify_with_attributes_and_locks(
            &indices,
            &adapter,
            &extra,
            &weights,
            weights.len(),
            &locks,
            tris * 3,
            max_error,
            meshopt::SimplifyOptions::None,
            Some(&mut error),
        );
        // Seams can stop it short of the target: then allow collapses across them.
        if out.len() > tris * 3 * 13 / 10 {
            println!("  {} triangles at the seams' limit; collapsing across them", out.len() / 3);
            out = meshopt::simplify_with_attributes_and_locks(
                &indices,
                &adapter,
                &extra,
                &weights,
                weights.len(),
                &locks,
                tris * 3,
                max_error,
                meshopt::SimplifyOptions::Permissive,
                Some(&mut error),
            );
        }
        // Keep only the vertices used, in first-use order.
        let mut remap = vec![u32::MAX; pos.len()];
        let mut order: Vec<usize> = Vec::new();
        let new_indices: Vec<u32> = out
            .iter()
            .map(|&i| {
                let r = &mut remap[i as usize];
                if *r == u32::MAX {
                    *r = order.len() as u32;
                    order.push(i as usize);
                }
                *r
            })
            .collect();
        println!("  -> {} triangles, {} vertices (error {:.4} of its size)", new_indices.len() / 3, order.len(), error);
        let mut replace = HashMap::new();
        for (k, &(_, a)) in attrs.iter().enumerate() {
            let bv = g.json["accessors"][a]["bufferView"].as_u64().unwrap() as usize;
            assert!(g.json["bufferViews"][bv].get("byteStride").is_none(), "interleaved attributes");
            let v: Vec<u8> = order.iter().flat_map(|&i| data[k][i].iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).collect();
            replace.insert(bv, v);
            g.json["accessors"][a]["count"] = json!(order.len());
            g.json["accessors"][a]["byteOffset"] = json!(0);
        }
        let ia = g.json["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize;
        let ibv = g.json["accessors"][ia]["bufferView"].as_u64().unwrap() as usize;
        replace.insert(ibv, new_indices.iter().flat_map(|i| i.to_le_bytes()).collect());
        g.json["accessors"][ia]["count"] = json!(new_indices.len());
        g.json["accessors"][ia]["byteOffset"] = json!(0);
        g.json["accessors"][ia]["componentType"] = json!(5125);
        let views: Vec<usize> = (0..g.arr("bufferViews").len()).collect();
        g.rebuild(&views, &replace);
        // POSITION's bounds.
        let p = g.attribute("POSITION").unwrap();
        let pos = g.read(p);
        g.write(p, &pos);
        g
    }
}
