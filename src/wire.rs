//! Byte-level little-endian reader/writer.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Ran out of bytes.
    Eof,
    /// Structurally invalid (unknown type, trailing bytes, bad size, ...).
    Invalid,
    /// Failed authentication: forged, corrupted, replayed from another
    /// sequence window, or from a different protocol version.
    Unauthenticated,
}

#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn with_capacity(n: usize) -> Self {
        Self { buf: Vec::with_capacity(n) }
    }
    #[inline]
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    #[inline]
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    /// 1 byte for n < 128, else 2 bytes (max 32767).
    #[inline]
    pub fn varlen(&mut self, n: usize) {
        assert!(n < 0x8000, "varlen too large: {n}");
        if n < 0x80 {
            self.u8(n as u8);
        } else {
            self.u8(0x80 | (n >> 8) as u8);
            self.u8(n as u8);
        }
    }
    pub fn pad_to(&mut self, n: usize) {
        if self.buf.len() < n {
            self.buf.resize(n, 0);
        }
    }
    pub fn len(&self) -> usize {
        self.buf.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    #[inline]
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Eof)?;
        if end > self.buf.len() {
            return Err(DecodeError::Eof);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    #[inline]
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    #[inline]
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    #[inline]
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    #[inline]
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    #[inline]
    pub fn varlen(&mut self) -> Result<usize, DecodeError> {
        let b = self.u8()? as usize;
        if b & 0x80 == 0 {
            Ok(b)
        } else {
            Ok(((b & 0x7f) << 8) | self.u8()? as usize)
        }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }
    /// Error if there are unread bytes.
    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(DecodeError::Invalid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varlen_roundtrip() {
        for n in [0usize, 1, 127, 128, 1199, 32767] {
            let mut w = Writer::default();
            w.varlen(n);
            assert_eq!(w.len(), if n < 128 { 1 } else { 2 });
            let mut r = Reader::new(w.as_slice());
            assert_eq!(r.varlen().unwrap(), n);
        }
    }
}
