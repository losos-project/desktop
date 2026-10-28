//! mDNS between the host and the networks this machine joins, as Avahi's
//! reflector does: what one side multicasts to 224.0.0.251:5353, the other
//! sides hear, sent from this machine. The host is on no network of its own,
//! and its resolved finds a LosOS box's `.local` name, and the boxes
//! machine its `_losos._tcp`, through this. Never in schizo mode, where
//! nothing of the host's reaches the local network.

use std::io;
use std::mem::{size_of, zeroed};
use std::net::{Ipv4Addr, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, warn};

use crate::daemon::Shared;

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;

fn check(ret: libc::c_int) -> io::Result<()> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Port 5353 on every address, shared with anything else that listens
/// there, with the interface each packet came in on.
fn socket() -> io::Result<UdpSocket> {
    // SAFETY: plain socket calls; every pointer is to a local of the size
    // passed with it.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        check(fd)?;
        let socket = UdpSocket::from_raw_fd(fd);
        let one: libc::c_int = 1;
        for option in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
            check(libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&one as *const libc::c_int).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            ))?;
        }
        check(libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_PKTINFO,
            (&one as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        ))?;
        // Our own packets are not heard back.
        let zero: libc::c_int = 0;
        check(libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_LOOP,
            (&zero as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        ))?;
        let ttl: libc::c_int = 255;
        check(libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_TTL,
            (&ttl as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        ))?;
        let mut addr: libc::sockaddr_in = zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = PORT.to_be();
        check(libc::bind(
            fd,
            (&addr as *const libc::sockaddr_in).cast(),
            size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ))?;
        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        Ok(socket)
    }
}

fn index_of(name: &str) -> Option<i32> {
    std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn membership(socket: &UdpSocket, index: i32, join: bool) -> io::Result<()> {
    // SAFETY: as in socket().
    unsafe {
        let mut request: libc::ip_mreqn = zeroed();
        request.imr_multiaddr.s_addr = u32::from(GROUP).to_be();
        request.imr_ifindex = index;
        check(libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            if join {
                libc::IP_ADD_MEMBERSHIP
            } else {
                libc::IP_DROP_MEMBERSHIP
            },
            (&request as *const libc::ip_mreqn).cast(),
            size_of::<libc::ip_mreqn>() as libc::socklen_t,
        ))
    }
}

/// Receives one packet: its bytes and the interface it came in on.
fn receive(socket: &UdpSocket, buf: &mut [u8]) -> io::Result<(usize, i32)> {
    // SAFETY: msghdr points at buf and a control buffer, both locals that
    // outlive the call, with their sizes.
    unsafe {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        let mut control = [0u8; 64];
        let mut message: libc::msghdr = zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as _;
        let n = libc::recvmsg(socket.as_raw_fd(), &mut message, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut cmsg = libc::CMSG_FIRSTHDR(&message);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::IPPROTO_IP && (*cmsg).cmsg_type == libc::IP_PKTINFO {
                let info = libc::CMSG_DATA(cmsg)
                    .cast::<libc::in_pktinfo>()
                    .read_unaligned();
                return Ok((n as usize, info.ipi_ifindex));
            }
            cmsg = libc::CMSG_NXTHDR(&message, cmsg);
        }
        Ok((n as usize, 0))
    }
}

fn send_on(socket: &UdpSocket, index: i32, packet: &[u8]) -> io::Result<()> {
    // SAFETY: as in socket().
    unsafe {
        let mut request: libc::ip_mreqn = zeroed();
        request.imr_ifindex = index;
        check(libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            (&request as *const libc::ip_mreqn).cast(),
            size_of::<libc::ip_mreqn>() as libc::socklen_t,
        ))?;
    }
    socket.send_to(packet, (GROUP, PORT)).map(|_| ())
}

/// Reflects mDNS between the host's link and the cards for as long as the
/// machine runs, following the cards as they come and go.
pub fn reflect(shared: Arc<Shared>) {
    let socket = match socket() {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "no mDNS between the host and the networks");
            return;
        }
    };
    let mut joined: Vec<i32> = Vec::new();
    let mut buf = vec![0u8; 9000];
    loop {
        // Which interfaces, now: the host's link and every card with an
        // address, or none in schizo mode.
        let (up, cards) = shared.mdns_links();
        let mut want: Vec<i32> = Vec::new();
        let up = up.and_then(|u| index_of(&u));
        if let Some(up) = up {
            if !cards.is_empty() {
                want.push(up);
                want.extend(cards.iter().filter_map(|c| index_of(c)));
            }
        }
        for index in joined.iter().filter(|i| !want.contains(i)) {
            let _ = membership(&socket, *index, false);
        }
        joined.retain(|i| want.contains(i));
        for index in &want {
            if !joined.contains(index) && membership(&socket, *index, true).is_ok() {
                joined.push(*index);
            }
        }

        let (n, from) = match receive(&socket, &mut buf) {
            Ok(got) => got,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => {
                debug!(error = %e, "mDNS");
                continue;
            }
        };
        let Some(up) = up else { continue };
        if !joined.contains(&from) {
            continue;
        }
        // From the host to every network; from a network to the host only,
        // so one network's names do not leak onto another.
        let targets: Vec<i32> = if from == up {
            joined.iter().copied().filter(|i| *i != up).collect()
        } else {
            vec![up]
        };
        for target in targets {
            if let Err(e) = send_on(&socket, target, &buf[..n]) {
                debug!(error = %e, target, "mDNS");
            }
        }
    }
}
