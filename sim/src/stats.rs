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

/// Counts of non-negative integer samples in unit buckets up to `cap`, with
/// everything above `cap` in the last bucket (the max is still exact). For
/// streams too large to keep, like per-tick latencies of 10k bots.
#[derive(Debug, Clone)]
pub struct Histogram {
    counts: Vec<u64>,
    n: u64,
    sum: u64,
    max: u32,
}

impl Histogram {
    pub fn new(cap: u32) -> Self {
        Self { counts: vec![0; cap as usize + 1], n: 0, sum: 0, max: 0 }
    }

    pub fn record(&mut self, v: u32) {
        let last = self.counts.len() - 1;
        self.counts[(v as usize).min(last)] += 1;
        self.n += 1;
        self.sum += v as u64;
        self.max = self.max.max(v);
    }

    pub fn merge(&mut self, o: &Histogram) {
        for (a, b) in self.counts.iter_mut().zip(&o.counts) {
            *a += b;
        }
        self.n += o.n;
        self.sum += o.sum;
        self.max = self.max.max(o.max);
    }

    pub fn len(&self) -> u64 {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn mean(&self) -> f64 {
        self.sum as f64 / self.n.max(1) as f64
    }

    /// Nearest-rank percentiles, matching `summarize`.
    pub fn summary(&self) -> Summary {
        if self.n == 0 {
            return Summary::default();
        }
        let at = |p: f64| {
            let rank = ((self.n as f64 * p).ceil() as u64).clamp(1, self.n);
            let mut seen = 0;
            for (v, &c) in self.counts.iter().enumerate() {
                seen += c;
                if seen >= rank {
                    return v as u32;
                }
            }
            self.max
        };
        Summary { p50: at(0.50), p99: at(0.99), max: self.max }
    }
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

    #[test]
    fn histogram_matches_summarize() {
        let mut v: Vec<u32> = (0..1000).map(|i| (i * 7919) % 400).collect();
        let mut h = Histogram::new(1000);
        v.iter().for_each(|&x| h.record(x));
        let (a, b) = (summarize(&mut v), h.summary());
        assert_eq!((a.p50, a.p99, a.max), (b.p50, b.p99, b.max));
        let mut capped = Histogram::new(10);
        capped.record(3);
        capped.record(500);
        assert_eq!((capped.summary().p99, capped.summary().max), (10, 500));
    }
}

/// End-of-run results for scripts: one `key=value` per line, in insertion
/// order (`--summary PATH` on the server and the bots; `scripts/baseline.sh`
/// reads them). Values are plain numbers or words, never containing newlines.
#[derive(Debug, Default)]
pub struct KeyValues(Vec<(String, String)>);

impl KeyValues {
    pub fn put(&mut self, key: impl Into<String>, value: impl std::fmt::Display) {
        let (key, value) = (key.into(), value.to_string());
        debug_assert!(!key.contains(['=', '\n']) && !value.contains('\n'));
        self.0.push((key, value));
    }

    /// Milliseconds with two decimals, from microseconds.
    pub fn put_ms(&mut self, key: impl Into<String>, us: u32) {
        self.put(key, format!("{:.2}", us as f64 / 1000.0));
    }

    pub fn write(&self, path: &str) -> std::io::Result<()> {
        let text: String = self.0.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        std::fs::write(path, text)
    }
}
