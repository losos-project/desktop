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
//!
//! With encrypted DNS on, the networks' servers are asked only about names
//! that exist on the local network alone (`LOCAL_ZONES`); everything else
//! goes over HTTPS or TLS to the servers chosen for it (encrypted.rs), and
//! when none of them answers the query fails. Falling back to the
//! network's server would hand anyone who can block ports 443 and 853 every
//! name in plain text. The one exception is a captive portal, unless
//! encrypted DNS is strict (portal.rs).

use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, SocketAddrV6, TcpListener, TcpStream, UdpSocket};
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::TrySendError;
use tracing::{debug, info, warn};

use crate::dns;
use crate::encrypted;
use crate::lists::Paths;
use crate::portal;
use crate::rules::{Blocklist, Verdict};

/// Where resolved lists the servers it uses, for a system without
/// NetworkManager.
const RESOLV_CONF: &str = "/run/systemd/resolve/resolv.conf";
/// Servers that replace every network's while the file is there, in
/// resolv.conf's `nameserver` lines: schizo mode's Tor gateway
/// (nixos/modules/schizo.nix), past which no other server is reachable.
const OVERRIDE: &str = "/run/losos-adblock/upstreams";
/// There, empty, while `OVERRIDE`'s servers must be the only ones asked,
/// encrypted DNS's included (`Switch`).
const FORCED: &str = "/run/losos-adblock/upstreams-forced";
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
/// How often the encrypted DNS switch is looked at, so turning it on takes
/// effect within a second rather than a reload's fifteen.
const SWITCH_EVERY: Duration = Duration::from_secs(1);
/// How long names go to the network's server once a captive portal was
/// found, before encrypted DNS is tried again: long enough to sign in
/// without a check on every query, short enough that a portal faked by the
/// network sees little.
const PORTAL_FOR: Duration = Duration::from_secs(30);
/// The least time between two portal checks, so a network that is down
/// is not checked on every query.
const PORTAL_CHECK_GAP: Duration = Duration::from_secs(10);
/// Names only the local network can answer, which still go to its server
/// in plain DNS with encrypted DNS on: a resolver on the internet has no
/// answer for them, and the network sees them either way when the
/// connection is made. RFC 8375's home.arpa, the private-use names routers
/// hand out, and the reverse zones of RFC 1918 and RFC 4193 addresses.
/// `.local` is mDNS, which resolved answers itself.
const LOCAL_ZONES: &[&str] = &[
    "home.arpa",
    "lan",
    "home",
    "internal",
    "localdomain",
    "intranet",
    "10.in-addr.arpa",
    "168.192.in-addr.arpa",
    "16.172.in-addr.arpa",
    "17.172.in-addr.arpa",
    "18.172.in-addr.arpa",
    "19.172.in-addr.arpa",
    "20.172.in-addr.arpa",
    "21.172.in-addr.arpa",
    "22.172.in-addr.arpa",
    "23.172.in-addr.arpa",
    "24.172.in-addr.arpa",
    "25.172.in-addr.arpa",
    "26.172.in-addr.arpa",
    "27.172.in-addr.arpa",
    "28.172.in-addr.arpa",
    "29.172.in-addr.arpa",
    "30.172.in-addr.arpa",
    "31.172.in-addr.arpa",
    "254.169.in-addr.arpa",
    "d.f.ip6.arpa",
];

/// Whether `name` is in one of the `LOCAL_ZONES`.
pub fn is_local(name: &str) -> bool {
    LOCAL_ZONES.iter().any(|zone| {
        name == *zone
            || name
                .strip_suffix(zone)
                .is_some_and(|rest| rest.ends_with('.'))
    })
}

/// What encrypted DNS is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// On, with the network's server asked while a captive portal is in
    /// the way.
    On,
    /// On, with no exception at all.
    Strict,
}

impl Mode {
    fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::On => "on",
            Mode::Strict => "strict",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        [Mode::Off, Mode::On, Mode::Strict]
            .into_iter()
            .find(|mode| mode.word() == word)
    }
}

/// Encrypted DNS while it is on: whom it asks, and whether strictly.
pub struct Encrypted {
    pub servers: Vec<Arc<encrypted::Server>>,
    pub strict: bool,
}

/// Encrypted DNS's settings, three files. The OS's, in the configuration
/// directory: `encrypted-dns`, the servers, and `encrypted-dns-default`,
/// the mode, `off`, `on` or `strict`. The person's, `encrypted-dns` in the
/// state directory: a mode on its first line, which wins over the OS's,
/// and any servers after it, which replace the OS's.
///
/// `FORCED` wins over all three. While it is there, the servers
/// `OVERRIDE` names are the only way out: schizo mode's Tor gateway, whose
/// DNS port keeps each destination's lookups on a circuit of its own and
/// maps `.onion` names, which one resolver behind one encrypted connection
/// would not, or the resolver inside an edge's tunnel. A file of its own
/// because `OVERRIDE` alone does not mean that: in the network machine,
/// which has no resolved, it always holds the network's servers. Both live
/// in /run, so a machine that dies with them in place comes back without
/// them, and the person's own choice was never touched.
pub struct Switch {
    pub own: PathBuf,
    servers: PathBuf,
    default: PathBuf,
    overridden: PathBuf,
}

/// The words of a settings file, without blank lines and `#` comments.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
}

/// The mode on the first line of `text`, if there is one.
fn choice(text: &str) -> Option<Mode> {
    Mode::from_word(lines(text).next()?)
}

impl Switch {
    pub fn new(paths: &Paths) -> Self {
        Self {
            own: paths.state.join("encrypted-dns"),
            servers: paths.config.join("encrypted-dns"),
            default: paths.config.join("encrypted-dns-default"),
            overridden: PathBuf::from(FORCED),
        }
    }

    /// When any of the three last changed, to tell whether to read again.
    fn stamp(&self) -> [Option<SystemTime>; 4] {
        [
            modified(&self.own),
            modified(&self.servers),
            modified(&self.default),
            modified(&self.overridden),
        ]
    }

    /// Whether `FORCED` is there, and encrypted DNS stands aside.
    pub fn overridden(&self) -> bool {
        self.overridden.exists()
    }

    /// Encrypted DNS's servers and strictness, or `None` while it is off.
    /// On with no server at all is still on, and every query fails until
    /// one is named: off has to be chosen, never fallen into.
    pub fn read(&self) -> Option<Encrypted> {
        if self.overridden() {
            return None;
        }
        let own = fs::read_to_string(&self.own).unwrap_or_default();
        let default = fs::read_to_string(&self.default).unwrap_or_default();
        let strict = match choice(&own).or(choice(&default)).unwrap_or(Mode::Off) {
            Mode::Off => return None,
            Mode::On => false,
            Mode::Strict => true,
        };
        let listed: String = lines(&own)
            .filter(|line| Mode::from_word(line).is_none())
            .collect::<Vec<_>>()
            .join("\n");
        let mut servers = encrypted::parse(&listed);
        if servers.is_empty() {
            servers = encrypted::parse(&fs::read_to_string(&self.servers).unwrap_or_default());
        }
        if servers.is_empty() {
            warn!("encrypted DNS is on with no server to ask; every query fails");
        }
        Some(Encrypted { servers, strict })
    }

    /// The person's switch set to `mode`, keeping any servers of their
    /// own below it.
    pub fn set(&self, mode: Mode) -> io::Result<()> {
        let own = fs::read_to_string(&self.own).unwrap_or_default();
        let mut text = format!("{}\n", mode.word());
        for line in lines(&own).filter(|line| Mode::from_word(line).is_none()) {
            text.push_str(line);
            text.push('\n');
        }
        // Whole or not at all, for the forwarder reading it.
        let new = self.own.with_extension("new");
        fs::write(&new, text)?;
        fs::rename(&new, &self.own)
    }
}

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
    /// Encrypted DNS, while it is on.
    encrypted: Option<Encrypted>,
}

struct Shared {
    state: RwLock<State>,
    list: PathBuf,
    switch: Switch,
    /// Our own addresses, never forwarded to: NetworkManager and resolved
    /// list us among their servers.
    own: Vec<SocketAddr>,
    /// When the upstreams were last read.
    refreshed: Mutex<Instant>,
    /// Until when names go to the network's server for a captive portal,
    /// and when one was last checked for.
    portal_until: Mutex<Option<Instant>>,
    portal_checked: Mutex<Option<Instant>>,
}

/// Whether a captive portal is in the way of encrypted DNS: one found
/// lately, or found now by a check, unless one ran within the gap.
fn behind_portal(shared: &Shared) -> bool {
    let now = Instant::now();
    if shared
        .portal_until
        .lock()
        .ok()
        .and_then(|u| *u)
        .is_some_and(|until| now < until)
    {
        return true;
    }
    {
        let Ok(mut checked) = shared.portal_checked.try_lock() else {
            // Another worker is checking.
            return false;
        };
        if checked.is_some_and(|at| now.duration_since(at) < PORTAL_CHECK_GAP) {
            return false;
        }
        *checked = Some(now);
    }
    let upstreams = match shared.state.read() {
        Ok(state) => state.upstreams.clone(),
        Err(_) => return false,
    };
    let network = portal::check(&upstreams);
    info!(
        ?network,
        "encrypted DNS failed; checked for a captive portal"
    );
    let portal = network == portal::Network::Portal;
    if portal {
        if let Ok(mut until) = shared.portal_until.lock() {
            *until = Some(now + PORTAL_FOR);
        }
    }
    portal
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
    let encrypted = {
        let state = shared.state.read().ok()?;
        let question = dns::question(query);
        if let Some(question) = &question {
            if let Verdict::Blocked(rule) = state.blocklist.verdict(&question.name) {
                debug!(name = %question.name, %rule, "blocked");
                return Some(dns::blocked(query, question));
            }
        }
        match &state.encrypted {
            // Local names go to the network's server; so does the portal
            // check's own name unless strict, so Danube's sign-in window
            // can find a portal that lets DNS through (docs/danube.md).
            Some(encrypted)
                if !question.as_ref().is_some_and(|q| {
                    is_local(&q.name) || (!encrypted.strict && q.name == portal::CHECK_HOST)
                }) =>
            {
                Some((encrypted.servers.clone(), encrypted.strict))
            }
            _ => None,
        }
    };
    if let Some((mut servers, strict)) = encrypted {
        let portal_now = !strict
            && shared
                .portal_until
                .lock()
                .ok()
                .and_then(|u| *u)
                .is_some_and(|until| Instant::now() < until);
        if !portal_now {
            // In the order given, except that one that failed lately waits
            // until the rest have been tried.
            servers.sort_by_key(|server| server.resting());
            for server in &servers {
                match server.ask(query) {
                    Ok(reply) => {
                        if let Ok(mut until) = shared.portal_until.lock() {
                            *until = None;
                        }
                        return Some(reply);
                    }
                    Err(error) => debug!(url = %server.url, %error, "encrypted upstream failed"),
                }
            }
            if strict || !behind_portal(shared) {
                return dns::servfail(query);
            }
        }
        debug!("behind a captive portal; asking the network's server");
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
pub fn run(sockets: Sockets, paths: &Paths) {
    let own = sockets.local_addrs();
    let list = paths.dns();
    let switch = Switch::new(paths);
    let encrypted = switch.read();
    info!(on = encrypted.is_some(), "encrypted DNS");
    let shared = Shared {
        state: RwLock::new(State {
            blocklist: load_list(&list),
            upstreams: load_upstreams(&own),
            encrypted,
        }),
        list,
        switch,
        own,
        refreshed: Mutex::new(Instant::now()),
        portal_until: Mutex::new(None),
        portal_checked: Mutex::new(None),
    };
    let shared = &shared;

    // One UDP query per message: the datagram and who sent it, with the
    // socket it came on to answer from.
    let (queue, queries) = crossbeam_channel::bounded::<(Vec<u8>, SocketAddr, &UdpSocket)>(QUEUE);

    thread::scope(|s| {
        // The encrypted DNS settings, by modification time, and whether
        // the person's switch is there at all.
        s.spawn(move || {
            let mut seen = shared.switch.stamp();
            loop {
                thread::sleep(SWITCH_EVERY);
                let now = shared.switch.stamp();
                if now == seen {
                    continue;
                }
                seen = now;
                let encrypted = shared.switch.read();
                info!(on = encrypted.is_some(), "encrypted DNS");
                if let Ok(mut state) = shared.state.write() {
                    state.encrypted = encrypted;
                }
            }
        });

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
    fn the_persons_choice_wins_and_off_must_be_chosen() {
        let dir = std::env::temp_dir().join(format!("losos-adblock-switch-{}", std::process::id()));
        let paths = Paths {
            config: dir.join("etc"),
            state: dir.join("var"),
        };
        fs::create_dir_all(&paths.config).unwrap();
        fs::create_dir_all(&paths.state).unwrap();
        let mut switch = Switch::new(&paths);
        switch.overridden = dir.join("upstreams-forced");
        fs::write(
            paths.config.join("encrypted-dns"),
            "https://dns.example/q 192.0.2.1\n",
        )
        .unwrap();
        // No default and no choice: off.
        assert!(switch.read().is_none());
        fs::write(paths.config.join("encrypted-dns-default"), "on\n").unwrap();
        assert_eq!(switch.read().unwrap().servers.len(), 1);
        switch.set(Mode::Off).unwrap();
        assert!(switch.read().is_none());
        // Servers of the person's own survive turning it on and off.
        fs::write(
            &switch.own,
            "off\ntls://dot.example 192.0.2.2\ntls://dot2.example 192.0.2.3\n",
        )
        .unwrap();
        switch.set(Mode::Strict).unwrap();
        let encrypted = switch.read().unwrap();
        assert!(encrypted.strict);
        assert_eq!(encrypted.servers.len(), 2);
        assert_eq!(encrypted.servers[0].url, "tls://dot.example");
        switch.set(Mode::On).unwrap();
        assert!(!switch.read().unwrap().strict);
        // On with nothing to ask is on, and fails, rather than plain.
        fs::write(paths.config.join("encrypted-dns"), "").unwrap();
        fs::write(&switch.own, "on\n").unwrap();
        assert_eq!(switch.read().unwrap().servers.len(), 0);
        // The runtime override stands encrypted DNS aside.
        fs::write(&switch.overridden, "").unwrap();
        assert!(switch.read().is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn knows_local_names() {
        assert!(is_local("router.lan"));
        assert!(is_local("lan"));
        assert!(is_local("nas.home.arpa"));
        assert!(is_local("1.1.168.192.in-addr.arpa"));
        assert!(!is_local("plan"));
        assert!(!is_local("example.com"));
        assert!(!is_local("1.1.1.1.in-addr.arpa"));
    }

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
