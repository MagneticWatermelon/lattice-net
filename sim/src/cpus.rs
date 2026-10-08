//! Where threads run, on Linux: CPU lists (`0-3,8`), pinning the calling
//! thread, and how long the scheduler kept a thread waiting.
//!
//! The server's rayon workers keep their cores busy through each tick
//! (`pool::awake`), so its receive threads and the network card's interrupt
//! work compete with them for cores. `lattice-server --worker-cpus` and
//! `--rx-cpus` pin each side to its own CPUs (with `scripts/irq-affinity.sh`
//! steering the card's interrupts to the receive side), and the summary
//! reports how long the receive threads waited for a core and how much CPU
//! the kernel's softirq threads (`ksoftirqd`, where interrupt work goes when
//! it overflows) used, so a run says whether pinning is needed.

use std::time::Duration;

/// A CPU list as the kernel writes them: `0-3,8,10-11`.
pub fn parse_list(s: &str) -> Result<Vec<usize>, String> {
    let mut cpus = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let num = |n: &str| n.trim().parse::<usize>().map_err(|_| format!("bad CPU {n:?} in {s:?}"));
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (num(a)?, num(b)?);
                if a > b {
                    return Err(format!("backwards range {part:?} in {s:?}"));
                }
                cpus.extend(a..=b);
            }
            None => cpus.push(num(part)?),
        }
    }
    if cpus.is_empty() {
        return Err(format!("no CPUs in {s:?}"));
    }
    cpus.sort_unstable();
    cpus.dedup();
    Ok(cpus)
}

/// Pins the calling thread to `cpus`.
#[cfg(target_os = "linux")]
pub fn pin(cpus: &[usize]) -> std::io::Result<()> {
    // SAFETY: a zeroed cpu_set_t is a valid empty set; CPU_SET bounds-checks
    // against CPU_SETSIZE below; pid 0 is the calling thread.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus {
            if c >= libc::CPU_SETSIZE as usize {
                return Err(std::io::Error::other(format!("CPU {c} is past CPU_SETSIZE")));
            }
            libc::CPU_SET(c, &mut set);
        }
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn pin(_cpus: &[usize]) -> std::io::Result<()> {
    Err(std::io::Error::other("pinning threads needs Linux"))
}

/// The CPUs the calling thread may run on.
#[cfg(target_os = "linux")]
pub fn affinity() -> std::io::Result<Vec<usize>> {
    // SAFETY: as in `pin`; the kernel fills `set`.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((0..libc::CPU_SETSIZE as usize).filter(|&c| libc::CPU_ISSET(c, &set)).collect())
    }
}

/// The calling thread's kernel id, for reading its `/proc` entries.
#[cfg(target_os = "linux")]
pub fn tid() -> i32 {
    // SAFETY: gettid takes no arguments and can't fail.
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}

#[cfg(not(target_os = "linux"))]
pub fn tid() -> i32 {
    0
}

/// How long thread `tid` of this process has waited on a run queue
/// (runnable, but another thread had its core). `None` without schedstats
/// (`/proc/<pid>/task/<tid>/schedstat`, CONFIG_SCHED_INFO).
pub fn runq_wait(tid: i32) -> Option<Duration> {
    let s = std::fs::read_to_string(format!("/proc/self/task/{tid}/schedstat")).ok()?;
    // "run time (ns), run-queue wait (ns), time slices"
    s.split_whitespace().nth(1)?.parse().ok().map(Duration::from_nanos)
}

/// CPU time the kernel's softirq threads (`ksoftirqd/N`) have used, summed:
/// network interrupt work they took over because it didn't fit in the
/// interrupts themselves. `None` where `/proc` doesn't say.
#[cfg(target_os = "linux")]
pub fn ksoftirqd_cpu() -> Option<Duration> {
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let mut ticks = 0u64;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().filter(|n| n.bytes().all(|b| b.is_ascii_digit())) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        // "pid (comm) state ...": comm can hold spaces, so split after its ')'.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else { continue };
        if !stat[open + 1..close].starts_with("ksoftirqd/") {
            continue;
        }
        // After the comm: state is field 3, utime 14 and stime 15.
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        let (Some(u), Some(s)) = (fields.get(11), fields.get(12)) else { continue };
        ticks += u.parse::<u64>().unwrap_or(0) + s.parse::<u64>().unwrap_or(0);
    }
    Some(Duration::from_secs_f64(ticks as f64 / hz as f64))
}

#[cfg(not(target_os = "linux"))]
pub fn ksoftirqd_cpu() -> Option<Duration> {
    None
}

/// The CPUs for thread `i` of `n` given a list: one each when the list has
/// exactly one per thread, else the whole list (the scheduler places them).
pub fn for_thread(list: &[usize], i: usize, n: usize) -> &[usize] {
    if list.len() == n {
        &list[i..=i]
    } else {
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_lists_parse_like_the_kernels() {
        assert_eq!(parse_list("0-3,8,10-11"), Ok(vec![0, 1, 2, 3, 8, 10, 11]));
        assert_eq!(parse_list(" 5 , 1-2,2 "), Ok(vec![1, 2, 5]));
        assert!(parse_list("").is_err());
        assert!(parse_list("3-1").is_err());
        assert!(parse_list("a").is_err());
        assert_eq!(for_thread(&[4, 5, 6], 1, 3), &[5]);
        assert_eq!(for_thread(&[4, 5, 6], 1, 8), &[4, 5, 6]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_pinned_thread_stays_pinned_and_its_waits_are_read() {
        let mine = affinity().unwrap();
        let first = mine[0];
        std::thread::spawn(move || {
            pin(&[first]).unwrap();
            assert_eq!(affinity().unwrap(), vec![first]);
            // Busy a moment, so the scheduler has something to count.
            let t = std::time::Instant::now();
            while t.elapsed() < Duration::from_millis(5) {}
            // Kernels without schedstats say nothing; with them, a number.
            if let Some(w) = runq_wait(tid()) {
                assert!(w < Duration::from_secs(5));
            }
        })
        .join()
        .unwrap();
        assert!(ksoftirqd_cpu().is_some(), "every Linux has ksoftirqd threads");
        assert!(pin(&[usize::MAX]).is_err());
    }
}
