//! Batched UDP receive for the binaries: `recvmmsg` with the kernel's arrival
//! timestamps (`SO_TIMESTAMPNS`) on Linux, one `recv_from` per call elsewhere.
//! The protocol code never sees a socket; this is socket-loop plumbing shared by
//! `lattice-server` and `lattice-bots`.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Instant, SystemTime};

/// Largest datagram received; the transport never sends more.
pub const MAX_DATAGRAM: usize = 1500;

/// Receive buffers for up to `capacity` datagrams per call, reused across calls.
pub struct RecvBatch {
    bufs: Vec<[u8; MAX_DATAGRAM]>,
    lens: Vec<usize>,
    from: Vec<Option<SocketAddr>>,
    stamps: Vec<Option<SystemTime>>,
    #[cfg(target_os = "linux")]
    names: Vec<libc::sockaddr_storage>,
    #[cfg(target_os = "linux")]
    control: Vec<[u64; 8]>,
}

impl RecvBatch {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            bufs: vec![[0; MAX_DATAGRAM]; capacity],
            lens: vec![0; capacity],
            from: vec![None; capacity],
            stamps: vec![None; capacity],
            // SAFETY: sockaddr_storage is plain data; all-zero is valid.
            #[cfg(target_os = "linux")]
            names: vec![unsafe { std::mem::zeroed() }; capacity],
            #[cfg(target_os = "linux")]
            control: vec![[0; 8]; capacity],
        }
    }

    pub fn capacity(&self) -> usize {
        self.bufs.len()
    }

    /// Datagram `i` of the last `recv`: its bytes, sender and the kernel's
    /// arrival time (wall clock; `Clocks::instant_of` converts it), when known.
    pub fn get(&self, i: usize) -> (&[u8], Option<SocketAddr>, Option<SystemTime>) {
        (&self.bufs[i][..self.lens[i]], self.from[i], self.stamps[i])
    }

    /// Receives up to `capacity` datagrams in one `recvmmsg`. With `wait`, it
    /// blocks (up to the socket's read timeout) for the first one and then takes
    /// whatever else is already queued (`MSG_WAITFORONE`); without, it never
    /// blocks (`WouldBlock` when nothing is queued).
    #[cfg(target_os = "linux")]
    pub fn recv(&mut self, sock: &UdpSocket, wait: bool) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd;
        let cap = self.capacity();
        let mut iovs: Vec<libc::iovec> = self
            .bufs
            .iter_mut()
            .map(|b| libc::iovec { iov_base: b.as_mut_ptr() as *mut libc::c_void, iov_len: b.len() })
            .collect();
        let (iov_base, name_base, control_base) = (iovs.as_mut_ptr(), self.names.as_mut_ptr(), self.control.as_mut_ptr());
        let mut msgs: Vec<libc::mmsghdr> = (0..cap)
            .map(|i| {
                // SAFETY: msghdr is plain data; all-zero is a valid empty header.
                let mut h: libc::msghdr = unsafe { std::mem::zeroed() };
                // SAFETY: i < cap, the length of iovs, names and control.
                unsafe {
                    h.msg_name = name_base.add(i) as *mut libc::c_void;
                    h.msg_iov = iov_base.add(i);
                    h.msg_control = control_base.add(i) as *mut libc::c_void;
                }
                h.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
                h.msg_iovlen = 1;
                h.msg_controllen = std::mem::size_of::<[u64; 8]>() as _;
                libc::mmsghdr { msg_hdr: h, msg_len: 0 }
            })
            .collect();
        let flags = if wait { libc::MSG_WAITFORONE } else { libc::MSG_DONTWAIT };
        // SAFETY: every header points into `iovs` (into `self.bufs`), `self.names`
        // and `self.control`, all alive and unmoved for the duration of the call.
        let n = unsafe { libc::recvmmsg(sock.as_raw_fd(), msgs.as_mut_ptr(), cap as u32, flags, std::ptr::null_mut()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for (i, m) in msgs.iter().enumerate().take(n as usize) {
            self.lens[i] = m.msg_len as usize;
            // SAFETY: the kernel wrote a sockaddr of msg_namelen bytes into names[i].
            self.from[i] = unsafe { socket2::SockAddr::new(self.names[i], m.msg_hdr.msg_namelen) }.as_socket();
            self.stamps[i] = None;
            // SAFETY: walking the control messages the kernel just wrote.
            unsafe {
                let mut c = libc::CMSG_FIRSTHDR(&m.msg_hdr);
                while !c.is_null() {
                    if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_TIMESTAMPNS {
                        let ts: libc::timespec = std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const libc::timespec);
                        self.stamps[i] =
                            Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32));
                    }
                    c = libc::CMSG_NXTHDR(&m.msg_hdr, c);
                }
            }
        }
        Ok(n as usize)
    }

    /// One `recv_from`: blocking or not as the socket is set (`wait` is ignored).
    #[cfg(not(target_os = "linux"))]
    pub fn recv(&mut self, sock: &UdpSocket, _wait: bool) -> std::io::Result<usize> {
        let (n, from) = sock.recv_from(&mut self.bufs[0])?;
        (self.lens[0], self.from[0], self.stamps[0]) = (n, Some(from), None);
        Ok(1)
    }
}

/// Has the kernel stamp each datagram's arrival (`SO_TIMESTAMPNS`), read by
/// `RecvBatch::recv`. A no-op off Linux.
pub fn enable_rx_timestamps(sock: &UdpSocket) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let on: libc::c_int = 1;
        // SAFETY: a plain setsockopt with a valid fd and an int option value.
        let r = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TIMESTAMPNS,
                &on as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = sock;
    Ok(())
}

/// Converts kernel receive timestamps (wall clock) to `Instant`s, using one
/// pair of clock readings.
#[derive(Debug, Clone, Copy)]
pub struct Clocks {
    pub instant: Instant,
    pub system: SystemTime,
}

impl Clocks {
    pub fn now() -> Self {
        Self { instant: Instant::now(), system: SystemTime::now() }
    }

    /// `t` as an `Instant`; never later than the reading.
    pub fn instant_of(&self, t: SystemTime) -> Instant {
        self.system.duration_since(t).map_or(self.instant, |ago| self.instant.checked_sub(ago).unwrap_or(self.instant))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_carries_senders_bytes_and_arrival_times() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        enable_rx_timestamps(&rx).unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        let (a, b) = (UdpSocket::bind("127.0.0.1:0").unwrap(), UdpSocket::bind("127.0.0.1:0").unwrap());
        let before = SystemTime::now();
        for i in 0..5u8 {
            let s = if i % 2 == 0 { &a } else { &b };
            s.send_to(&vec![i; 10 + i as usize], rx.local_addr().unwrap()).unwrap();
        }
        let mut batch = RecvBatch::new(4);
        let mut got = Vec::new();
        while got.len() < 5 {
            let n = batch.recv(&rx, true).unwrap();
            assert!(n <= 4);
            for i in 0..n {
                let (data, from, stamp) = batch.get(i);
                got.push((data.to_vec(), from.unwrap()));
                if cfg!(target_os = "linux") {
                    let t = stamp.expect("kernel timestamp");
                    assert!(t >= before - std::time::Duration::from_millis(5) && t <= SystemTime::now());
                }
            }
        }
        for (i, (data, from)) in got.iter().enumerate() {
            assert_eq!(*data, vec![i as u8; 10 + i]);
            assert_eq!(*from, if i % 2 == 0 { a.local_addr().unwrap() } else { b.local_addr().unwrap() });
        }
        // Nothing queued: a non-waiting receive returns at once.
        rx.set_nonblocking(true).unwrap();
        assert_eq!(batch.recv(&rx, false).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn clocks_convert_wall_time_to_instants() {
        let c = Clocks::now();
        let ago = std::time::Duration::from_millis(30);
        assert_eq!(c.instant_of(c.system - ago), c.instant - ago);
        // A stamp ahead of the reading (wall clock stepped) is taken as now.
        assert_eq!(c.instant_of(c.system + ago), c.instant);
    }
}
