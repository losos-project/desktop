//! Two streams joined: what either end sends, the other receives, until
//! one of them closes, and then both are shut so neither waits on the
//! other. The router joins a device's stream from the USB machine to the
//! machine it goes to this way, and a socket on the host to a machine's
//! sound server.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::thread;

pub fn splice<A, B>(a: &A, b: &B)
where
    A: AsRawFd + Sync,
    B: AsRawFd + Sync,
    for<'x> &'x A: Read + Write,
    for<'x> &'x B: Read + Write,
{
    fn shut(fd: &impl AsRawFd) {
        unsafe {
            libc::shutdown(fd.as_raw_fd(), libc::SHUT_RDWR);
        }
    }
    thread::scope(|s| {
        s.spawn(|| {
            let (mut from, mut to) = (a, b);
            let _ = io::copy(&mut from, &mut to);
            shut(a);
            shut(b);
        });
        let (mut from, mut to) = (b, a);
        let _ = io::copy(&mut from, &mut to);
        shut(a);
        shut(b);
    });
}
