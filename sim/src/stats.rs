//! Percentile summaries for per-tick samples.

#[derive(Debug, Clone, Copy, Default)]
pub struct Summary {
    pub p50: u32,
    pub p99: u32,
    pub max: u32,
}

/// Sorts `v` in place. Nearest-rank percentiles; all zero when empty.
pub fn summarize(v: &mut [u32]) -> Summary {
    if v.is_empty() {
        return Summary::default();
    }
    v.sort_unstable();
    let at = |p: f64| v[((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1];
    Summary { p50: at(0.50), p99: at(0.99), max: v[v.len() - 1] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank() {
        let mut v: Vec<u32> = (1..=200).rev().collect();
        let s = summarize(&mut v);
        assert_eq!((s.p50, s.p99, s.max), (100, 198, 200));
        assert_eq!(summarize(&mut [7]).p99, 7);
        assert_eq!(summarize(&mut []).max, 0);
    }
}
