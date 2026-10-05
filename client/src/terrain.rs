//! Terrain meshes from the world's heightmap.
//!
//! The map is 64 × 64 chunks of 128 m. Each chunk is drawn at one of two
//! levels of detail: every sample (4 m) near the camera, every 8th (32 m)
//! farther out. Each mesh hangs a skirt from its edges so the seams between
//! levels never show sky.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use lattice_game::world::{World, TERRAIN_N, TERRAIN_RES};

/// Samples per chunk side (cells): 128 m.
pub const CHUNK_CELLS: usize = 32;
pub const CHUNKS: usize = (TERRAIN_N - 1) / CHUNK_CELLS;
pub const CHUNK_SIZE: f32 = CHUNK_CELLS as f32 * TERRAIN_RES;
/// Sample strides of the two levels.
pub const FINE: usize = 1;
pub const COARSE: usize = 8;
/// How far the skirts hang below the edge, in meters.
const SKIRT: f32 = 6.0;

/// A mesh's arrays, in Bevy space.
#[derive(Debug, Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn into_mesh(self) -> Mesh {
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
            .with_inserted_indices(Indices::U32(self.indices))
    }
}

fn height(world: &World, ix: isize, iy: isize) -> f32 {
    let c = |v: isize| v.clamp(0, TERRAIN_N as isize - 1) as usize;
    world.sample(c(ix), c(iy))
}

/// Grass on the flats, drier up high, rock on the steep. (Vertex colors
/// are linear: picked in sRGB, converted.)
fn color(h: f32, slope: f32) -> [f32; 4] {
    let t = (h / 160.0).clamp(0.0, 1.0);
    let grass = [0.30 + 0.25 * t, 0.45 - 0.02 * t, 0.20 + 0.12 * t];
    let rock = [0.47, 0.45, 0.42];
    let r = ((slope - 0.45) / 0.35).clamp(0.0, 1.0);
    let srgb = |a: f32, b: f32| (a + (b - a) * r).powf(2.2);
    [srgb(grass[0], rock[0]), srgb(grass[1], rock[1]), srgb(grass[2], rock[2]), 1.0]
}

/// Chunk (cx, cy) with every `stride`th sample.
pub fn chunk(world: &World, cx: usize, cy: usize, stride: usize) -> MeshData {
    let n = CHUNK_CELLS / stride + 1;
    let (x0, y0) = ((cx * CHUNK_CELLS) as isize, (cy * CHUNK_CELLS) as isize);
    let s = stride as isize;
    let mut m = MeshData::default();
    for j in 0..n as isize {
        for i in 0..n as isize {
            let (ix, iy) = (x0 + i * s, y0 + j * s);
            let h = height(world, ix, iy);
            let dhdx = (height(world, ix + s, iy) - height(world, ix - s, iy)) / (2.0 * s as f32 * TERRAIN_RES);
            let dhdy = (height(world, ix, iy + s) - height(world, ix, iy - s)) / (2.0 * s as f32 * TERRAIN_RES);
            let nrm = Vec3::new(-dhdx, 1.0, dhdy).normalize();
            m.positions.push([ix as f32 * TERRAIN_RES, h, -(iy as f32) * TERRAIN_RES]);
            m.normals.push(nrm.into());
            m.colors.push(color(h, dhdx.hypot(dhdy)));
        }
    }
    let at = |i: usize, j: usize| (j * n + i) as u32;
    for j in 0..n - 1 {
        for i in 0..n - 1 {
            // Counterclockwise seen from above (Bevy +y).
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            m.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    // Skirts: each edge copied SKIRT lower, joined to the edge by a strip.
    let edges: [Vec<(usize, usize)>; 4] = [
        (0..n).map(|i| (i, 0)).collect(),
        (0..n).map(|j| (n - 1, j)).collect(),
        (0..n).rev().map(|i| (i, n - 1)).collect(),
        (0..n).rev().map(|j| (0, j)).collect(),
    ];
    for edge in edges {
        let base = m.positions.len() as u32;
        for &(i, j) in &edge {
            let k = at(i, j) as usize;
            let p = m.positions[k];
            m.positions.push([p[0], p[1] - SKIRT, p[2]]);
            m.normals.push(m.normals[k]);
            m.colors.push(m.colors[k]);
        }
        for w in 0..edge.len() - 1 {
            let (t0, t1) = (at(edge[w].0, edge[w].1), at(edge[w + 1].0, edge[w + 1].1));
            let (b0, b1) = (base + w as u32, base + w as u32 + 1);
            // Both windings: the skirt is seen from whichever side shows.
            m.indices.extend_from_slice(&[t0, b0, t1, t1, b0, b1, t0, t1, b0, t1, b1, b0]);
        }
    }
    m
}

/// Chunk coordinates holding game position (x, y).
pub fn chunk_of(x: f32, y: f32) -> (i32, i32) {
    ((x / CHUNK_SIZE).floor() as i32, (y / CHUNK_SIZE).floor() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_follow_the_heightmap_and_face_up() {
        let w = World::shared(7);
        let m = chunk(&w, 3, 5, FINE);
        let n = CHUNK_CELLS + 1;
        assert_eq!(m.positions.len(), n * n + 4 * n);
        // A vertex sits where the game's terrain is.
        let p = m.positions[2 * n + 7];
        let (gx, gy) = (p[0], -p[2]);
        assert!((w.terrain(gx, gy) - p[1]).abs() < 1e-3, "{p:?}");
        assert_eq!(chunk_of(gx, gy), (3, 5));
        // The first triangle faces up.
        let v = |k: u32| Vec3::from(m.positions[k as usize]);
        let (a, b, c) = (v(m.indices[0]), v(m.indices[1]), v(m.indices[2]));
        assert!((b - a).cross(c - a).y > 0.0);
        let coarse = chunk(&w, 3, 5, COARSE);
        assert_eq!(coarse.positions.len(), 25 + 4 * 5);
        assert_eq!(CHUNKS, 64);
    }
}
