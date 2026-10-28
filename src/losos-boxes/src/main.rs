//! Offers the LosOS boxes on the local network as applications.
//!
//!     losos-boxes watch [--terminal]  keep an entry per box in the app list
//!     losos-boxes list                print the boxes found in a few seconds
//!     losos-boxes clear               remove the entries `watch` wrote
//!     losos-boxes terminal <url>      sign in to a box when needed, then open it
//!     losos-boxes search <text>       find text in the files and Git of the
//!                                     boxes `losos` is signed in to, as JSON
//!     losos-boxes route               pass the other machines' connections
//!                                     to a box on (the LosOS boxes machine)
//!     losos-boxes relay               let the LosOS boxes machine use this
//!                                     host's resolved and name its boxes
//!
//! `watch` is a user service in the LosOS boxes machine
//! (nixos/guest/boxes.nix). It browses `_losos._tcp` and `_workstation._tcp`
//! through systemd-resolved and writes a `.desktop` file for each box into
//! the user's applications directory, which the machine's `losos-guest-run`
//! lists for the host's desktop. `terminal` is what those entries run in
//! derisk-terminal. The machine has no mDNS of its own, so the resolved it
//! browses through is the host's: `LOSOS_BOXES_RESOLVE` names the socket
//! that reaches it, and `relay`, a service on the host
//! (nixos/modules/losos-boxes.nix), is the other end.
//!
//! The other machines reach a box through the LosOS boxes machine too.
//! `route`, a system service there, finds the boxes the same way, tells the
//! host's relay their names, and the relay has the host's DNS for machines
//! answer those names with the LosOS boxes machine's address. `route` then
//! passes each connection on ports 80 and 443 to the box its `Host` header
//! or TLS server name asks for, and refuses any other name.

use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::net::{IpAddr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV6, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use serde_json::{json, Value};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use losos_boxes::{
    browse_events, frame, hostname_addresses, hosts_file, http_host, is_box_state,
    machine_cid_and_template, ok_body, parse_reply, parse_resolved, resolve_parameters,
    state_request, tls_record_len, tls_server_name, Boxes, Change, Instance, LososBox, Opens, Seen,
    BOX_TYPE, DOMAIN, ENTRY_PREFIX, HOSTS_DIR, MACHINE_TEMPLATE, NAMES_PORT, RELAY_PORT,
    RESOLVE_SOCKET, ROUTED_PORTS, WORKSTATION_TYPE,
};

/// How long a workstation gets to answer for `/setup/state.json`, per
/// address. A box on the LAN answers in milliseconds.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a connection `route` takes gets to say which box it is for.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// The most `route` reads of a request or ClientHello before it knows.
const HELLO_MAX: usize = 16 * 1024;
/// How long `route` waits before it browses or tells the host again, when
/// either stopped.
const ROUTE_RETRY: Duration = Duration::from_secs(5);
/// How long `list` browses: mDNS answers a query within a second, and
/// resolved asks again shortly after.
const LIST_FOR: Duration = Duration::from_secs(4);

fn main() -> ExitCode {
    // stderr, which is the journal for a service.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .without_time()
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("watch") => match args.get(1).map(String::as_str) {
            None => entries_dir().and_then(|dir| watch(&dir, Opens::AdminPages)),
            Some("--terminal") => entries_dir().and_then(|dir| watch(&dir, Opens::Terminal)),
            Some(_) => Err(miette!("usage: losos-boxes watch [--terminal]")),
        },
        Some("relay") => relay(),
        Some("route") => route(),
        Some("list") => list(),
        Some("clear") => entries_dir().and_then(|dir| clear(&dir)),
        Some("terminal") => match args.get(1) {
            Some(url) => return terminal(url),
            None => Err(miette!("usage: losos-boxes terminal <url>")),
        },
        Some("search") => match &args[1..] {
            [text] => search(text),
            _ => Err(miette!("usage: losos-boxes search <text>")),
        },
        _ => Err(miette!(
            "usage: losos-boxes watch [--terminal] | list | clear | terminal <url> | search <text> | route | relay"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

/// `$XDG_DATA_HOME/applications`, which derisk reads before the system's.
fn entries_dir() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| miette!("neither XDG_DATA_HOME nor HOME is set"))?;
    Ok(data.join("applications"))
}

/// A connection to resolved's Varlink socket.
struct Varlink {
    reader: BufReader<UnixStream>,
}

/// The socket resolved answers on: its own, or the one `LOSOS_BOXES_RESOLVE`
/// names, which in the LosOS boxes machine reaches the host's.
fn resolve_socket() -> PathBuf {
    std::env::var_os("LOSOS_BOXES_RESOLVE")
        .filter(|s| !s.is_empty())
        .map_or_else(|| PathBuf::from(RESOLVE_SOCKET), PathBuf::from)
}

impl Varlink {
    fn connect() -> Result<Varlink> {
        let socket = resolve_socket();
        let stream = UnixStream::connect(&socket)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot reach systemd-resolved at {}", socket.display()))?;
        Ok(Varlink {
            reader: BufReader::new(stream),
        })
    }

    /// The stream under the reader, for shutting it down from another thread.
    fn handle(&self) -> Result<UnixStream> {
        self.reader.get_ref().try_clone().into_diagnostic()
    }

    fn send(&mut self, method: &str, parameters: Value, more: bool) -> Result<()> {
        self.reader
            .get_mut()
            .write_all(&frame(method, parameters, more))
            .into_diagnostic()
    }

    /// The next reply, or `None` when resolved hung up.
    fn receive(&mut self) -> Result<Option<losos_boxes::Reply>> {
        let mut message = Vec::new();
        let read = self.reader.read_until(0, &mut message).into_diagnostic()?;
        if read == 0 || message.last() != Some(&0) {
            return Ok(None);
        }
        message.pop();
        parse_reply(&message).map(Some).map_err(|e| miette!("{e}"))
    }

    /// One call and its one reply.
    fn call(&mut self, method: &str, parameters: Value) -> Result<Value> {
        self.send(method, parameters, false)?;
        match self.receive()? {
            Some(reply) => Ok(reply.parameters),
            None => bail!("systemd-resolved hung up during {method}"),
        }
    }
}

/// Browses `service_type` until resolved hangs up or the connection is shut
/// down, sending each instance it adds or removes. Returns why it stopped.
fn browse(
    mut varlink: Varlink,
    service_type: &str,
    seen: crossbeam_channel::Sender<Seen>,
) -> Result<()> {
    varlink.send(
        "io.systemd.Resolve.BrowseServices",
        json!({ "type": service_type, "domain": DOMAIN }),
        true,
    )?;
    while let Some(reply) = varlink.receive()? {
        for event in browse_events(&reply.parameters) {
            if seen.send(event).is_err() {
                return Ok(());
            }
        }
        if !reply.continues {
            break;
        }
    }
    bail!("systemd-resolved stopped browsing {service_type}")
}

/// What `instance` is, if it is a box: resolved for its host, and, for a
/// workstation, asked for a box's state.
fn identify(resolver: &mut Varlink, instance: &Instance) -> Result<Option<LososBox>> {
    let reply = resolver.call(
        "io.systemd.Resolve.ResolveService",
        resolve_parameters(instance),
    )?;
    let Some(resolved) = parse_resolved(&reply) else {
        return Ok(None);
    };
    if instance.service_type == BOX_TYPE {
        return Ok(LososBox::announced(&resolved));
    }
    // A workstation: a box serves its state on its front port, 80, whatever
    // port the workstation record names.
    Ok(LososBox::new(&resolved.host, 80, None).filter(|b| probe(&b.host, &resolved.addresses)))
}

/// Whether the machine at `addresses` answers `/setup/state.json` as a box.
/// IPv4 first: a box's nginx listens there on every version so far.
fn probe(host: &str, addresses: &[(IpAddr, u32)]) -> bool {
    let mut ordered: Vec<&(IpAddr, u32)> = addresses.iter().collect();
    ordered.sort_by_key(|(ip, _)| ip.is_ipv6());
    ordered.into_iter().any(|&(ip, ifindex)| {
        let socket = match ip {
            IpAddr::V4(_) => SocketAddr::new(ip, 80),
            IpAddr::V6(v6) => {
                // A link-local address means nothing without its interface.
                let scope = if v6.segments()[0] & 0xffc0 == 0xfe80 {
                    ifindex
                } else {
                    0
                };
                SocketAddr::V6(SocketAddrV6::new(v6, 80, 0, scope))
            }
        };
        let answer = (|| -> io::Result<Vec<u8>> {
            let mut stream = TcpStream::connect_timeout(&socket, PROBE_TIMEOUT)?;
            stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
            stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
            stream.write_all(state_request(host).as_bytes())?;
            let mut response = Vec::new();
            stream.take(64 * 1024).read_to_end(&mut response)?;
            Ok(response)
        })();
        match answer {
            Ok(response) => ok_body(&response).is_some_and(is_box_state),
            Err(e) => {
                debug!(%host, %socket, error = %e, "no answer for /setup/state.json");
                false
            }
        }
    })
}

/// Browses both types and hands every change in the boxes present to
/// `apply`, until resolved stops browsing or `until` passes. Two threads
/// read the browsers; this one resolves and probes, one instance at a time,
/// so the order of a removal after its addition is kept.
fn run(until: Option<Instant>, mut apply: impl FnMut(&Change) -> Result<()>) -> Result<Boxes> {
    let browsers = [BOX_TYPE, WORKSTATION_TYPE]
        .into_iter()
        .map(|t| Ok((t, Varlink::connect()?)))
        .collect::<Result<Vec<_>>>()?;
    let handles = browsers
        .iter()
        .map(|(_, v)| v.handle())
        .collect::<Result<Vec<_>>>()?;
    let mut resolver = Varlink::connect()?;
    let mut boxes = Boxes::default();

    let (seen_tx, seen) = crossbeam_channel::bounded::<Seen>(64);
    let (done_tx, done) = crossbeam_channel::bounded::<Result<()>>(browsers.len());
    std::thread::scope(|scope| {
        for (service_type, varlink) in browsers {
            let (seen_tx, done_tx) = (seen_tx.clone(), done_tx.clone());
            scope.spawn(move || {
                let _ = done_tx.send(browse(varlink, service_type, seen_tx));
            });
        }
        drop(seen_tx);

        let deadline = match until {
            Some(until) => crossbeam_channel::at(until),
            None => crossbeam_channel::never(),
        };
        let outcome = (|| -> Result<()> {
            loop {
                let event = crossbeam_channel::select! {
                    recv(seen) -> event => match event {
                        Ok(event) => event,
                        // Both browsers stopped; the first reason is the one to report.
                        Err(_) => return done.try_recv().unwrap_or(Ok(())),
                    },
                    // One browser stopping is the end of watching: a box
                    // could no longer leave the list, or join it.
                    recv(done) -> why => return why.unwrap_or(Ok(())),
                    recv(deadline) -> _ => return Ok(()),
                };
                let change = match event {
                    Seen::Added(instance) => match identify(&mut resolver, &instance) {
                        Ok(Some(found)) => boxes.add(instance, found),
                        Ok(None) => None,
                        Err(e) => {
                            debug!(name = %instance.name, error = ?e, "cannot resolve");
                            None
                        }
                    },
                    Seen::Removed(instance) => boxes.remove(&instance),
                };
                if let Some(change) = change {
                    apply(&change)?;
                }
            }
        })();

        // Wakes the browsers from their reads, so the scope can end.
        for handle in &handles {
            let _ = handle.shutdown(Shutdown::Both);
        }
        outcome
    })?;
    Ok(boxes)
}

/// Keeps one entry per box present, until resolved stops browsing, which
/// the service restarts after. Entries from an earlier run go first: their
/// boxes may have left while nothing watched.
///
/// A box `losos` remembers an edge address for gets an entry too while it
/// is not on this network (`remembered`), so it can be reached away from
/// home. Its entry is the same file, and the box's own replaces it as soon
/// as the box is found here.
fn watch(dir: &Path, opens: Opens) -> Result<()> {
    clear(dir)?;
    std::fs::create_dir_all(dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot create {}", dir.display()))?;
    info!(dir = %dir.display(), "watching for LosOS boxes");
    // The hosts found on this network, which a remembered box's entry must
    // not replace; held while either side writes, so neither overwrites
    // the other's newer entry.
    let present = Mutex::new(BTreeSet::<String>::new());
    let (stop_tx, stop) = crossbeam_channel::bounded::<()>(0);
    std::thread::scope(|scope| {
        scope.spawn(|| remembered(dir, &present, &stop));
        let watched = run(None, |change| {
            let mut present = present.lock().unwrap_or_else(PoisonError::into_inner);
            match change {
                Change::Write(found) => {
                    info!(host = %found.host, version = ?found.version, "box present");
                    present.insert(found.host.clone());
                    write_entry(dir, &found.file_name(), &found.desktop_entry(opens))
                }
                Change::Remove(file) => {
                    info!(%file, "box gone");
                    if let Some(host) = file
                        .strip_prefix(ENTRY_PREFIX)
                        .and_then(|f| f.strip_suffix(".desktop"))
                    {
                        present.remove(host);
                    }
                    remove(&dir.join(file))
                }
            }
        });
        drop(stop_tx);
        watched
    })?;
    Ok(())
}

/// How often `watch` reads which boxes `losos` remembers: a sign-in or a
/// sign-out shows within a minute.
const REMEMBER_EVERY: Duration = Duration::from_secs(60);

/// Writes an entry for each box `losos` remembers an edge address for
/// while it is not on this network, and removes it once `losos` forgets
/// the box, every [`REMEMBER_EVERY`] until `stop` closes. It reads nothing
/// while the keyring is locked, so it never brings up the keyring's
/// password window, and leaves the entries it wrote as they were.
fn remembered(
    dir: &Path,
    present: &Mutex<BTreeSet<String>>,
    stop: &crossbeam_channel::Receiver<()>,
) {
    let mut away = BTreeSet::<String>::new();
    loop {
        if keyring_unlocked() {
            match Command::new("losos")
                .args(["--lang", "en", "hosts"])
                .output()
            {
                Ok(out) => {
                    let listed = String::from_utf8_lossy(&out.stdout);
                    let remembered: Vec<(LososBox, String, String)> =
                        losos_boxes::remembered_boxes(&listed)
                            .into_iter()
                            .filter_map(|(url, edge)| Some((LososBox::at(&url)?, url, edge)))
                            .collect();
                    let present = present.lock().unwrap_or_else(PoisonError::into_inner);
                    for (found, url, edge) in &remembered {
                        if present.contains(&found.host) {
                            away.remove(&found.host);
                            continue;
                        }
                        if let Err(e) =
                            write_entry(dir, &found.file_name(), &found.away_entry(url, edge))
                        {
                            warn!(host = %found.host, "{e:?}");
                        } else if away.insert(found.host.clone()) {
                            info!(host = %found.host, %edge, "box away from home");
                        }
                    }
                    let forgotten: Vec<String> = away
                        .iter()
                        .filter(|host| !remembered.iter().any(|(b, _, _)| &b.host == *host))
                        .cloned()
                        .collect();
                    for host in forgotten {
                        away.remove(&host);
                        if !present.contains(&host) {
                            info!(%host, "box no longer remembered");
                            let _ = remove(&dir.join(format!("{ENTRY_PREFIX}{host}.desktop")));
                        }
                    }
                }
                Err(e) => debug!(error = %e, "cannot run losos"),
            }
        }
        if !matches!(
            stop.recv_timeout(REMEMBER_EVERY),
            Err(crossbeam_channel::RecvTimeoutError::Timeout)
        ) {
            return;
        }
    }
}

/// Writes `text` as the entry `file` in `dir`, whole or not at all.
fn write_entry(dir: &Path, file: &str, text: &str) -> Result<()> {
    let path = dir.join(file);
    let partial = dir.join(format!(".{file}.partial"));
    std::fs::write(&partial, text)
        .and_then(|()| std::fs::rename(&partial, &path))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot write {}", path.display()))
}

/// Prints the boxes found within a few seconds, one per line.
fn list() -> Result<()> {
    let boxes = run(Some(Instant::now() + LIST_FOR), |_| Ok(()))?;
    let mut out = io::stdout().lock();
    for found in boxes.iter() {
        let version = found.version.as_deref().unwrap_or("");
        writeln!(out, "{}\t{}\t{version}", found.name(), found.url()).into_diagnostic()?;
    }
    Ok(())
}

/// Removes every entry `watch` wrote.
fn clear(dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).into_diagnostic(),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(ENTRY_PREFIX) && name.ends_with(".desktop") {
            remove(&entry.path())?;
        }
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot remove {}", path.display())),
        _ => Ok(()),
    }
}

/// Opens `url` in `losos`, signing in first when `losos hosts` does not list
/// it. Runs in a terminal window of its own, which closes when this returns,
/// so a failure waits for Enter: `losos` has said what went wrong, and the
/// window should stay until it has been read.
fn terminal(url: &str) -> ExitCode {
    let result = (|| -> Result<()> {
        let hosts = Command::new("losos")
            .arg("hosts")
            .output()
            .into_diagnostic()
            .wrap_err("cannot run losos")?;
        let listed = String::from_utf8_lossy(&hosts.stdout);
        if !losos_boxes::logged_in_hosts(&listed).any(|h| h == url) {
            run_losos(&["login", url])?;
        }
        run_losos(&["open", url])
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(report) => {
            warn!(%url, "{report}");
            eprint!("\nPress Enter to close this window.");
            let _ = io::stdin().lock().read_line(&mut String::new());
            ExitCode::FAILURE
        }
    }
}

fn run_losos(args: &[&str]) -> Result<()> {
    let status = Command::new("losos")
        .args(args)
        .status()
        .into_diagnostic()
        .wrap_err("cannot run losos")?;
    if !status.success() {
        bail!("losos {} did not finish", args[0]);
    }
    Ok(())
}

/// The most boxes one search asks: each is two calls through `losos`.
const SEARCH_BOXES: usize = 3;

/// Prints, as one JSON object `{"hits": [...]}`, what the boxes `losos` is
/// signed in to find for `text` in LosOS Cloud's files and LosOS Git's
/// repositories (`losos_boxes::text_hits` has a row's shape). It is how
/// derisk's command palette searches the boxes (docs/losos-boxes.md, "In
/// the command palette"), and it asks nothing of the person: with the
/// keyring locked it finds nothing rather than have the keyring ask for its
/// password, and a box that does not answer is left out.
fn search(text: &str) -> Result<()> {
    let hits = if keyring_unlocked() {
        let hosts = Command::new("losos")
            .args(["--lang", "en", "hosts"])
            .output()
            .into_diagnostic()
            .wrap_err("cannot run losos")?;
        let listed = String::from_utf8_lossy(&hosts.stdout);
        let boxes: Vec<&str> = losos_boxes::logged_in_boxes(&listed)
            .take(SEARCH_BOXES)
            .collect();
        let paths = losos_boxes::text_paths(text);
        // Every call at once: each is a round trip to a box and a search
        // there, and the palette is waiting.
        std::thread::scope(|scope| {
            let calls: Vec<_> = boxes
                .iter()
                .flat_map(|url| paths.iter().map(move |(app, path)| (*url, *app, path)))
                .map(|(url, app, path)| {
                    scope.spawn(move || {
                        let answer = call(url, path).unwrap_or_else(|report| {
                            debug!(%url, %path, "{report}");
                            Value::Null
                        });
                        losos_boxes::text_hits(url, app, &answer)
                    })
                })
                .collect();
            calls
                .into_iter()
                .flat_map(|call| call.join().unwrap_or_default())
                .collect::<Vec<_>>()
        })
    } else {
        debug!("the keyring is locked: no box is searched");
        Vec::new()
    };
    println!("{}", json!({ "hits": hits }));
    Ok(())
}

/// `losos call GET <path>` on the box at `url`: its answer's body, which
/// `losos` prints on stdout, the status going to stderr.
fn call(url: &str, path: &str) -> Result<Value> {
    let output = Command::new("losos")
        .args(["--host", url, "call", "GET", path])
        .output()
        .into_diagnostic()
        .wrap_err("cannot run losos")?;
    if !output.status.success() {
        bail!(
            "losos call failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout)
        .into_diagnostic()
        .wrap_err("losos call printed no JSON")
}

/// Whether the Secret Service's default collection, where `losos` keeps its
/// tokens, is open. `losos` asks it to unlock, which makes GNOME Keyring
/// show a password window; a search typed into the palette must not. With
/// no keyring, or no collection yet, nothing has been signed in to either.
fn keyring_unlocked() -> bool {
    Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.freedesktop.secrets",
            "/org/freedesktop/secrets/aliases/default",
            "org.freedesktop.Secret.Collection",
            "Locked",
        ])
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout.trim_ascii() == b"b false")
}

/// Lets the LosOS boxes machine browse and resolve through this host's
/// resolved, which does mDNS on every network the host is on, the LAN
/// included: a machine behind libvirt's NAT can multicast only to the other
/// machines. Each vsock connection is let through to resolved's Varlink
/// socket if libvirt says the machine at its address was made from the
/// LosOS boxes template, and refused otherwise; the address is the one the
/// kernel stamps on the connection, so a machine cannot claim another's.
/// What goes through is resolved's lookup API: names, addresses, services,
/// records and browsing, nothing that changes the host.
///
/// On a second port it takes the names of the boxes that machine found, and
/// writes them into a hosts file for libvirt's dnsmasq at that machine's
/// address, so the other machines' connections to a box go to it. The file
/// goes when the machine hangs up: a stopped machine routes nothing.
fn relay() -> Result<()> {
    let listen = |port: u32| {
        vsock::VsockListener::bind(&vsock::VsockAddr::new(vsock::VMADDR_CID_ANY, port))
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot listen on vsock port {port}"))
    };
    let (lookups, names) = (listen(RELAY_PORT)?, listen(NAMES_PORT)?);
    info!(
        lookups = RELAY_PORT,
        names = NAMES_PORT,
        "relaying resolved to the LosOS boxes machine"
    );
    std::thread::scope(|scope| {
        for (listener, serve) in [
            (&lookups, relay_one as fn(vsock::VsockStream) -> Result<()>),
            (&names, name_boxes),
        ] {
            scope.spawn(move || {
                std::thread::scope(|connections| {
                    for stream in listener.incoming() {
                        match stream {
                            Ok(stream) => {
                                connections.spawn(move || {
                                    if let Err(e) = serve(stream) {
                                        warn!("{e:?}");
                                    }
                                });
                            }
                            Err(e) => warn!(error = %e, "cannot accept"),
                        }
                    }
                });
            });
        }
    });
    Ok(())
}

/// Keeps the hosts file of the LosOS boxes machine on `stream` in step with
/// the names it sends: one per line, each list ended by an empty line.
fn name_boxes(stream: vsock::VsockStream) -> Result<()> {
    let cid = stream.peer_addr().into_diagnostic()?.cid();
    let Some(name) = machine_at(cid)? else {
        info!(cid, "refused: not a LosOS boxes machine");
        return Ok(());
    };
    let address = machine_address(&name)?;
    let file = Path::new(HOSTS_DIR).join(&name);
    info!(%name, %address, "naming its boxes");
    let mut list = BTreeSet::new();
    let result = (|| -> Result<()> {
        for line in BufReader::new(stream).lines() {
            let line = line.into_diagnostic()?;
            if !line.is_empty() {
                // Only a name a box could have reaches dnsmasq.
                if let Some(found) = LososBox::new(&line, 80, None) {
                    list.insert(found.host);
                }
                continue;
            }
            // Beside the directory, not in it: dnsmasq reads every file there.
            let partial = Path::new(HOSTS_DIR).with_file_name(format!(".{name}.partial"));
            std::fs::write(
                &partial,
                hosts_file(address, list.iter().map(String::as_str)),
            )
            .and_then(|()| std::fs::rename(&partial, &file))
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot write {}", file.display()))?;
            info!(%name, boxes = ?list, "named");
            list.clear();
        }
        Ok(())
    })();
    info!(%name, "stopped naming its boxes");
    remove(&file)?;
    result
}

fn relay_one(machine: vsock::VsockStream) -> Result<()> {
    let cid = machine.peer_addr().into_diagnostic()?.cid();
    let Some(name) = machine_at(cid)? else {
        info!(cid, "refused: not a LosOS boxes machine");
        return Ok(());
    };
    debug!(%name, cid, "relaying");
    let socket = UnixStream::connect(RESOLVE_SOCKET)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot reach systemd-resolved at {RESOLVE_SOCKET}"))?;
    let (mut from_machine, mut to_machine) = (
        machine.try_clone().into_diagnostic()?,
        machine.try_clone().into_diagnostic()?,
    );
    let (mut from_resolved, mut to_resolved) = (socket.try_clone().into_diagnostic()?, socket);
    // Either side hanging up ends both: a browse lasts until the machine
    // closes it, and resolved ends one when it restarts.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = io::copy(&mut from_machine, &mut to_resolved);
            let _ = to_resolved.shutdown(Shutdown::Both);
            let _ = machine.shutdown(Shutdown::Both);
        });
        let _ = io::copy(&mut from_resolved, &mut to_machine);
        let _ = to_machine.shutdown(Shutdown::Both);
        let _ = from_resolved.shutdown(Shutdown::Both);
    });
    Ok(())
}

/// The address libvirt's DHCP gave the machine `name` on its network.
fn machine_address(name: &str) -> Result<IpAddr> {
    let output = virsh(&["domifaddr", name, "--source", "lease"])?;
    output
        .lines()
        .filter_map(|line| line.split_whitespace().nth(3))
        .filter_map(|cidr| cidr.split('/').next()?.parse::<IpAddr>().ok())
        .min_by_key(IpAddr::is_ipv6)
        .ok_or_else(|| miette!("libvirt has no address for the machine {name} yet"))
}

/// The name of the running machine at vsock address `cid`, if libvirt has
/// it as made from the LosOS boxes template. A read-only connection, which
/// every account has, is enough to ask.
fn machine_at(cid: u32) -> Result<Option<String>> {
    for name in virsh(&["list", "--name"])?
        .lines()
        .filter(|n| !n.is_empty())
    {
        let xml = match virsh(&["dumpxml", name]) {
            Ok(xml) => xml,
            // It stopped between the two calls.
            Err(e) => {
                debug!(%name, error = ?e, "skipped");
                continue;
            }
        };
        if let (Some(at), template) = machine_cid_and_template(&xml) {
            if at == cid {
                return Ok((template == Some(MACHINE_TEMPLATE)).then(|| name.to_owned()));
            }
        }
    }
    Ok(None)
}

/// virsh on a read-only connection, which every account has.
fn virsh(args: &[&str]) -> Result<String> {
    let output = Command::new("virsh")
        .args(["-r", "-q"])
        .args(args)
        .output()
        .into_diagnostic()
        .wrap_err("cannot run virsh")?;
    if !output.status.success() {
        bail!(
            "virsh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The LosOS boxes machine's half of the other machines' way to a box: it
/// browses for boxes, tells the host's relay their names whenever they
/// change, and passes on each connection to ports 80 and 443 that names one.
fn route() -> Result<()> {
    let listeners = ROUTED_PORTS
        .into_iter()
        .map(|port| {
            // One socket for both families: the other machines are on
            // libvirt's IPv4 network, but nothing here depends on it.
            TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port))
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot listen on port {port}"))
                .map(|listener| (port, listener))
        })
        .collect::<Result<Vec<_>>>()?;
    let boxes = RwLock::new(BTreeSet::<String>::new());
    std::thread::scope(|scope| {
        for (port, listener) in &listeners {
            let boxes = &boxes;
            scope.spawn(move || {
                std::thread::scope(|connections| {
                    for stream in listener.incoming() {
                        match stream {
                            Ok(stream) => {
                                connections.spawn(move || {
                                    if let Err(e) = pass_on(stream, *port, boxes) {
                                        debug!(port, "{e:?}");
                                    }
                                });
                            }
                            Err(e) => warn!(port, error = %e, "cannot accept"),
                        }
                    }
                });
            });
        }
        loop {
            if let Err(e) = name_boxes_to_host(&boxes) {
                warn!("{e:?}");
            }
            boxes.write().unwrap_or_else(|e| e.into_inner()).clear();
            std::thread::sleep(ROUTE_RETRY);
        }
    })
}

/// Browses until resolved or the host stops, keeping `boxes` and the host's
/// list of names in step.
fn name_boxes_to_host(boxes: &RwLock<BTreeSet<String>>) -> Result<()> {
    let mut host =
        vsock::VsockStream::connect(&vsock::VsockAddr::new(vsock::VMADDR_CID_HOST, NAMES_PORT))
            .into_diagnostic()
            .wrap_err_with(|| {
                format!("cannot reach the host's relay on vsock port {NAMES_PORT}")
            })?;
    // An empty list first, so the host drops what an earlier run named.
    host.write_all(b"\n").into_diagnostic()?;
    info!("routing the other machines' connections to boxes");
    run(None, |change| {
        let mut present = boxes.write().unwrap_or_else(|e| e.into_inner());
        match change {
            Change::Write(found) => {
                present.insert(found.host.clone());
            }
            Change::Remove(file) => {
                present.retain(|host| {
                    LososBox::new(host, 80, None).map(|b| b.file_name()) != Some(file.clone())
                });
            }
        }
        let mut list: String = present.iter().map(|host| format!("{host}\n")).collect();
        list.push('\n');
        host.write_all(list.as_bytes())
            .into_diagnostic()
            .wrap_err("the host's relay hung up")
    })?;
    Ok(())
}

/// Reads which box `client` is for, from its HTTP request or its TLS
/// ClientHello, and if that is a box present, connects it there.
fn pass_on(mut client: TcpStream, port: u16, boxes: &RwLock<BTreeSet<String>>) -> Result<()> {
    client
        .set_read_timeout(Some(HELLO_TIMEOUT))
        .into_diagnostic()?;
    let mut hello = Vec::new();
    let host = loop {
        let mut chunk = [0; 4096];
        let read = client.read(&mut chunk).into_diagnostic()?;
        if read == 0 {
            return Ok(());
        }
        hello.extend_from_slice(&chunk[..read]);
        let found = if port == 443 {
            tls_record_len(&hello)
                .filter(|len| hello.len() >= *len)
                .map(|len| tls_server_name(&hello[..len]))
        } else {
            http_host(&hello).map(Some)
        };
        match found {
            Some(Some(host)) => break host,
            Some(None) => bail!("a TLS client on port {port} named no server"),
            None if hello.len() >= HELLO_MAX => bail!("no host in {HELLO_MAX} bytes"),
            None => {}
        }
    };
    if !boxes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&host)
    {
        bail!("{host} is not a LosOS box");
    }
    let mut resolver = Varlink::connect()?;
    let reply = resolver.call(
        "io.systemd.Resolve.ResolveHostname",
        json!({ "name": host }),
    )?;
    let addresses = hostname_addresses(&reply);
    let mut server = addresses
        .iter()
        .find_map(|ip| TcpStream::connect_timeout(&SocketAddr::new(*ip, port), PROBE_TIMEOUT).ok())
        .ok_or_else(|| miette!("cannot reach {host} at {addresses:?}"))?;
    client.set_read_timeout(None).into_diagnostic()?;
    server.write_all(&hello).into_diagnostic()?;
    debug!(%host, port, "passing on");
    let (mut from_client, mut to_client) = (client.try_clone().into_diagnostic()?, client);
    let (mut from_server, mut to_server) = (server.try_clone().into_diagnostic()?, server);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = io::copy(&mut from_client, &mut to_server);
            let _ = to_server.shutdown(Shutdown::Write);
        });
        let _ = io::copy(&mut from_server, &mut to_client);
        // The box is done; a client that keeps its end open is too.
        let _ = to_client.shutdown(Shutdown::Both);
    });
    Ok(())
}
