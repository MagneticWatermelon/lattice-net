//! 16-bit wrapping sequence numbers and a fixed-size ring buffer indexed by them.

/// `a` is newer than `b`, accounting for wraparound (half-range comparison).
#[inline]
pub fn seq_gt(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// `a` is older than `b`, accounting for wraparound.
#[inline]
pub fn seq_lt(a: u16, b: u16) -> bool {
    seq_gt(b, a)
}

const EMPTY: u32 = u32::MAX;

/// Ring buffer of `size` entries keyed by a wrapping u16 sequence.
/// Entries older than `newest - size` are rejected. Inserting a newer sequence
/// clears the skipped slots, so stale data from a previous lap never leaks through.
pub struct SequenceBuffer<T> {
    /// Most recent inserted sequence + 1.
    sequence: u16,
    entry_seq: Vec<u32>,
    entries: Vec<Option<T>>,
}

impl<T> SequenceBuffer<T> {
    pub fn new(size: usize) -> Self {
        assert!(size.is_power_of_two() && size <= 0x8000, "size must be a power of two <= 32768");
        Self {
            sequence: 0,
            entry_seq: vec![EMPTY; size],
            entries: (0..size).map(|_| None).collect(),
        }
    }

    #[inline]
    fn size(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    fn idx(&self, s: u16) -> usize {
        s as usize & (self.size() - 1)
    }

    /// Most recent inserted sequence + 1.
    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    /// Returns false (and stores nothing) if `s` is too old for the window.
    pub fn insert(&mut self, s: u16, value: T) -> bool {
        if seq_lt(s, self.sequence.wrapping_sub(self.size() as u16)) {
            return false;
        }
        let next = s.wrapping_add(1);
        if seq_gt(next, self.sequence) {
            self.clear_range(self.sequence, s);
            self.sequence = next;
        }
        let i = self.idx(s);
        self.entry_seq[i] = s as u32;
        self.entries[i] = Some(value);
        true
    }

    /// Clears [start, end).
    fn clear_range(&mut self, start: u16, end: u16) {
        let n = end.wrapping_sub(start) as usize;
        if n >= self.size() {
            self.entry_seq.fill(EMPTY);
            self.entries.iter_mut().for_each(|e| *e = None);
            return;
        }
        for k in 0..n {
            let i = self.idx(start.wrapping_add(k as u16));
            self.entry_seq[i] = EMPTY;
            self.entries[i] = None;
        }
    }

    #[inline]
    pub fn exists(&self, s: u16) -> bool {
        self.entry_seq[self.idx(s)] == s as u32
    }

    pub fn get(&self, s: u16) -> Option<&T> {
        if self.exists(s) {
            self.entries[self.idx(s)].as_ref()
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, s: u16) -> Option<&mut T> {
        if self.exists(s) {
            let i = self.idx(s);
            self.entries[i].as_mut()
        } else {
            None
        }
    }

    pub fn remove(&mut self, s: u16) -> Option<T> {
        if self.exists(s) {
            let i = self.idx(s);
            self.entry_seq[i] = EMPTY;
            self.entries[i].take()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraparound_compare() {
        assert!(seq_gt(1, 0));
        assert!(seq_gt(0, 65535));
        assert!(seq_gt(100, 65000));
        assert!(!seq_gt(65000, 100));
        assert!(seq_lt(65535, 0));
        assert!(!seq_gt(5, 5));
    }

    #[test]
    fn buffer_window_and_wrap() {
        let mut b = SequenceBuffer::new(16);
        for s in 0u32..200_000 {
            let s = s as u16;
            assert!(b.insert(s, s));
            assert_eq!(b.get(s), Some(&s));
            // 16 behind is gone (slot reused)
            assert!(!b.exists(s.wrapping_sub(16)));
            // 15 behind still present
            if b.sequence() >= 16 || s > 15 {
                assert!(b.exists(s.wrapping_sub(15)));
            }
        }
        // too old
        let cur = b.sequence();
        assert!(!b.insert(cur.wrapping_sub(100), 0));
    }

    #[test]
    fn gap_clears_stale_slots() {
        let mut b = SequenceBuffer::new(8);
        for s in 0..8u16 {
            b.insert(s, s);
        }
        b.insert(20, 20); // jump ahead: everything in between must be empty
        for s in 13..20u16 {
            assert!(!b.exists(s));
        }
        assert!(b.exists(20));
    }
}
