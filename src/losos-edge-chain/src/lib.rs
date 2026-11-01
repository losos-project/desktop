//! Takes a connection the schizo gateway caught and carries it out through
//! the person's LosOS edge, over Tor (docs/schizo-mode.md, "Through an
//! edge").
//!
//! The gateway's firewall redirects every TCP connection from the inside
//! network here while the host has chosen an edge. Each one becomes two
//! SOCKS5 handshakes on one stream: the first asks the gateway's own Tor to
//! reach the edge's onion service, the second asks the edge's exit, at the
//! other end of that circuit, for the address the connection was meant for.
//! The traffic then leaves from the edge, never from a Tor exit, and the
//! edge sees only that it came over Tor.
//!
//! Everything here is the wire and the edge file; `main.rs` is the
//! listener.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// Where the host's choice reaches the gateway. The host writes
/// /run/losos-schizo/edge/edge.json and shares that folder read-only, which
/// the gateway mounts here.
pub const EDGE_FILE: &str = "/run/losos-edge/edge.json";

/// How long Tor gets to build a circuit to the onion service and the edge
/// to answer for the address. A fresh onion circuit can take half a minute.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(90);

/// The edge the host chose, as the network machine enrolled it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// The edge's v3 onion address, `<56 characters>.onion`.
    pub onion: String,
    /// The port its SOCKS5 exit answers on, on that address.
    pub port: u16,
    /// What the edge gave this computer at enrolment, which its exit asks
    /// for on every connection.
    pub username: String,
    pub password: String,
}

impl Edge {
    /// Reads the edge file. Any fault is an error, and the caller refuses
    /// the connection: with the file gone or wrong there is no edge to
    /// send it to, and sending it anywhere else is what the person turned
    /// the edge on to prevent.
    pub fn read(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> io::Result<Self> {
        let edge: Edge = serde_json::from_str(text).map_err(invalid)?;
        if !is_onion(&edge.onion) {
            return Err(invalid(format!(
                "{:?} is not a v3 onion address",
                edge.onion
            )));
        }
        if edge.port == 0 {
            return Err(invalid("the exit's port is 0"));
        }
        // RFC 1929 gives each field one length byte, and an empty one is
        // the same as none.
        for (name, field) in [("username", &edge.username), ("password", &edge.password)] {
            if field.is_empty() || field.len() > 255 {
                return Err(invalid(format!(
                    "the {name} is empty or longer than 255 bytes"
                )));
            }
        }
        Ok(edge)
    }
}

/// A v3 onion address: 56 base32 characters and `.onion`. Tor checks the
/// key and checksum itself; this only keeps a mistyped file from reaching
/// Tor as a name it would try to resolve.
pub fn is_onion(host: &str) -> bool {
    host.strip_suffix(".onion").is_some_and(|label| {
        label.len() == 56
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

fn invalid(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Where a SOCKS5 CONNECT goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A name the proxy resolves: the onion service, for Tor.
    Name(String, u16),
    /// An address: what the connection was meant for, for the edge.
    Addr(SocketAddr),
}

/// The client half of RFC 1928's greeting with RFC 1929's username and
/// password, the only method offered.
pub fn authenticate(
    stream: &mut (impl Read + Write),
    username: &str,
    password: &str,
) -> io::Result<()> {
    stream.write_all(&[5, 1, 2])?;
    let mut chosen = [0u8; 2];
    stream.read_exact(&mut chosen)?;
    if chosen != [5, 2] {
        return Err(refused(format!(
            "the proxy would not take a password ({chosen:?})"
        )));
    }
    let mut request = Vec::with_capacity(3 + username.len() + password.len());
    request.push(1);
    push_field(&mut request, username.as_bytes())?;
    push_field(&mut request, password.as_bytes())?;
    stream.write_all(&request)?;
    let mut status = [0u8; 2];
    stream.read_exact(&mut status)?;
    if status[1] != 0 {
        return Err(refused("the proxy refused the username and password"));
    }
    Ok(())
}

fn push_field(out: &mut Vec<u8>, field: &[u8]) -> io::Result<()> {
    let len = u8::try_from(field.len()).map_err(|_| invalid("a SOCKS field is over 255 bytes"))?;
    out.push(len);
    out.extend_from_slice(field);
    Ok(())
}

/// RFC 1928's CONNECT. On success the stream carries the connection.
pub fn connect(stream: &mut (impl Read + Write), target: &Target) -> io::Result<()> {
    let mut request = vec![5, 1, 0];
    let port = match target {
        Target::Name(name, port) => {
            request.push(3);
            push_field(&mut request, name.as_bytes())?;
            *port
        }
        Target::Addr(SocketAddr::V4(a)) => {
            request.push(1);
            request.extend_from_slice(&a.ip().octets());
            a.port()
        }
        Target::Addr(SocketAddr::V6(a)) => {
            request.push(4);
            request.extend_from_slice(&a.ip().octets());
            a.port()
        }
    };
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request)?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head)?;
    if head[0] != 5 {
        return Err(invalid("the proxy does not speak SOCKS5"));
    }
    if head[1] != 0 {
        return Err(refused(format!(
            "the proxy could not connect: {}",
            reply(head[1])
        )));
    }
    // The bound address, which nothing here needs, still has to be read
    // off the stream before the connection's own bytes.
    let rest = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            usize::from(len[0]) + 2
        }
        other => {
            return Err(invalid(format!(
                "the proxy answered with address type {other}"
            )))
        }
    };
    let mut bound = vec![0u8; rest];
    stream.read_exact(&mut bound)?;
    Ok(())
}

fn refused(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionRefused, e)
}

/// RFC 1928's reply codes, and Tor's own for onion services
/// (tor's doc/socks-extensions.txt), in words for the log.
pub fn reply(code: u8) -> &'static str {
    match code {
        1 => "general failure",
        2 => "not allowed by the ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        0xf0 => "onion service descriptor not found",
        0xf1 => "onion service descriptor is invalid",
        0xf2 => "onion service introduction failed",
        0xf3 => "onion service rendezvous failed",
        0xf4 => "onion service needs client authorization",
        0xf5 => "onion service client authorization is wrong",
        0xf6 => "onion address is invalid",
        0xf7 => "onion service introduction timed out",
        _ => "unknown error",
    }
}

/// Opens the way out for one connection: Tor to the edge's onion, then the
/// edge's exit to `dst`.
///
/// Tor gets the client's address and the destination port as its own
/// username and password. With `IsolateSOCKSAuth`, its default, that keeps
/// each app on each machine to circuits of its own, as Tor's TransPort
/// would have, so two machines' traffic never shares a circuit to the edge.
pub fn open(
    tor: SocketAddr,
    edge: &Edge,
    client: IpAddr,
    dst: SocketAddr,
) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect_timeout(&tor, HANDSHAKE_TIMEOUT)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    authenticate(&mut stream, &client.to_string(), &dst.port().to_string())?;
    connect(&mut stream, &Target::Name(edge.onion.clone(), edge.port))?;
    authenticate(&mut stream, &edge.username, &edge.password)?;
    connect(&mut stream, &Target::Addr(dst))?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Copies both ways until both sides have finished, passing each side's
/// end of stream on to the other.
pub fn splice(a: &TcpStream, b: &TcpStream) {
    std::thread::scope(|s| {
        s.spawn(|| pass(a, b));
        pass(b, a);
    });
}

fn pass(from: &TcpStream, to: &TcpStream) {
    let _ = io::copy(&mut &*from, &mut &*to);
    let _ = to.shutdown(Shutdown::Write);
}

/// Where a connection the firewall redirected was going. Linux keeps it in
/// conntrack and hands it back through `SO_ORIGINAL_DST`.
pub fn original_dst(stream: &TcpStream) -> io::Result<SocketAddr> {
    // linux/netfilter_ipv4.h and linux/netfilter_ipv6/ip6_tables.h, both
    // 80; libc does not export the IPv6 one on every target.
    const SO_ORIGINAL_DST: libc::c_int = 80;
    const IP6T_SO_ORIGINAL_DST: libc::c_int = 80;
    let fd = stream.as_raw_fd();
    if stream.local_addr()?.is_ipv4() {
        // SAFETY: a zeroed sockaddr_in is a valid value, and the kernel
        // writes at most `len` bytes into it.
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let r = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_IP,
                SO_ORIGINAL_DST,
                (&raw mut addr).cast(),
                &mut len,
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
        Ok(SocketAddr::new(ip.into(), u16::from_be(addr.sin_port)))
    } else {
        // SAFETY: as above, for sockaddr_in6.
        let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
        let r = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_IPV6,
                IP6T_SO_ORIGINAL_DST,
                (&raw mut addr).cast(),
                &mut len,
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        let ip = Ipv6Addr::from(addr.sin6_addr.s6_addr);
        Ok(SocketAddr::new(ip.into(), u16::from_be(addr.sin6_port)))
    }
}

/// Whether a destination may go out through the edge. The gateway's inside
/// address, on any port, and loopback would loop back into the gateway, and
/// a connection the firewall did not redirect reads as the listener itself.
/// The gateway's uplink address is not checked here: it is private, and the
/// edge's exit lets out only public addresses.
pub fn may_leave(dst: SocketAddr, listen: SocketAddr) -> bool {
    let ip = dst.ip();
    ip != listen.ip() && !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";

    #[test]
    fn reads_the_edge_the_host_wrote() {
        let edge = Edge::parse(&format!(
            r#"{{"onion":"{ONION}","port":1080,"username":"desk-1","password":"s3cret"}}"#
        ))
        .unwrap();
        assert_eq!(edge.port, 1080);
        assert_eq!(edge.username, "desk-1");
    }

    #[test]
    fn refuses_an_edge_file_that_is_not_one() {
        for text in [
            "",
            "{}",
            r#"{"onion":"example.com","port":1080,"username":"a","password":"b"}"#,
            &format!(r#"{{"onion":"{ONION}","port":0,"username":"a","password":"b"}}"#),
            &format!(r#"{{"onion":"{ONION}","port":1080,"username":"","password":"b"}}"#),
            &format!(
                r#"{{"onion":"{ONION}","port":1080,"username":"a","password":"b","via":"x"}}"#
            ),
        ] {
            assert!(Edge::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn knows_a_v3_onion_address() {
        assert!(is_onion(ONION));
        assert!(!is_onion(
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRSTUVWX.onion"
        ));
        assert!(!is_onion("abcdefghijklmnop.onion"));
        assert!(!is_onion(ONION.trim_end_matches(".onion")));
    }

    #[test]
    fn keeps_traffic_from_looping_into_the_gateway() {
        let listen: SocketAddr = "10.152.152.10:9041".parse().unwrap();
        assert!(may_leave("93.184.215.14:443".parse().unwrap(), listen));
        assert!(!may_leave(listen, listen));
        // Any other port of the gateway's inside address, such as DNS.
        assert!(!may_leave(SocketAddr::new(listen.ip(), 53), listen));
        assert!(!may_leave("127.0.0.1:9050".parse().unwrap(), listen));
        assert!(!may_leave("0.0.0.0:80".parse().unwrap(), listen));
        assert!(!may_leave("[::1]:80".parse().unwrap(), listen));
    }

    #[test]
    fn writes_a_connect_for_each_kind_of_target() {
        // A stream that answers success with a bound IPv4 address and
        // records what was written.
        struct Wire {
            written: Vec<u8>,
            answer: io::Cursor<Vec<u8>>,
        }
        impl Read for Wire {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.answer.read(buf)
            }
        }
        impl Write for Wire {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.written.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let ok = || io::Cursor::new(vec![5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);

        let mut w = Wire {
            written: vec![],
            answer: ok(),
        };
        connect(&mut w, &Target::Name("a.onion".into(), 1080)).unwrap();
        assert_eq!(
            w.written,
            [&[5, 1, 0, 3, 7][..], b"a.onion", &[4, 56]].concat()
        );

        let mut w = Wire {
            written: vec![],
            answer: ok(),
        };
        connect(&mut w, &Target::Addr("1.2.3.4:443".parse().unwrap())).unwrap();
        assert_eq!(w.written, [5, 1, 0, 1, 1, 2, 3, 4, 1, 187]);

        let mut w = Wire {
            written: vec![],
            answer: ok(),
        };
        connect(&mut w, &Target::Addr("[2001:db8::1]:80".parse().unwrap())).unwrap();
        assert_eq!(w.written[3], 4);
        assert_eq!(w.written.len(), 4 + 16 + 2);

        let mut w = Wire {
            written: vec![],
            answer: io::Cursor::new(vec![5, 0xf2, 0, 1, 0, 0, 0, 0, 0, 0]),
        };
        let e = connect(&mut w, &Target::Name("a.onion".into(), 1)).unwrap_err();
        assert!(e.to_string().contains("introduction failed"), "{e}");
    }
}
