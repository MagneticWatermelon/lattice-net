//! Bit-level packing + quantization for game state inside messages.
//!
//! The transport moves bytes. This is how you make those bytes small. Example:
//! a far-tier remote player in 8 bytes:
//!
//! | field                        | bits | precision            |
//! |------------------------------|------|----------------------|
//! | x, y relative to grid cell   | 2x15 | 512 m cell -> ~1.6 cm|
//! | z (altitude)                 | 12   | 0..1024 m -> 25 cm   |
//! | yaw                          | 9    | ~0.7 deg             |
//! | pitch                        | 6    | ~2.9 deg             |
//! | stance / vehicle-seat flags  | 3    |                      |
//! | health bucket                | 4    | 16 levels            |
//! | **total**                    | 64   |                      |
//!
//! Plenty for something rendered >300 m away at 2 Hz. Near-tier entities get more bits.

use crate::wire::DecodeError;

#[derive(Default)]
pub struct BitWriter {
    out: Vec<u8>,
    scratch: u64,
    bits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// With room for `bytes` bytes, so writing that much never reallocates.
    pub fn with_capacity(bytes: usize) -> Self {
        Self { out: Vec::with_capacity(bytes), ..Self::default() }
    }

    /// Write the low `n` bits of `value` (n <= 32).
    #[inline]
    pub fn write(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        debug_assert!(n == 32 || value >> n == 0, "value {value} doesn't fit in {n} bits");
        self.scratch |= (value as u64) << self.bits;
        self.bits += n;
        while self.bits >= 8 {
            self.out.push(self.scratch as u8);
            self.scratch >>= 8;
            self.bits -= 8;
        }
    }

    #[inline]
    pub fn write_bool(&mut self, b: bool) {
        self.write(b as u32, 1);
    }

    pub fn bit_len(&self) -> usize {
        self.out.len() * 8 + self.bits as usize
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push(self.scratch as u8);
        }
        self.out
    }
}

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u32, DecodeError> {
        let n = n as usize;
        if self.pos + n > self.data.len() * 8 {
            return Err(DecodeError::Eof);
        }
        let mut value = 0u64;
        let mut got = 0;
        while got < n {
            let off = self.pos % 8;
            let take = (8 - off).min(n - got);
            let bits = (self.data[self.pos / 8] >> off) & ((1u16 << take) - 1) as u8;
            value |= (bits as u64) << got;
            got += take;
            self.pos += take;
        }
        Ok(value as u32)
    }

    #[inline]
    pub fn read_bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.read(1)? != 0)
    }
}

/// Map `v` in [min, max] to an integer in [0, 2^bits - 1].
#[inline]
pub fn quantize(v: f32, min: f32, max: f32, bits: u32) -> u32 {
    let steps = ((1u64 << bits) - 1) as f32;
    let t = (v.clamp(min, max) - min) / (max - min);
    (t * steps).round() as u32
}

#[inline]
pub fn dequantize(q: u32, min: f32, max: f32, bits: u32) -> f32 {
    let steps = ((1u64 << bits) - 1) as f32;
    min + (q as f32 / steps) * (max - min)
}

/// Angles wrap, so quantize over [0, 2pi) with 2^bits steps (no duplicate at 2pi).
#[inline]
pub fn quantize_angle(rad: f32, bits: u32) -> u32 {
    let tau = std::f32::consts::TAU;
    let t = rad.rem_euclid(tau) / tau;
    ((t * (1u64 << bits) as f32).round() as u64 % (1u64 << bits)) as u32
}

#[inline]
pub fn dequantize_angle(q: u32, bits: u32) -> f32 {
    q as f32 / (1u64 << bits) as f32 * std::f32::consts::TAU
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed_widths() {
        let fields: Vec<(u32, u32)> = (1..=32).map(|n| (((1u64 << n) - 1) as u32 / 3, n)).collect();
        let mut w = BitWriter::new();
        for &(v, n) in &fields {
            w.write(v, n);
        }
        let bits = w.bit_len();
        let bytes = w.finish();
        assert_eq!(bytes.len(), bits.div_ceil(8));
        let mut r = BitReader::new(&bytes);
        for &(v, n) in &fields {
            assert_eq!(r.read(n).unwrap(), v);
        }
    }

    #[test]
    fn far_tier_player_is_8_bytes() {
        let (x, y, z, yaw, pitch) = (123.456f32, -200.25f32, 87.3f32, 4.0f32, -0.3f32);
        let mut w = BitWriter::new();
        w.write(quantize(x, -256.0, 256.0, 15), 15);
        w.write(quantize(y, -256.0, 256.0, 15), 15);
        w.write(quantize(z, 0.0, 1024.0, 12), 12);
        w.write(quantize_angle(yaw, 9), 9);
        w.write(quantize(pitch, -std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2, 6), 6);
        w.write(0b101, 3);
        w.write(12, 4);
        assert_eq!(w.bit_len(), 64);
        let bytes = w.finish();
        assert_eq!(bytes.len(), 8);

        let mut r = BitReader::new(&bytes);
        let dx = dequantize(r.read(15).unwrap(), -256.0, 256.0, 15);
        let dy = dequantize(r.read(15).unwrap(), -256.0, 256.0, 15);
        let dz = dequantize(r.read(12).unwrap(), 0.0, 1024.0, 12);
        let dyaw = dequantize_angle(r.read(9).unwrap(), 9);
        assert!((dx - x).abs() < 0.01);
        assert!((dy - y).abs() < 0.01);
        assert!((dz - z).abs() < 0.13);
        assert!((dyaw - yaw).abs() < 0.007);
        let _pitch = r.read(6).unwrap();
        assert_eq!(r.read(3).unwrap(), 0b101);
        assert_eq!(r.read(4).unwrap(), 12);
        assert!(r.read(1).is_err());
    }

    #[test]
    fn read_past_end_errors() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.read(8).unwrap(), 0xFF);
        assert_eq!(r.read(1), Err(DecodeError::Eof));
    }
}
