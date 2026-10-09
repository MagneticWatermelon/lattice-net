//! Distant fights: firing activity per map cell, so a battle beyond the near
//! tier (which alone gets exact tracers) is still seen. The server counts
//! every shot into its 128 m cell, per faction, over a window of ticks; each
//! client gets the finished window's active cells within its far radius once
//! per window, and draws that many ambient tracers there.
//!
//! ```text
//! S->C unreliable  Activity  tag | step:4 | len:1 | n:1 | n × entry     (one or more per window)
//!   entry (5 B) := cell:2 | faction:2 shots:6 | yaw:1 | at:1
//!   cell     cx + cy × 64, 128 m cells
//!   shots    shots in the window, log-coded past 32 (`shots_code`), up to ~1,150
//!   yaw      their mean aim, in 1/256 turns
//!   at       their mean origin in the cell, 8 m resolution: x:4 | y:4 << 4
//!   step     the window's last game step; len: its length in steps
//! ```
//!
//! Every recipient gets the same entry bytes (serialized once per window).
//! It's cosmetic: nothing here touches hit detection.

use lattice_net::wire::{DecodeError, Reader, Writer};

use crate::movement::WORLD_SIZE;

pub const MSG_ACTIVITY: u8 = 8;
/// Cell edge, meters.
pub const CELL: f32 = 128.0;
/// Cells per axis.
pub const CELLS: u32 = (WORLD_SIZE / CELL) as u32;
const _: () = assert!(CELLS * CELLS <= 1 << 16, "a cell index fits in u16");
pub const ENTRY: usize = 5;
/// Same as `msg::ENTITIES_HEADER`, so `PacketFill` packs both alike.
pub const HEADER: usize = 1 + 4 + 1 + 1;
const _: () = assert!(HEADER == crate::msg::ENTITIES_HEADER);

pub type Entry = [u8; ENTRY];

/// One cell's firing, as decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellFire {
    pub cell: u16,
    pub faction: u8,
    pub shots: u32,
    /// Mean aim, radians.
    pub yaw: f32,
    /// Mean origin, x and y (to 8 m).
    pub at: [f32; 2],
}

/// The cell `pos` is in.
pub fn cell_of(pos: [f32; 2]) -> u16 {
    let c = |v: f32| ((v / CELL).max(0.0) as u32).min(CELLS - 1);
    (c(pos[0]) + c(pos[1]) * CELLS) as u16
}

/// A cell's lower corner.
pub fn cell_origin(cell: u16) -> [f32; 2] {
    [(cell as u32 % CELLS) as f32 * CELL, (cell as u32 / CELLS) as f32 * CELL]
}

/// Shot counts in 6 bits: exact to 32, then 6 codes per doubling.
pub fn shots_code(n: u32) -> u8 {
    if n <= 32 {
        n as u8
    } else {
        (32.0 + (6.0 * libm::log2f(n as f32 / 32.0)).round()).min(63.0) as u8
    }
}

pub fn shots_from_code(c: u8) -> u32 {
    if c <= 32 {
        c as u32
    } else {
        (32.0 * libm::exp2f((c - 32) as f32 / 6.0)).round() as u32
    }
}

/// `at` is the mean origin, `yaw` the mean aim (radians).
pub fn encode_entry(cell: u16, faction: u8, shots: u32, yaw: f32, at: [f32; 2]) -> Entry {
    let o = cell_origin(cell);
    let q = |v: f32, o: f32| (((v - o) / (CELL / 16.0)) as i32).clamp(0, 15) as u8;
    let turn = (yaw.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * 256.0).round() as u32 as u8;
    let c = cell.to_le_bytes();
    [c[0], c[1], (faction & 3) << 6 | shots_code(shots), turn, q(at[0], o[0]) | q(at[1], o[1]) << 4]
}

pub fn decode_entry(e: &[u8]) -> CellFire {
    let cell = u16::from_le_bytes([e[0], e[1]]);
    let o = cell_origin(cell);
    let half = CELL / 32.0;
    CellFire {
        cell,
        faction: e[2] >> 6,
        shots: shots_from_code(e[2] & 63),
        yaw: e[3] as f32 / 256.0 * std::f32::consts::TAU,
        at: [o[0] + (e[4] & 15) as f32 * CELL / 16.0 + half, o[1] + (e[4] >> 4) as f32 * CELL / 16.0 + half],
    }
}

/// Most entries one message of at most `max_message` bytes holds.
pub fn per_message(max_message: usize) -> usize {
    ((max_message - HEADER) / ENTRY).min(u8::MAX as usize)
}

pub fn write_header(w: &mut Writer, step: u32, len: u8, n: u8) {
    w.u8(MSG_ACTIVITY);
    w.u32(step);
    w.u8(len);
    w.u8(n);
}

/// (window's last step, its length in steps, entries).
pub fn decode(data: &[u8]) -> Result<(u32, u8, Vec<CellFire>), DecodeError> {
    let mut r = Reader::new(data);
    if r.u8()? != MSG_ACTIVITY {
        return Err(DecodeError::Invalid);
    }
    let (step, len, n) = (r.u32()?, r.u8()?, r.u8()?);
    let body = r.take(n as usize * ENTRY)?;
    r.finish()?;
    Ok((step, len, body.as_chunks::<ENTRY>().0.iter().map(|e| decode_entry(e)).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_roundtrip_to_their_resolution() {
        let at = [3000.0, 1234.0];
        let cell = cell_of(at);
        let e = encode_entry(cell, 2, 17, 1.0, at);
        let d = decode_entry(&e);
        assert_eq!((d.cell, d.faction, d.shots), (cell, 2, 17));
        assert!((d.yaw - 1.0).abs() < 0.02, "{}", d.yaw);
        assert!((d.at[0] - at[0]).abs() <= 4.0 && (d.at[1] - at[1]).abs() <= 4.0, "{:?}", d.at);
        let mut w = Writer::with_capacity(HEADER + 2 * ENTRY);
        write_header(&mut w, 900, 15, 2);
        w.bytes(&e);
        w.bytes(&encode_entry(0, 0, 1, 0.0, [0.0, 0.0]));
        let (step, len, v) = decode(&w.into_inner()).unwrap();
        assert_eq!((step, len, v.len(), v[0]), (900, 15, 2, d));
    }

    #[test]
    fn shot_counts_are_exact_small_and_close_large() {
        for n in 0..=32 {
            assert_eq!(shots_from_code(shots_code(n)), n);
        }
        for n in [40, 100, 333, 1000] {
            let back = shots_from_code(shots_code(n)) as f32;
            assert!((back / n as f32 - 1.0).abs() < 0.07, "{n} -> {back}");
        }
        assert_eq!(shots_code(1_000_000), 63);
        assert!(shots_from_code(63) > 1100);
    }

    #[test]
    fn cells_cover_the_world() {
        assert_eq!(cell_of([0.0, 0.0]), 0);
        assert_eq!(cell_of([WORLD_SIZE, WORLD_SIZE]) as u32, CELLS * CELLS - 1);
        assert_eq!(cell_origin(cell_of([300.0, 520.0])), [256.0, 512.0]);
    }
}
