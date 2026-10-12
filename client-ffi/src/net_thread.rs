//! The net thread (GDD §12.3, U3): the session runs on a plugin thread about
//! a thousand times a second (receive and deliver, the input clock with the
//! caller's latest intent, send), behind the session's lock. The caller's
//! frame then only sets the intent, pulls the trigger and draws: a stalled
//! frame (a shader compile, a scene load longer than the 5 s timeout) costs
//! no inputs, no stand-ins and no resync.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::handle::Handle;

/// How often the thread runs the session. Inputs go out within this of
/// their step; arrivals are delivered within this of landing.
pub const TICK: Duration = Duration::from_millis(1);

/// A session shared by the caller and the net thread.
pub type Shared = Arc<Mutex<Option<Handle>>>;

/// The session, whatever a panic elsewhere left the lock in (a call catches
/// its own panics and closes the session: nothing half-done stays behind).
pub fn lock(s: &Shared) -> MutexGuard<'_, Option<Handle>> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

pub struct NetThread {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    /// Why the thread closed the session or what it last failed at, for
    /// `lt_last_error`.
    error: Arc<Mutex<Option<String>>>,
}

impl NetThread {
    pub fn spawn(session: Shared) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (stop2, error2) = (Arc::clone(&stop), Arc::clone(&error));
        let join = std::thread::Builder::new().name("lattice-net".into()).spawn(move || run(&session, &stop2, &error2))?;
        Ok(Self { stop, join: Some(join), error })
    }

    /// Stops the thread and waits for it (a tick at most).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
}

impl Drop for NetThread {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(session: &Shared, stop: &AtomicBool, error: &Mutex<Option<String>>) {
    let wait = Wait::new();
    raise_priority();
    while !stop.load(Ordering::Acquire) {
        {
            let mut g = lock(session);
            let Some(h) = g.as_mut() else { return };
            // The time is read inside the lock: every call on the session,
            // from either thread, sees time move forward.
            let now = Instant::now();
            match catch_unwind(AssertUnwindSafe(|| h.net_tick(now))) {
                Ok(Ok(())) => {}
                // A receive error other than nothing to read: the session's
                // state shows what it does to the connection.
                Ok(Err(e)) => {
                    if let Ok(mut slot) = error.lock() {
                        *slot = Some(format!("net thread: receive: {e}"));
                    }
                }
                Err(p) => {
                    let text = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
                    if let Ok(mut slot) = error.lock() {
                        *slot = Some(format!("session closed after a panic on the net thread: {}", text.unwrap_or_else(|| "a panic".into())));
                    }
                    *g = None;
                    return;
                }
            }
        }
        wait.sleep(TICK);
    }
}

/// A sleep accurate to well under a millisecond. Windows' timer ticks every
/// 15.6 ms by default: a plain sleep there would hold inputs back that long,
/// so it uses a high-resolution waitable timer (Windows 10 1803 and later),
/// without changing the process's timer resolution.
#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::time::Duration;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateWaitableTimerExW(attributes: *mut c_void, name: *const u16, flags: u32, access: u32) -> *mut c_void;
        fn SetWaitableTimer(timer: *mut c_void, due: *const i64, period: i32, routine: *mut c_void, arg: *mut c_void, resume: i32) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetCurrentThread() -> *mut c_void;
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
    }

    const CREATE_WAITABLE_TIMER_HIGH_RESOLUTION: u32 = 0x2;
    const TIMER_ALL_ACCESS: u32 = 0x1F_0003;
    const INFINITE: u32 = u32::MAX;
    const THREAD_PRIORITY_ABOVE_NORMAL: i32 = 1;

    pub struct Wait(*mut c_void);

    impl Wait {
        pub fn new() -> Self {
            Self(unsafe { CreateWaitableTimerExW(null_mut(), null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS) })
        }

        pub fn sleep(&self, d: Duration) {
            if self.0.is_null() {
                std::thread::sleep(d);
                return;
            }
            // A relative due time is negative, in 100 ns units.
            let due = -((d.as_nanos() / 100).max(1) as i64);
            unsafe {
                if SetWaitableTimer(self.0, &due, 0, null_mut(), null_mut(), 0) == 0 {
                    std::thread::sleep(d);
                    return;
                }
                WaitForSingleObject(self.0, INFINITE);
            }
        }
    }

    impl Drop for Wait {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// Above the engine's own worker threads: a busy frame mustn't starve it.
    pub fn raise_priority() {
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
    }
}

#[cfg(not(windows))]
mod sys {
    use std::time::Duration;

    pub struct Wait;

    impl Wait {
        pub fn new() -> Self {
            Self
        }

        pub fn sleep(&self, d: Duration) {
            std::thread::sleep(d);
        }
    }

    pub fn raise_priority() {}
}

use sys::{raise_priority, Wait};
