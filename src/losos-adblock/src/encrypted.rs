//! DNS over HTTPS (RFC 8484) and DNS over TLS (RFC 7858) to the
//! forwarder's upstreams, while encrypted DNS is on (docs/adblock.md,
//! "Encrypted DNS").
//!
//! Plain DNS shows every name a machine looks up to whoever carries its
//! traffic, the Wi-Fi's owner and the ISP, and lets them answer instead of
//! the server. Encrypted, they see a TLS connection to a resolver and
//! nothing of what it carries. The two differ in what that connection
//! looks like: DNS over TLS has a port of its own, 853, which a network can
//! block or single out at a glance, while DNS over HTTPS is on 443 among
//! every web page. A server list can mix both, and the forwarder tries
//! them in order.
//!
//! Each server is named by its URL, `https://` or `tls://`, and the
//! addresses to reach it at. The addresses are not looked up: looking up
//! the resolver's own name would be the one plain query left, and an
//! answer from the network would pick which server is trusted. The URL's
//! host is still what the certificate is checked against.
//!
//! Connections stay open between queries: resolved caches and coalesces,
//! so a desktop's cache misses are a few a second at most, and a kept
//! connection saves the TLS handshake on each. Over HTTPS that is
//! HTTP/1.1, one query per POST; nothing here needs HTTP/2's streams. Over
//! TLS it is DNS's own TCP framing, a length and the message.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use rustls_platform_verifier::BuilderVerifierExt;
use tracing::debug;

/// How long a server gets to accept a connection, and then to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// Idle connections kept per server: one per forwarder worker is the most
/// that can be in use at once.
const IDLE: usize = 8;
/// The largest DNS message, and so the largest body worth reading.
const MAX_BODY: usize = 65535;
/// How long a server that failed is asked only after the others: a
/// network that blocks port 853 would otherwise cost every query the
/// connect timeout before the next server is tried.
const REST: Duration = Duration::from_secs(60);

type Tls = StreamOwned<ClientConnection, TcpStream>;

/// How a server is spoken to.
#[derive(Debug, PartialEq, Eq)]
enum Protocol {
    /// DNS over HTTPS: the `Host` header (the URL's host, and its port if
    /// not 443) and the path to POST to.
    Https { authority: String, path: String },
    /// DNS over TLS.
    Tls,
}

/// A DNS over HTTPS or DNS over TLS server.
pub struct Server {
    /// As written, for logs and `status`.
    pub url: String,
    host: ServerName<'static>,
    protocol: Protocol,
    addrs: Vec<SocketAddr>,
    idle: Mutex<Vec<Tls>>,
    /// Until when it is asked last, after it failed.
    resting: Mutex<Option<Instant>>,
}

impl PartialEq for Server {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url && self.addrs == other.addrs
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {:?}", self.url, self.addrs)
    }
}

/// Why a server line could not be read.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LineError {
    #[error("{0} is neither an https:// nor a tls:// URL")]
    Url(String),
    #[error("{0} names no address to reach it at")]
    NoAddress(String),
    #[error("{0} is not an IP address")]
    Address(String),
}

/// A server from a line `https://host[:port]/path ADDRESS...` or
/// `tls://host[:port] ADDRESS...`.
pub fn parse_line(line: &str) -> Result<Server, LineError> {
    let mut words = line.split_whitespace();
    let url = words.next().unwrap_or_default();
    let (rest, https) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, true)
    } else if let Some(rest) = url.strip_prefix("tls://") {
        (rest, false)
    } else {
        return Err(LineError::Url(url.into()));
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/dns-query"),
    };
    // A path means nothing to DNS over TLS; one that is not "/" is a URL
    // meant for HTTPS.
    if !https && !matches!(path, "/" | "/dns-query") {
        return Err(LineError::Url(url.into()));
    }
    let (host, port) = match authority.rsplit_once(':') {
        // An IPv6 literal is bracketed; a colon inside it is not a port.
        Some((host, port)) if !port.contains(']') => (
            host,
            port.parse::<u16>()
                .map_err(|_| LineError::Url(url.into()))?,
        ),
        _ => (authority, if https { 443 } else { 853 }),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let name = ServerName::try_from(host.to_owned()).map_err(|_| LineError::Url(url.into()))?;
    let mut addrs = Vec::new();
    for word in words {
        let ip: IpAddr = word.parse().map_err(|_| LineError::Address(word.into()))?;
        addrs.push(SocketAddr::new(ip, port));
    }
    // A host that is itself an address needs no other.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if addrs.is_empty() {
            addrs.push(SocketAddr::new(ip, port));
        }
    }
    if addrs.is_empty() {
        return Err(LineError::NoAddress(url.into()));
    }
    let protocol = if https {
        Protocol::Https {
            authority: authority.into(),
            path: path.into(),
        }
    } else {
        Protocol::Tls
    };
    Ok(Server {
        url: url.into(),
        host: name,
        protocol,
        addrs,
        idle: Mutex::new(Vec::new()),
        resting: Mutex::new(None),
    })
}

/// The servers in a file of such lines, skipping blank ones and `#`
/// comments; a line that cannot be read is reported and left out.
pub fn parse(text: &str) -> Vec<Arc<Server>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(server) => out.push(Arc::new(server)),
            Err(error) => tracing::warn!(%error, "encrypted DNS server left out"),
        }
    }
    out
}

/// The TLS settings every connection of one protocol shares: the system's
/// CA store, as for fetching lists, and the protocol offered by ALPN,
/// HTTP/1.1 as a browser would or RFC 7858's "dot".
fn config(https: bool) -> io::Result<Arc<ClientConfig>> {
    type Cached = OnceLock<Result<Arc<ClientConfig>, String>>;
    static HTTPS: Cached = OnceLock::new();
    static TLS: Cached = OnceLock::new();
    let (cell, alpn): (&Cached, &[u8]) = if https {
        (&HTTPS, b"http/1.1")
    } else {
        (&TLS, b"dot")
    };
    cell.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(|b| b.with_platform_verifier())
            .map_err(|e| e.to_string())?
            .with_no_client_auth();
        config.alpn_protocols = vec![alpn.to_vec()];
        Ok(Arc::new(config))
    })
    .clone()
    .map_err(io::Error::other)
}

impl Server {
    fn connect(&self) -> io::Result<Tls> {
        let mut last = io::Error::other("no address");
        for addr in &self.addrs {
            match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
                Ok(tcp) => {
                    tcp.set_read_timeout(Some(READ_TIMEOUT))?;
                    tcp.set_write_timeout(Some(READ_TIMEOUT))?;
                    tcp.set_nodelay(true)?;
                    let https = matches!(self.protocol, Protocol::Https { .. });
                    let conn = ClientConnection::new(config(https)?, self.host.clone())
                        .map_err(io::Error::other)?;
                    return Ok(StreamOwned::new(conn, tcp));
                }
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// The server's reply to `query`, a DNS message, with the query's own
    /// id put back.
    ///
    /// RFC 8484 asks for id 0 on the wire, so the same question is the same
    /// request whoever asks it; the id the stub picked stays here. Over TLS
    /// one query is in flight per connection, so 0 is as good an id there.
    /// A kept
    /// connection may have been closed by the server while idle, so a
    /// failure on one is tried once more on a new connection.
    pub fn ask(&self, query: &[u8]) -> io::Result<Vec<u8>> {
        let result = self.ask_once(query);
        if let Ok(mut resting) = self.resting.lock() {
            *resting = result.is_err().then(|| Instant::now() + REST);
        }
        result
    }

    /// Whether it failed lately and should be asked after the others.
    pub fn resting(&self) -> bool {
        self.resting
            .lock()
            .ok()
            .and_then(|r| *r)
            .is_some_and(|until| Instant::now() < until)
    }

    fn ask_once(&self, query: &[u8]) -> io::Result<Vec<u8>> {
        if query.len() < 2 {
            return Err(io::Error::other("short query"));
        }
        let mut wire = query.to_vec();
        wire[0] = 0;
        wire[1] = 0;
        let kept = self.idle.lock().ok().and_then(|mut idle| idle.pop());
        let mut reply = match kept {
            Some(mut tls) => match self.exchange(&mut tls, &wire) {
                Ok(reply) => Ok((reply, tls)),
                Err(error) => {
                    debug!(url = %self.url, %error, "kept connection failed; reconnecting");
                    let mut tls = self.connect()?;
                    self.exchange(&mut tls, &wire).map(|reply| (reply, tls))
                }
            },
            None => {
                let mut tls = self.connect()?;
                self.exchange(&mut tls, &wire).map(|reply| (reply, tls))
            }
        }
        .map(|((reply, keep), tls)| {
            if keep {
                if let Ok(mut idle) = self.idle.lock() {
                    if idle.len() < IDLE {
                        idle.push(tls);
                    }
                }
            }
            reply
        })?;
        if reply.len() < 12 {
            return Err(io::Error::other("short reply"));
        }
        reply[0] = query[0];
        reply[1] = query[1];
        Ok(reply)
    }

    /// One query and its reply on `tls`: the reply, and whether the server
    /// lets the connection be used again.
    fn exchange(&self, tls: &mut Tls, wire: &[u8]) -> io::Result<(Vec<u8>, bool)> {
        let Protocol::Https { authority, path } = &self.protocol else {
            // DNS's TCP framing: a two-byte length, then the message, each
            // way, in one write so it goes out in one TLS record.
            let mut message = (wire.len() as u16).to_be_bytes().to_vec();
            message.extend_from_slice(wire);
            tls.write_all(&message)?;
            tls.flush()?;
            let mut len = [0u8; 2];
            tls.read_exact(&mut len)?;
            let mut reply = vec![0u8; u16::from_be_bytes(len) as usize];
            tls.read_exact(&mut reply)?;
            return Ok((reply, true));
        };
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/dns-message\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\n\r\n",
            wire.len()
        );
        let mut request = head.into_bytes();
        request.extend_from_slice(wire);
        tls.write_all(&request)?;
        tls.flush()?;
        read_response(&mut BufReader::new(tls))
    }
}

/// An HTTP/1.1 response's body, read from `reader`, and whether its
/// connection may carry another request.
pub fn read_response(reader: &mut impl BufRead) -> io::Result<(Vec<u8>, bool)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other(format!("not an HTTP response: {line:?}")))?;
    let mut length: Option<usize> = None;
    let mut chunked = false;
    let mut keep = true;
    let mut dns = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().ok(),
            "transfer-encoding" => chunked = value.contains("chunked"),
            "connection" => keep = !value.contains("close"),
            "content-type" => dns = value.starts_with("application/dns-message"),
            _ => {}
        }
    }
    let body = if chunked {
        read_chunked(reader)?
    } else {
        let length = length.ok_or_else(|| io::Error::other("no Content-Length"))?;
        if length > MAX_BODY {
            return Err(io::Error::other("body too large"));
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
        body
    };
    // The body has been read whatever the status, so the connection is
    // still in step, but only a DNS message is an answer.
    if status != 200 || !dns {
        return Err(io::Error::other(format!("HTTP {status}")));
    }
    Ok((body, keep))
}

fn read_chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| io::Error::other("bad chunk size"))?;
        if size == 0 {
            // Trailers, up to the blank line that ends them.
            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
                    return Ok(body);
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(io::Error::other("body too large"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        line.clear();
        reader.read_line(&mut line)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_server_line() {
        let server = parse_line("https://dns.quad9.net/dns-query 9.9.9.9 2620:fe::fe").unwrap();
        assert_eq!(
            server.protocol,
            Protocol::Https {
                authority: "dns.quad9.net".into(),
                path: "/dns-query".into()
            }
        );
        assert_eq!(
            server.addrs,
            vec![
                "9.9.9.9:443".parse::<SocketAddr>().unwrap(),
                "[2620:fe::fe]:443".parse::<SocketAddr>().unwrap()
            ]
        );
    }

    #[test]
    fn a_port_carries_to_every_address() {
        let server = parse_line("https://doh.test:8443/q 10.0.0.1").unwrap();
        assert!(
            matches!(&server.protocol, Protocol::Https { authority, .. } if authority == "doh.test:8443")
        );
        assert_eq!(
            server.addrs,
            vec!["10.0.0.1:8443".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn reads_a_tls_line() {
        let server = parse_line("tls://dns.quad9.net 9.9.9.9").unwrap();
        assert_eq!(server.protocol, Protocol::Tls);
        assert_eq!(
            server.addrs,
            vec!["9.9.9.9:853".parse::<SocketAddr>().unwrap()]
        );
        assert!(matches!(
            parse_line("tls://dns.quad9.net/path 9.9.9.9"),
            Err(LineError::Url(_))
        ));
    }

    #[test]
    fn an_address_host_needs_no_other() {
        let server = parse_line("https://9.9.9.9/dns-query").unwrap();
        assert_eq!(
            server.addrs,
            vec!["9.9.9.9:443".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn a_name_without_addresses_is_refused() {
        // Looking the resolver up would be a plain query of its own.
        assert_eq!(
            parse_line("https://dns.example/dns-query").unwrap_err(),
            LineError::NoAddress("https://dns.example/dns-query".into())
        );
        assert!(matches!(
            parse_line("http://dns.example 1.1.1.1"),
            Err(LineError::Url(_))
        ));
    }

    #[test]
    fn reads_a_length_response() {
        let text = b"HTTP/1.1 200 OK\r\nContent-Type: application/dns-message\r\nContent-Length: 3\r\n\r\nabcHTTP/1.1";
        let (body, keep) = read_response(&mut &text[..]).unwrap();
        assert_eq!(body, b"abc");
        assert!(keep);
    }

    #[test]
    fn reads_a_chunked_response() {
        let text = b"HTTP/1.1 200 OK\r\ncontent-type: application/dns-message\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nab\r\n1\r\nc\r\n0\r\n\r\n";
        let (body, keep) = read_response(&mut &text[..]).unwrap();
        assert_eq!(body, b"abc");
        assert!(!keep);
    }

    #[test]
    fn an_error_status_is_no_answer() {
        let text =
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nno";
        assert!(read_response(&mut &text[..]).is_err());
    }
}
