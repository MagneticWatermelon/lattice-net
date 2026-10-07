//! Rayon's workers, kept awake through a tick.
//!
//! An idle rayon worker yields a few dozen times and then sleeps, so the
//! serial steps between a tick's phases (the grid rebuild, hit application,
//! ...) put the pool to sleep, and each parallel pass then waits for its
//! workers to wake. On WSL that cost a pass ~0.2 ms at p50 and ~0.5 ms at p90,
//! against ~0.05 ms with the workers awake: more than many passes' work.
//! `awake` keeps the other workers looking for work until the tick is done;
//! between ticks they sleep as usual.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Runs `f` on a worker of the current rayon pool while every other worker
/// keeps looking for work (yielding to the OS when there's none), so `f`'s
/// parallel passes start at once instead of waking sleeping workers. Call it
/// from outside the pool, once per tick: the pool's threads are busy until
/// `f` returns (or panics).
pub fn awake<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    hold(f, None)
}

/// `awake`, counting the workers held in `held` (for tests).
fn hold<R: Send>(f: impl FnOnce() -> R + Send, held: Option<Arc<AtomicUsize>>) -> R {
    rayon::scope(|_| {
        let me = rayon::current_thread_index();
        let done = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&done);
        rayon::spawn_broadcast(move |ctx| {
            // Never on f's own worker: there it would wait for itself.
            if Some(ctx.index()) == me {
                return;
            }
            if let Some(h) = &held {
                h.fetch_add(1, Ordering::Relaxed);
            }
            while !stop.load(Ordering::Acquire) {
                if rayon::yield_now() == Some(rayon::Yield::Idle) {
                    std::thread::yield_now();
                }
            }
            if let Some(h) = &held {
                h.fetch_sub(1, Ordering::Relaxed);
            }
        });
        // Released when f returns or unwinds alike.
        struct Release(Arc<AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _release = Release(done);
        f()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;
    use std::time::{Duration, Instant};

    fn until(what: &str, cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting until {what}");
            std::thread::yield_now();
        }
    }

    #[test]
    fn the_other_workers_are_held_until_f_returns() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        let held = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&held);
        let sum = pool.install(|| {
            hold(
                || {
                    until("the other 3 workers are held", || h.load(Ordering::Relaxed) == 3);
                    // Passes still run (the held workers take their tasks).
                    (0..10_000u64).into_par_iter().sum::<u64>() + (0..100u64).into_par_iter().map(|i| i * 2).sum::<u64>()
                },
                Some(Arc::clone(&held)),
            )
        });
        assert_eq!(sum, 49_995_000 + 9_900);
        until("all are released", || held.load(Ordering::Relaxed) == 0);
        // And again: each call holds and releases its own.
        let n = pool.install(|| hold(|| (0..1000).into_par_iter().count(), Some(Arc::clone(&held))));
        assert_eq!(n, 1000);
        until("all are released again", || held.load(Ordering::Relaxed) == 0);
    }

    #[test]
    fn a_panic_releases_the_workers() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        let held = Arc::new(AtomicUsize::new(0));
        let h = Arc::clone(&held);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.install(|| {
                hold(
                    || {
                        until("the other 2 workers are held", || h.load(Ordering::Relaxed) == 2);
                        panic!("a tick panicked (this test expects it)");
                    },
                    Some(Arc::clone(&held)),
                )
            })
        }));
        assert!(r.is_err());
        until("all are released", || held.load(Ordering::Relaxed) == 0);
    }
}
