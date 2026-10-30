//! Whether a captive portal is what stops encrypted DNS.
//!
//! A hotel's or a train's Wi-Fi lets nothing through until someone signs
//! in, DNS over HTTPS and TLS included, and the sign-in page needs names
//! resolved before anything else works. Android's strict Private DNS meets
//! the same problem by asking the network's own server while a portal is in
//! the way, and so does the forwarder (serve.rs), unless encrypted DNS is
//! strict. It asks only when every encrypted server failed, and it decides
//! with GNOME's check, the one Danube's sign-in window uses
//! (docs/danube.md, "Captive portals"): `nmcheck.gnome.org` looked up from
//! the network's server and fetched over plain HTTP. A portal intercepts
//! that fetch. A network that answers it as GNOME does is not a portal but
//! one that blocks encrypted DNS, and then nothing goes out in plain text.
//!
//! The trade-off is Android's too: whoever runs the network can fake a
//! portal and read names in plain text until it stops, which is why strict
//! mode leaves this out.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

/// GNOME's connectivity check: the host, the path, and the body it serves
/// when nothing is in the way.
pub const CHECK_HOST: &str = "nmcheck.gnome.org";
const CHECK_PATH: &str = "/check_network_status.txt";
const CHECK_BODY: &str = "NetworkManager is online";
const TIMEOUT: Duration = Duration::from_secs(3);

/// What the check found.
#[derive(Debug, PartialEq, Eq)]
pub enum Network {
    /// Something answered the check other than GNOME: a sign-in page.
    Portal,
    /// The check got through: the network works and blocks encrypted DNS.
    Open,
    /// The check got nowhere: no network to speak of.
    Down,
}

/// Runs the check through the network's own DNS `servers`.
pub fn check(servers: &[SocketAddr]) -> Network {
    let Some(ip) = servers
        .iter()
        .find_map(|s| lookup(*s, CHECK_HOST).ok().flatten())
    else {
        return Network::Down;
    };
    match fetch(SocketAddr::new(IpAddr::V4(ip), 80)) {
        Ok(body) if body.contains(CHECK_BODY) => Network::Open,
        Ok(_) => Network::Portal,
        Err(_) => Network::Down,
    }
}

/// The first IPv4 address `server` gives for `name`, over plain UDP.
fn lookup(server: SocketAddr, name: &str) -> io::Result<Option<Ipv4Addr>> {
    let mut query = vec![0x4c, 0x6f, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect(server)?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.send(&query)?;
    let mut buf = [0u8; 1500];
    let n = socket.recv(&mut buf)?;
    Ok(first_a(&buf[..n], &query[..2]))
}

/// The first A record in the answer section of `reply` to the query with
/// `id`.
pub fn first_a(reply: &[u8], id: &[u8]) -> Option<Ipv4Addr> {
    if reply.len() < 12 || &reply[..2] != id {
        return None;
    }
    let questions = u16::from_be_bytes([reply[4], reply[5]]);
    let answers = u16::from_be_bytes([reply[6], reply[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(reply, at)? + 4;
    }
    for _ in 0..answers {
        at = skip_name(reply, at)?;
        let kind = u16::from_be_bytes([*reply.get(at)?, *reply.get(at + 1)?]);
        let len = u16::from_be_bytes([*reply.get(at + 8)?, *reply.get(at + 9)?]) as usize;
        at += 10;
        let data = reply.get(at..at + len)?;
        if kind == 1 && len == 4 {
            return Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        at += len;
    }
    None
}

/// Where the name at `at` ends, labels or a compression pointer.
fn skip_name(packet: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *packet.get(at)? as usize;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(at + 2);
        }
        at += 1 + len;
    }
}

/// The check page's response from `addr`, status line, headers and body,
/// at most a few kilobytes of it.
fn fetch(addr: SocketAddr) -> io::Result<String> {
    let mut tcp = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    write!(
        tcp,
        "GET {CHECK_PATH} HTTP/1.1\r\nHost: {CHECK_HOST}\r\nConnection: close\r\n\r\n"
    )?;
    let mut body = Vec::new();
    tcp.take(16 * 1024).read_to_end(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_address_past_a_cname() {
        // nmcheck.gnome.org CNAME x.example, x.example A 192.0.2.7, the
        // answers' names compressed back to the question.
        let mut reply = vec![0xab, 0xcd, 0x81, 0x80, 0, 1, 0, 2, 0, 0, 0, 0];
        reply.extend_from_slice(b"\x07nmcheck\x05gnome\x03org\x00\x00\x01\x00\x01");
        reply.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 11]);
        reply.extend_from_slice(b"\x01x\x07example\x00");
        reply.extend_from_slice(&[0xc0, 47, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 192, 0, 2, 7]);
        assert_eq!(
            first_a(&reply, &[0xab, 0xcd]),
            Some(Ipv4Addr::new(192, 0, 2, 7))
        );
        assert_eq!(first_a(&reply, &[0, 0]), None);
    }
}
