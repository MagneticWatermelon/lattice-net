//! Tiny deterministic xorshift RNG, so runs are reproducible from a seed.

#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // splitmix64 so nearby seeds give unrelated streams, never 0
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.unit() * (hi - lo)
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    /// Uniform point in the disk of radius `r` around `c`.
    pub fn in_disk(&mut self, c: [f32; 2], r: f32) -> [f32; 2] {
        let d = r * self.unit().sqrt();
        let a = self.unit() * std::f32::consts::TAU;
        [c[0] + d * a.cos(), c[1] + d * a.sin()]
    }
}
