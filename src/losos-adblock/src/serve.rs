//! The DNS forwarder resolved sends every query to.
//!
//! resolved stays the system's resolver, with its cache, its stub on
//! 127.0.0.53, mDNS and LLMNR. The OS gives NetworkManager a dummy link whose
//! only DNS server is this forwarder, for every domain and ahead of every
//! other link (nixos/modules/adblock.nix), so every query for the wider
//! internet comes here and the networks' own servers are handed to resolved
//! for nothing. A blocked name is answered on the spot; anything else goes,
//! unchanged, to the servers the network handed out, which NetworkManager
//! still lists for each device and `nmcli` reads back.
//!
//! Queries go through a bounded queue to a few worker threads: resolved
//! caches and coalesces, so what reaches here is a desktop's worth of
//! cache misses, and a worker blocked on a slow upstream holds up only its
//! own query. When every worker is busy and the queue is full, a query is
//! dropped and the client retries, as DNS clients do. Every thread is
//! scoped to `run`, which shares its state by reference.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, SocketAddrV6, TcpListener, TcpStream, UdpSocket};
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::TrySendError;
use tracing::{debug, info, warn};

use crate::dns;
use crate::rules::{Blocklist, Verdict};

/// Where resolved lists the servers it uses, for a system without
/// NetworkManager.
const RESOLV_CONF: &str = "/run/systemd/resolve/resolv.conf";
/// Servers that replace every network's while the file is there, in
/// resolv.conf's `nameserver` lines: schizo mode's Tor gateway
/// (nixos/modules/schizo.nix), past which no other server is reachable.
const OVERRIDE: &str = "/run/losos-adblock/upstreams";
/// How long one upstream gets before the next is tried.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the compiled list is checked for changes and the upstreams
/// read again.
const RELOAD_EVERY: Duration = Duration::from_secs(15);
/// The least time between two readings of the upstreams forced by a query
/// none of them answered, so a network that is down is not asked about on
/// every query.
const REFRESH_GAP: Duration = Duration::from_secs(2);
/// Worker threads answering UDP queries, and how many queries may wait
/// for one: enough for a burst of cache misses, few enough that a dead
/// upstream cannot pile up minutes of work.
const WORKERS: usize = 8;
const QUEUE: usize = 256;

/// The listening sockets: from systemd's socket activation, or bound here.
pub struct Sockets {
    pub udp: Vec<UdpSocket>,
    pub tcp: Vec<TcpListener>,
}

impl Sockets {
    /// Sockets systemd passed (`LISTEN_FDS`), sorted by type.
    pub fn from_systemd() -> Option<Self> {
        let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
        if pid != std::process::id() {
            return None;
        }
        let count: i32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
        let mut sockets = Self {
            udp: Vec::new(),
            tcp: Vec::new(),
        };
        for fd in 3..3 + count {
            let mut kind: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            // SAFETY: fd is one systemd passed us; SO_TYPE writes one int.
            let ok = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&mut kind as *mut libc::c_int).cast(),
                    &mut len,
                )
            } == 0;
            if !ok {
                continue;
            }
            // SAFETY: systemd hands over ownership of each listed fd.
            match kind {
                libc::SOCK_DGRAM => sockets.udp.push(unsafe { UdpSocket::from_raw_fd(fd) }),
                libc::SOCK_STREAM => sockets.tcp.push(unsafe { TcpListener::from_raw_fd(fd) }),
                _ => {}
            }
        }
        Some(sockets)
    }

    /// Binds UDP and TCP on `addr`, for running outside systemd.
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self {
            udp: vec![UdpSocket::bind(addr)?],
            tcp: vec![TcpListener::bind(addr)?],
        })
    }

    fn local_addrs(&self) -> Vec<SocketAddr> {
        self.udp
            .iter()
            .filter_map(|s| s.local_addr().ok())
            .chain(self.tcp.iter().filter_map(|s| s.local_addr().ok()))
            .collect()
    }
}

/// What every query is checked and forwarded against, reloaded in place.
struct State {
    blocklist: Blocklist,
    upstreams: Vec<SocketAddr>,
}

struct Shared {
    state: RwLock<State>,
    list: PathBuf,
    /// Our own addresses, never forwarded to: NetworkManager and resolved
    /// list us among their servers.
    own: Vec<SocketAddr>,
    /// When the upstreams were last read.
    refreshed: Mutex<Instant>,
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn load_list(path: &std::path::Path) -> Blocklist {
    match fs::read_to_string(path) {
        Ok(text) => {
            let list = Blocklist::read(&text);
            info!(domains = list.len(), "loaded the blocklist");
            list
        }
        Err(error) => {
            // Before the first update there is nothing to block yet;
            // forwarding still works.
            warn!(path = %path.display(), %error, "no blocklist yet; forwarding everything");
            Blocklist::default()
        }
    }
}

/// A server's address from its text, `ip` or `ip%link`, minus resolved's
/// own stubs and us.
fn server(text: &str, link: Option<&str>, own: &[SocketAddr]) -> Option<SocketAddr> {
    let (ip, link) = match text.split_once('%') {
        Some((ip, link)) => (ip, Some(link)),
        None => (text, link),
    };
    let addr = match ip.parse::<IpAddr>().ok()? {
        // A link-local IPv6 server is only reachable through its link.
        IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => {
            let name = std::ffi::CString::new(link?).ok()?;
            // SAFETY: a NUL-terminated interface name.
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            SocketAddr::V6(SocketAddrV6::new(v6, 53, 0, index))
        }
        ip => SocketAddr::new(ip, 53),
    };
    let stub = matches!(addr.ip(), IpAddr::V4(v4) if v4.octets()[..3] == [127, 0, 0] && matches!(v4.octets()[3], 53 | 54));
    (!stub && !own.iter().any(|o| o.ip() == addr.ip())).then_some(addr)
}

/// The servers in a resolv.conf, minus resolved's own stubs and us.
pub fn upstreams(text: &str, own: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(text) = line.trim().strip_prefix("nameserver") else {
            continue;
        };
        match server(text.trim(), None, own) {
            Some(addr) if !out.contains(&addr) => out.push(addr),
            _ => {}
        }
    }
    out
}

/// The servers in `nmcli -t -f GENERAL.DEVICE,IP4.DNS,IP6.DNS device show`,
/// each device's after its `GENERAL.DEVICE` line, minus us. Terse output
/// escapes a colon in a value, as in every IPv6 address, with a backslash.
pub fn nm_upstreams(text: &str, own: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    let mut device = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.replace("\\:", ":").replace("\\\\", "\\");
        if key == "GENERAL.DEVICE" {
            device = value;
        } else if key.starts_with("IP4.DNS[") || key.starts_with("IP6.DNS[") {
            match server(&value, Some(&device), own) {
                Some(addr) if !out.contains(&addr) => out.push(addr),
                _ => {}
            }
        }
    }
    out
}

fn load_upstreams(own: &[SocketAddr]) -> Vec<SocketAddr> {
    if let Ok(text) = fs::read_to_string(OVERRIDE) {
        let found = upstreams(&text, own);
        debug!(?found, "upstream servers, from {OVERRIDE}");
        return found;
    }
    // NetworkManager knows each network's servers. Without it, resolved's
    // resolv.conf lists them, and outside a resolved system /etc/resolv.conf
    // does; under resolved that names only the stub, which is skipped.
    let nmcli = Command::new("nmcli")
        .args([
            "-t",
            "-f",
            "GENERAL.DEVICE,IP4.DNS,IP6.DNS",
            "device",
            "show",
        ])
        .stderr(Stdio::null())
        .output();
    let found = match nmcli {
        Ok(out) if out.status.success() => nm_upstreams(&String::from_utf8_lossy(&out.stdout), own),
        _ => {
            let text = fs::read_to_string(RESOLV_CONF)
                .or_else(|_| fs::read_to_string("/etc/resolv.conf"))
                .unwrap_or_default();
            upstreams(&text, own)
        }
    };
    debug!(?found, "upstream servers");
    found
}

/// Reads the upstreams again, unless that was done within `gap`; true if
/// they changed.
fn refresh_upstreams(shared: &Shared, gap: Duration) -> bool {
    {
        let Ok(mut refreshed) = shared.refreshed.try_lock() else {
            return false;
        };
        if refreshed.elapsed() < gap {
            return false;
        }
        *refreshed = Instant::now();
    }
    let upstreams = load_upstreams(&shared.own);
    let Ok(mut state) = shared.state.write() else {
        return false;
    };
    let changed = state.upstreams != upstreams;
    state.upstreams = upstreams;
    changed
}

/// Answers `query`: blocked here, forwarded, or a SERVFAIL. When no
/// upstream answers, the network may have changed since they were read, so
/// they are read again and tried once more.
fn answer(shared: &Shared, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
    {
        let state = shared.state.read().ok()?;
        if let Some(question) = dns::question(query) {
            if let Verdict::Blocked(rule) = state.blocklist.verdict(&question.name) {
                debug!(name = %question.name, %rule, "blocked");
                return Some(dns::blocked(query, &question));
            }
        }
    }
    if let Some(reply) = forward(shared, query, tcp) {
        return Some(reply);
    }
    if refresh_upstreams(shared, REFRESH_GAP) {
        if let Some(reply) = forward(shared, query, tcp) {
            return Some(reply);
        }
    }
    dns::servfail(query)
}

/// The first upstream's reply to `query`, if any gives one.
fn forward(shared: &Shared, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
    let upstreams = shared.state.read().ok()?.upstreams.clone();
    for upstream in upstreams {
        let result = if tcp {
            forward_tcp(upstream, query)
        } else {
            forward_udp(upstream, query)
        };
        match result {
            Ok(reply) => return Some(reply),
            Err(error) => debug!(%upstream, %error, "upstream failed"),
        }
    }
    None
}

fn forward_udp(upstream: SocketAddr, query: &[u8]) -> io::Result<Vec<u8>> {
    let bind: SocketAddr = if upstream.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect(upstream)?;
    socket.set_read_timeout(Some(UPSTREAM_TIMEOUT))?;
    socket.send(query)?;
    let mut buf = vec![0u8; 65535];
    loop {
        let n = socket.recv(&mut buf)?;
        // Only the reply to this query: same id. A connected socket already
        // drops packets from anyone else.
        if n >= 2 && buf[..2] == query[..2] {
            buf.truncate(n);
            return Ok(buf);
        }
    }
}

fn forward_tcp(upstream: SocketAddr, query: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&upstream, UPSTREAM_TIMEOUT)?;
    stream.set_read_timeout(Some(UPSTREAM_TIMEOUT))?;
    stream.set_write_timeout(Some(UPSTREAM_TIMEOUT))?;
    stream.write_all(&(query.len() as u16).to_be_bytes())?;
    stream.write_all(query)?;
    read_tcp_message(&mut stream)
}

fn read_tcp_message(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len)?;
    let mut message = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut message)?;
    Ok(message)
}

fn serve_tcp_client(shared: &Shared, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    // A client may send several queries on one connection.
    loop {
        let query = match read_tcp_message(&mut stream) {
            Ok(q) => q,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Some(reply) = answer(shared, &query, true) {
            stream.write_all(&(reply.len() as u16).to_be_bytes())?;
            stream.write_all(&reply)?;
        }
    }
}

/// Serves until the process is stopped; nothing it meets ends it, so it
/// has nothing to return.
pub fn run(sockets: Sockets, list: PathBuf) {
    let own = sockets.local_addrs();
    let shared = Shared {
        state: RwLock::new(State {
            blocklist: load_list(&list),
            upstreams: load_upstreams(&own),
        }),
        list,
        own,
        refreshed: Mutex::new(Instant::now()),
    };
    let shared = &shared;

    // One UDP query per message: the datagram and who sent it, with the
    // socket it came on to answer from.
    let (queue, queries) = crossbeam_channel::bounded::<(Vec<u8>, SocketAddr, &UdpSocket)>(QUEUE);

    thread::scope(|s| {
        // Reloads: the blocklist after an update, checked by modification
        // time so an update needs no signal to reach a running forwarder,
        // and the upstreams, which a network change moves without touching
        // any file this could watch.
        s.spawn(move || {
            let mut list_time = modified(&shared.list);
            loop {
                thread::sleep(RELOAD_EVERY);
                let now = modified(&shared.list);
                if now != list_time {
                    list_time = now;
                    let list = load_list(&shared.list);
                    if let Ok(mut state) = shared.state.write() {
                        state.blocklist = list;
                    }
                }
                refresh_upstreams(shared, Duration::ZERO);
            }
        });

        for _ in 0..WORKERS {
            let queries = queries.clone();
            s.spawn(move || {
                for (query, from, socket) in queries {
                    let started = Instant::now();
                    if let Some(reply) = answer(shared, &query, false) {
                        let _ = socket.send_to(&reply, from);
                    }
                    debug!(elapsed = ?started.elapsed(), "answered");
                }
            });
        }

        // A TCP client gets a thread of its own for as long as it stays
        // connected; there are few, and each may send several queries.
        for listener in &sockets.tcp {
            s.spawn(move || {
                for stream in listener.incoming().flatten() {
                    s.spawn(move || {
                        if let Err(error) = serve_tcp_client(shared, stream) {
                            debug!(%error, "TCP client");
                        }
                    });
                }
            });
        }

        for socket in &sockets.udp {
            let queue = queue.clone();
            s.spawn(move || {
                let mut buf = vec![0u8; 65535];
                loop {
                    let Ok((n, from)) = socket.recv_from(&mut buf) else {
                        continue;
                    };
                    match queue.try_send((buf[..n].to_vec(), from, socket)) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            debug!(%from, "query dropped, every worker is busy")
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_networkmanagers_servers() {
        let own: Vec<SocketAddr> = vec!["169.254.153.53:53".parse().unwrap()];
        let text = "GENERAL.DEVICE:eth0\nIP4.DNS[1]:192.168.1.1\nIP4.DNS[2]:192.168.1.1\nIP6.DNS[1]:2001\\:db8\\:\\:1\n\nGENERAL.DEVICE:adblock0\nIP4.DNS[1]:169.254.153.53\n\nGENERAL.DEVICE:lo\n";
        let found = nm_upstreams(text, &own);
        assert_eq!(
            found,
            vec![
                "192.168.1.1:53".parse::<SocketAddr>().unwrap(),
                "[2001:db8::1]:53".parse::<SocketAddr>().unwrap()
            ]
        );
    }

    #[test]
    fn reads_resolveds_servers() {
        let own: Vec<SocketAddr> = vec!["127.0.0.153:53".parse().unwrap()];
        let text = "# generated\nnameserver 127.0.0.153\nnameserver 192.168.1.1\nnameserver 127.0.0.53\nnameserver 2001:db8::1\nnameserver 192.168.1.1\nsearch lan\n";
        let found = upstreams(text, &own);
        assert_eq!(
            found,
            vec![
                "192.168.1.1:53".parse::<SocketAddr>().unwrap(),
                "[2001:db8::1]:53".parse::<SocketAddr>().unwrap()
            ]
        );
    }
}
