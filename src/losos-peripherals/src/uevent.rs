//! The kernel's device events, straight from its netlink socket.
//!
//! udev would hand them on only after its own rules ran, and the router has
//! to see a USB device before anything acts on it. Only messages the kernel
//! itself sent are read: a process may not send to this group, and one that
//! did would not have the kernel's port id, 0.

use std::collections::HashMap;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// One event: `ACTION`, `DEVPATH`, `SUBSYSTEM`, `DEVTYPE` and the rest.
#[derive(Debug, Default)]
pub struct Event(pub HashMap<String, String>);

impl Event {
    pub fn get(&self, key: &str) -> &str {
        self.0.get(key).map(String::as_str).unwrap_or("")
    }

    /// Parses one message: `action@devpath`, then `KEY=value` strings, each
    /// ended by a NUL.
    pub fn parse(msg: &[u8]) -> Option<Event> {
        let mut parts = msg.split(|&b| b == 0).filter(|p| !p.is_empty());
        let head = std::str::from_utf8(parts.next()?).ok()?;
        head.split_once('@')?;
        let map = parts
            .filter_map(|p| std::str::from_utf8(p).ok())
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Some(Event(map))
    }
}

pub struct Socket(OwnedFd);

impl Socket {
    pub fn open() -> io::Result<Socket> {
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut a: libc::sockaddr_nl = unsafe { mem::zeroed() };
        a.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // Group 1: the kernel's own events (2 is udev's re-sends).
        a.nl_groups = 1;
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &a as *const _ as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Socket(fd))
    }

    /// The next event the kernel sent.
    pub fn next(&self) -> io::Result<Event> {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let mut from: libc::sockaddr_nl = unsafe { mem::zeroed() };
            let mut len = mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            let n = unsafe {
                libc::recvfrom(
                    self.0.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                    &mut from as *mut _ as *mut libc::sockaddr,
                    &mut len,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::Interrupted => continue,
                    // ENOBUFS: events were dropped while the router was
                    // busy; the ones that follow are still good.
                    _ if e.raw_os_error() == Some(libc::ENOBUFS) => {
                        tracing::warn!("missed device events");
                        continue;
                    }
                    _ => return Err(e),
                }
            }
            if from.nl_pid != 0 {
                continue;
            }
            if let Some(ev) = Event::parse(&buf[..n as usize]) {
                return Ok(ev);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_usb_add() {
        let msg = b"add@/devices/pci0000:00/0000:00:14.0/usb1/1-2\0ACTION=add\0\
                    DEVPATH=/devices/pci0000:00/0000:00:14.0/usb1/1-2\0SUBSYSTEM=usb\0\
                    DEVTYPE=usb_device\0SEQNUM=4242\0";
        let ev = Event::parse(msg).unwrap();
        assert_eq!(ev.get("ACTION"), "add");
        assert_eq!(ev.get("DEVTYPE"), "usb_device");
        assert_eq!(ev.get("MISSING"), "");
    }

    #[test]
    fn libudev_messages_are_not_events() {
        assert!(Event::parse(b"libudev\0\xfe\xed\xca\xfe").is_none());
    }
}
