//! `losos-vpn`, on the host and in sys-vpn (docs/vpn.md).
//!
//! On the host:
//!
//!   losos-vpn providers         the providers it takes, with their owners
//!   losos-vpn status            the provider set, and whether sys-vpn runs
//!   losos-vpn set <provider>    the provider's WireGuard file on stdin
//!   losos-vpn off               back to the network's own way out
//!
//! `set` and `off` run as root through systemd-run, so polkit asks an
//! administrator, as for schizo mode and the edge.
//!
//! In sys-vpn, `losos-vpn up <file>` makes the tunnel and keeps it up.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use miette::{miette, IntoDiagnostic, Result, WrapErr};
use tracing::{info, warn};

use losos_vpn::{Provider, Tunnel, PROVIDER_FILE, STATE_DIR, TUNNEL_FILE};

/// The tunnel's interface in sys-vpn.
const LINK: &str = "wg0";
/// The table the inside's connections take: the tunnel, or nowhere. Also
/// the mark the tunnel's own packets carry, which keeps them out of it.
const TABLE: u32 = 51820;
/// The inside card's name in sys-vpn (nixos/guest/vpn.nix).
const INSIDE: &str = "inside";
/// A handshake older than this means the tunnel is gone: WireGuard
/// rekeys every two minutes while traffic flows, and the keepalive keeps
/// it flowing.
const STALE: Duration = Duration::from_secs(180);
/// The unit `set` and `off` start and stop.
const TARGET: &str = "losos-vpn.target";
const MACHINE_UNIT: &str = "losos-service-machine-sys-vpn.service";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .without_time()
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["providers"] => {
            providers();
            Ok(())
        }
        [] | ["status"] => status(),
        ["set", provider] => as_root(&args).and_then(|()| set(provider)),
        ["off"] => as_root(&args).and_then(|()| off()),
        ["check", provider] => check(provider),
        ["up", file] => up(Path::new(file)),
        _ => Err(miette!(
            "usage: losos-vpn [providers|status|set <provider>|off|check <provider>|up <file>]"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

fn providers() {
    for p in Provider::ALL {
        println!("{}  {}", p.id(), p.name());
        println!("    run by     {}", p.operator());
        println!("    owned by   {}", p.owners());
        println!("    WireGuard  {}", p.config_page());
        for s in p.sources() {
            println!("    source     {s}");
        }
    }
}

fn status() -> Result<()> {
    let provider = std::fs::read_to_string(Path::new(STATE_DIR).join(PROVIDER_FILE))
        .ok()
        .and_then(|t| t.trim().parse::<Provider>().ok());
    let Some(provider) = provider else {
        println!("vpn=none");
        return Ok(());
    };
    println!("vpn={}", provider.id());
    // Root's alone; another user sees only which provider.
    if let Ok(text) = std::fs::read_to_string(Path::new(STATE_DIR).join(TUNNEL_FILE)) {
        if let Ok(t) = Tunnel::parse(&text) {
            println!("endpoint={}", t.peer.endpoint());
        }
    }
    let active = Command::new("systemctl")
        .args(["is-active", "--quiet", MACHINE_UNIT])
        .status()
        .is_ok_and(|s| s.success());
    println!("machine={}", if active { "running" } else { "stopped" });
    Ok(())
}

/// Runs this command again as root through systemd-run when it is not
/// root already, and exits with its status.
fn as_root(args: &[&str]) -> Result<()> {
    let uid = std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .into_diagnostic()?;
    if uid == 0 {
        return Ok(());
    }
    let me = std::env::current_exe().into_diagnostic()?;
    let status = Command::new("systemd-run")
        .args([
            "--quiet",
            "--pipe",
            "--wait",
            "--collect",
            "--service-type=exec",
            "-p",
            "ProtectHome=yes",
        ])
        .arg(me)
        .args(args)
        .status()
        .into_diagnostic()
        .wrap_err("cannot run systemd-run")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn read_tunnel(provider: &str) -> Result<(Provider, Tunnel)> {
    let provider: Provider = provider.parse().map_err(|e: String| miette!(e))?;
    let mut text = String::new();
    std::io::stdin()
        .take(64 * 1024)
        .read_to_string(&mut text)
        .into_diagnostic()
        .wrap_err("cannot read the WireGuard file on stdin")?;
    let tunnel = Tunnel::parse(&text).map_err(|e| miette!("{}'s file: {e}", provider.name()))?;
    Ok((provider, tunnel))
}

fn check(provider: &str) -> Result<()> {
    let (provider, tunnel) = read_tunnel(provider)?;
    println!(
        "{}: endpoint {}, address {}",
        provider.name(),
        tunnel.peer.endpoint(),
        tunnel
            .addresses
            .iter()
            .map(|(a, p)| format!("{a}/{p}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    let partial = path.with_extension("new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot write {}", partial.display()))?;
    file.write_all(text.as_bytes()).into_diagnostic()?;
    std::fs::rename(&partial, path).into_diagnostic()
}

fn set(provider: &str) -> Result<()> {
    let (provider, tunnel) = read_tunnel(provider)?;
    std::fs::create_dir_all(STATE_DIR).into_diagnostic()?;
    std::fs::set_permissions(STATE_DIR, std::fs::Permissions::from_mode(0o700))
        .into_diagnostic()?;
    write_private(&Path::new(STATE_DIR).join(TUNNEL_FILE), &tunnel.render())?;
    write_private(
        &Path::new(STATE_DIR).join(PROVIDER_FILE),
        &format!("{}\n", provider.id()),
    )?;
    // A disposable machine: a new one, with the new file, every time.
    systemctl(&["restart", TARGET])?;
    println!("{} through {}", provider.name(), tunnel.peer.endpoint());
    Ok(())
}

fn off() -> Result<()> {
    for name in [TUNNEL_FILE, PROVIDER_FILE] {
        let _ = std::fs::remove_file(Path::new(STATE_DIR).join(name));
    }
    systemctl(&["stop", TARGET])
}

fn systemctl(args: &[&str]) -> Result<()> {
    let ok = Command::new("systemctl")
        .args(args)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Ok(())
    } else {
        Err(miette!("systemctl {} failed", args.join(" ")))
    }
}

fn ip(args: &[&str]) -> bool {
    Command::new("ip")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The endpoint's IPv4 address, asked of the network machine's resolver
/// through the uplink. None while it cannot say.
fn resolve(tunnel: &Tunnel) -> Option<Ipv4Addr> {
    if let Ok(ip) = tunnel.peer.host.parse::<Ipv4Addr>() {
        return Some(ip);
    }
    let found = (tunnel.peer.host.as_str(), tunnel.peer.port)
        .to_socket_addrs()
        .ok()?
        .find_map(|a| match a {
            SocketAddr::V4(v4) => Some(*v4.ip()),
            SocketAddr::V6(_) => None,
        });
    if found.is_none() {
        warn!(host = %tunnel.peer.host, "no IPv4 address");
    }
    found
}

/// The tunnel, from nothing: the interface, its keys and peer, its
/// addresses, and the inside's route into it.
fn tunnel_up(tunnel: &Tunnel, endpoint: Ipv4Addr) -> Result<()> {
    ip(&["link", "del", LINK]);
    if !ip(&["link", "add", LINK, "type", "wireguard"]) {
        return Err(miette!("cannot make {LINK}"));
    }
    let mut wg = Command::new("wg")
        .args(["setconf", LINK, "/dev/stdin"])
        .stdin(Stdio::piped())
        .spawn()
        .into_diagnostic()?;
    if let Some(mut stdin) = wg.stdin.take() {
        stdin
            .write_all(tunnel.wg_config(endpoint, TABLE).as_bytes())
            .into_diagnostic()?;
    }
    if !wg.wait().into_diagnostic()?.success() {
        return Err(miette!("wg setconf failed"));
    }
    for (addr, prefix) in &tunnel.addresses {
        ip(&[
            "-4",
            "addr",
            "add",
            &format!("{addr}/{prefix}"),
            "dev",
            LINK,
        ]);
    }
    let mtu = tunnel.mtu.unwrap_or(1420).to_string();
    if !ip(&["link", "set", LINK, "mtu", &mtu, "up"]) {
        return Err(miette!("cannot bring {LINK} up"));
    }
    let table = TABLE.to_string();
    if !ip(&[
        "-4", "route", "replace", "default", "dev", LINK, "table", &table,
    ]) {
        return Err(miette!("cannot route through {LINK}"));
    }
    Ok(())
}

/// What the inside's connections take while the tunnel is down: nowhere.
/// Set once; the tunnel's own default route comes ahead of it.
fn kill_switch() {
    let table = TABLE.to_string();
    ip(&[
        "-4",
        "route",
        "replace",
        "unreachable",
        "default",
        "metric",
        "4096",
        "table",
        &table,
    ]);
    while ip(&["-4", "rule", "del", "pref", "100"]) {}
    ip(&[
        "-4", "rule", "add", "pref", "100", "iif", INSIDE, "lookup", &table,
    ]);
}

/// Seconds since the last handshake, if there was one.
fn last_handshake() -> Option<u64> {
    let out = Command::new("wg")
        .args(["show", LINK, "latest-handshakes"])
        .output()
        .ok()?;
    let at: u64 = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (at > 0).then(|| now.saturating_sub(at))
}

fn up(file: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot read {}", file.display()))?;
    let tunnel = Tunnel::parse(&text).map_err(|e| miette!("{}: {e}", file.display()))?;
    kill_switch();
    let mut since: Option<Instant> = None;
    loop {
        let fresh = last_handshake().is_some_and(|age| age < STALE.as_secs());
        let young = since.is_some_and(|s| s.elapsed() < STALE);
        if !fresh && !young {
            // Gone, or never there: a new tunnel, to the endpoint's
            // address as it is now.
            match resolve(&tunnel) {
                Some(endpoint) => match tunnel_up(&tunnel, endpoint) {
                    Ok(()) => {
                        info!(%endpoint, "tunnel up");
                        since = Some(Instant::now());
                    }
                    Err(e) => {
                        warn!("{e}");
                        ip(&["link", "del", LINK]);
                    }
                },
                None => {
                    ip(&["link", "del", LINK]);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
