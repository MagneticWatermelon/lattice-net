//! Rayon's workers, kept awake through a tick.
//!
//! An idle rayon worker yields a few dozen times and then sleeps, so the
//! serial steps between a tick's phases (the grid rebuild, hit application,
//! ...) put the pool to sleep, and each parallel pass then waits for its
//! workers to wake. On WSL that cost a pass ~0.2 ms at p50 and ~0.5 ms at p90,
//! against ~0.05 ms with the workers awake: more than many passes' work.
//! `awake` keeps the other workers looking for work until the tick is done;
//! between ticks they sleep as usual.
//!
//! A held worker runs a broadcast job that loops until the tick is done.
//! That job must be the bottom frame on its worker: a worker that picked it
//! up while waiting inside a join (for the other half of a task it stole)
//! would loop on top of that unfinished task, which the tick needs in order
//! to end, and the tick would hang. So the other workers start their jobs
//! before `f` makes any tasks, and then a gate closes: a job that starts
//! later returns at once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// State bits: the gate is closed (jobs that start now return at once), and
/// `f` is done (held workers let go). The rest counts jobs that started.
const CLOSED: usize = 1 << (usize::BITS - 2);
const DONE: usize = 1 << (usize::BITS - 1);
/// How long `awake` waits for the other workers to start their jobs (woken
/// from sleep, they take tens of microseconds). One that starts later just
/// isn't held this time.
const START_WAIT: Duration = Duration::from_millis(1);

/// Runs `f` on a worker of the current rayon pool while every other worker
/// keeps looking for work (yielding to the OS when there's none), so `f`'s
/// parallel passes start at once instead of waking sleeping workers. Call it
/// from outside the pool, once per tick, with nothing else running on the
/// pool: its threads are busy until `f` returns (or panics). It assumes the
/// machine is the server's alone: two servers keeping their workers awake
/// on the same cores would starve each other.
pub fn awake<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    hold(f, None)
}

/// `awake`, counting the workers held in `held` (for tests).
fn hold<R: Send>(f: impl FnOnce() -> R + Send, held: Option<Arc<AtomicUsize>>) -> R {
    rayon::scope(|_| {
        let me = rayon::current_thread_index();
        let others = rayon::current_num_threads() - 1;
        let state = Arc::new(AtomicUsize::new(0));
        let st = Arc::clone(&state);
        rayon::spawn_broadcast(move |ctx| {
            // Never on f's own worker (it would wait for itself), and never
            // once the gate is closed (this worker may be inside f's tasks).
            if Some(ctx.index()) == me || st.fetch_add(1, Ordering::AcqRel) & CLOSED != 0 {
                return;
            }
            if let Some(h) = &held {
                h.fetch_add(1, Ordering::Relaxed);
            }
            while st.load(Ordering::Acquire) & DONE == 0 {
                if rayon::yield_now() == Some(rayon::Yield::Idle) {
                    std::thread::yield_now();
                }
            }
            if let Some(h) = &held {
                h.fetch_sub(1, Ordering::Relaxed);
            }
        });
        let start = Instant::now();
        while state.load(Ordering::Acquire) & !(CLOSED | DONE) < others && start.elapsed() < START_WAIT {
            std::hint::spin_loop();
        }
        state.fetch_or(CLOSED, Ordering::AcqRel);
        // Released when f returns or unwinds alike.
        struct Release(Arc<AtomicUsize>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.fetch_or(DONE, Ordering::Release);
            }
        }
        let _release = Release(state);
        f()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;

    fn until(what: &str, cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting until {what}");
            std::thread::yield_now();
        }
    }

    fn spin(us: u64) {
        let t = Instant::now();
        while t.elapsed() < Duration::from_micros(us) {}
    }

    #[test]
    fn the_other_workers_are_held_until_f_returns() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        let held = Arc::new(AtomicUsize::new(0));
        // All three are held from the first call, unless one can't start
        // within START_WAIT (a loaded machine): then it isn't, so try again.
        let all = (0..50).any(|_| {
            let h = Arc::clone(&held);
            let (all, sum) = pool.install(|| {
                hold(
                    || {
                        let t = Instant::now();
                        while h.load(Ordering::Relaxed) < 3 && t.elapsed() < Duration::from_millis(100) {
                            std::thread::yield_now();
                        }
                        // Passes still run (the held workers take their tasks).
                        (h.load(Ordering::Relaxed) == 3, (0..10_000u64).into_par_iter().sum::<u64>())
                    },
                    Some(Arc::clone(&held)),
                )
            });
            assert_eq!(sum, 49_995_000);
            until("all are released", || held.load(Ordering::Relaxed) == 0);
            all
        });
        assert!(all, "the other three workers were never all held");
    }

    #[test]
    fn a_panic_releases_the_workers() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        let held = Arc::new(AtomicUsize::new(0));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.install(|| hold(|| -> () { panic!("a tick panicked (this test expects it)") }, Some(Arc::clone(&held))))
        }));
        assert!(r.is_err());
        until("all are released", || held.load(Ordering::Relaxed) == 0);
    }

    /// Short ticks of small tasks, with gaps that leave workers idle, awake
    /// or asleep as a tick starts: a held worker must never sit on top of a
    /// task the tick waits for. (Without the gate this hung within a few
    /// thousand ticks, often within a few hundred.)
    #[test]
    fn ticks_never_wait_on_a_held_worker() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
            for i in 0..5000 {
                let tick = || (0..256usize).into_par_iter().with_max_len(4).map(|x| (spin(2), x).1).sum::<usize>();
                assert_eq!(pool.install(|| awake(tick)), 255 * 256 / 2);
                spin(i % 50);
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(120)).expect("a tick hung");
    }

    /// The stress run that caught the hang, at full length and against
    /// competing load. The gate depends on how rayon runs broadcast jobs
    /// (inside joins and `yield_now`), so run it after upgrading rayon.
    #[test]
    #[ignore = "long (minutes): cargo test --release -p lattice-sim pool -- --ignored, after upgrading rayon"]
    fn many_ticks_under_load_never_wait_on_a_held_worker() {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let load: Vec<_> = (0..8)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        spin(50);
                        std::thread::yield_now();
                    }
                })
            })
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
            for i in 0..150_000 {
                let tick = || (0..256usize).into_par_iter().with_max_len(4).map(|x| (spin(2), x).1).sum::<usize>();
                assert_eq!(pool.install(|| awake(tick)), 255 * 256 / 2);
                spin(i % 50);
            }
            tx.send(()).unwrap();
        });
        let done = rx.recv_timeout(Duration::from_secs(1800));
        stop.store(true, Ordering::Relaxed);
        load.into_iter().for_each(|t| t.join().unwrap());
        done.expect("a tick hung");
    }
}
