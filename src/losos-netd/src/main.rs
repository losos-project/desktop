//! The network machine's one program (docs/network-machine.md).
//!
//!     losos-netd init [<config>]     PID 1: starts the daemon, reaps
//!     losos-netd daemon [<config>]   runs the cards and routing, answers nmcli
//!     losos-netd nmcli <arg>...      nmcli's subset that derisk and Danube use
//!     losos-netd apply               looks at the host's flags now, and says
//!                                    whether schizo mode is as the host asks
//!     losos-netd dhcp <event>        udhcpc's hook

mod daemon;
mod mdns;

use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use losos_netd::{nmcli, Config, CONTROL};

/// Where network-machine.nix puts the configuration in the initrd.
const CONFIG: &str = "/etc/losos-netd.conf";

/// The request `losos-netd apply` sends on the control socket, which no
/// nmcli command line starts with.
pub const APPLY: &str = "\u{1}apply";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);
    // The daemon and init log to the console, which is the machine's
    // serial port and so `virsh console` and libvirt's log on the host. A
    // client's stderr is its caller's, so it logs only what goes wrong.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(match command {
                Some("init" | "daemon") => "info",
                _ => "warn",
            })
        }))
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .without_time()
        .init();

    let config_path = || args.get(1).map(String::as_str).unwrap_or(CONFIG);
    let result = match command {
        Some("init") => init(config_path()),
        Some("daemon") => run_daemon(config_path()),
        Some("nmcli") => return client(&args[1..]),
        Some("apply") => return client(&[APPLY.to_owned()]),
        Some("dhcp") if args.len() == 2 => daemon::dhcp_hook(&args[1]).into_diagnostic(),
        _ => Err(miette!(
            "usage: losos-netd init|daemon [<config>]\n       \
             losos-netd nmcli <arg>...\n       \
             losos-netd apply\n       \
             losos-netd dhcp <event>"
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

/// Powers the machine off, after writing out what the disks hold. libvirt
/// sees the machine stop, which is what its shutdown waits for.
pub fn power_off() -> ! {
    // SAFETY: sync and reboot take no pointers; reboot only returns on
    // failure, which leaves nothing to do but stop.
    unsafe {
        libc::sync();
        libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF);
    }
    std::process::exit(1)
}

/// PID 1. Everything the daemon starts is its child, and the daemon needs
/// to wait for those itself to see them exit, so the daemon is not PID 1:
/// PID 1 only starts it and reaps what is orphaned to it. A daemon that
/// exits takes what it started with it, and starts again, so a fault in it
/// costs the machine a few seconds of network rather than the machine.
fn init(config: &str) -> Result<()> {
    let exe = std::env::current_exe().into_diagnostic()?;
    loop {
        info!("starting the daemon");
        let daemon = std::process::Command::new(&exe)
            .args(["daemon", config])
            .spawn()
            .into_diagnostic()
            .wrap_err("cannot start the daemon")?;
        let pid = daemon.id() as libc::pid_t;
        // Its Child is not waited on through std: PID 1's waitpid(-1)
        // below reaps it with everything else.
        std::mem::forget(daemon);
        loop {
            let mut status = 0;
            // SAFETY: status is a valid int for waitpid to write.
            let reaped = unsafe { libc::waitpid(-1, &mut status, 0) };
            if reaped == pid {
                warn!(status, "the daemon exited");
                break;
            }
            if reaped < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        // Whatever the daemon left running would hold its cards and
        // sockets; kill(-1) from PID 1 reaches every process but itself.
        // SAFETY: plain syscalls on integers.
        unsafe { libc::kill(-1, libc::SIGKILL) };
        // SAFETY: as above.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        // mdev, which loads a device's driver as it comes, went with them.
        if let Err(e) = std::process::Command::new("mdev").arg("-d").status() {
            warn!(error = %e, "cannot start mdev");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn run_daemon(config_path: &str) -> Result<()> {
    let text = std::fs::read_to_string(config_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot read {config_path}"))?;
    let config = Config::parse(&text).map_err(|e| miette!("{config_path}: {e}"))?;

    daemon::prepare().into_diagnostic()?;
    if let Some((device, path)) = &config.state_disk {
        mount_state(device, path)?;
    }
    // Routing is this machine's job; nothing the host or a card sends
    // should turn it off, and nothing else touches these.
    for (key, value) in [
        ("net/ipv4/ip_forward", "1"),
        ("net/ipv4/conf/all/rp_filter", "1"),
        ("net/ipv4/conf/default/rp_filter", "1"),
        // IPv4 only through the machine (docs/not-done.md): no card takes
        // an IPv6 address a network hands out, so nothing leaves by IPv6
        // around the firewall.
        ("net/ipv6/conf/all/disable_ipv6", "1"),
        ("net/ipv6/conf/default/disable_ipv6", "1"),
    ] {
        if let Err(e) = std::fs::write(Path::new("/proc/sys").join(key), value) {
            warn!(key, error = %e, "cannot set");
        }
    }
    daemon::run_quiet("ip", &["link", "set", "dev", "lo", "up"]);
    if let Some(rules) = &config.nft_base {
        if !daemon::run_quiet("nft", &["-f", &rules.to_string_lossy()]) {
            bail!("cannot load the firewall from {}", rules.display());
        }
    }
    daemon::import(&config);

    let shared = daemon::shared(config);
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            if let Err(e) = daemon::serve(shared) {
                warn!(error = %e, "the control socket");
                std::process::exit(1);
            }
        });
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || daemon::power_button(shared));
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || mdns::reflect(shared));
    }
    daemon::run_loop(shared);
    Ok(())
}

/// Mounts the state disk, giving it a filesystem the first time: the host
/// makes it as an empty file when the machine first starts, and nothing
/// lays it out before then.
fn mount_state(device: &Path, path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).into_diagnostic()?;
    let (device, path) = (device.to_string_lossy(), path.to_string_lossy());
    for attempt in 0..30 {
        if Path::new(&*device).exists() {
            break;
        }
        if attempt == 29 {
            bail!("no state disk at {device}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if daemon::run_quiet(
        "mount",
        &["-o", "noatime,nodev,nosuid,noexec", &device, &path],
    ) {
        return Ok(());
    }
    info!(device = %device, "making the state disk's filesystem");
    if !daemon::run_quiet("mke2fs", &["-L", "losos-network", &device])
        || !daemon::run_quiet(
            "mount",
            &["-o", "noatime,nodev,nosuid,noexec", &device, &path],
        )
    {
        bail!("cannot mount the state disk {device} on {path}");
    }
    Ok(())
}

/// Sends a command line to the daemon and prints its answer, as nmcli
/// would: the output on stdout and status 0, or the error on stderr and
/// nmcli's status for it.
fn client(args: &[String]) -> ExitCode {
    match ask(args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            // nmcli's status when NetworkManager is not running.
            eprintln!("Error: NetworkManager is not running. ({e})");
            ExitCode::from(8)
        }
    }
}

fn ask(args: &[String]) -> io::Result<i32> {
    // A passwd-file holds the secret, which is read here, where the caller
    // can read it (it is the call's stdin as often as not), and sent after
    // the arguments.
    let secret = match nmcli::parse(args) {
        Ok(nmcli::Command {
            request:
                nmcli::Request::Up {
                    passwd_file: Some(file),
                    ..
                },
            ..
        }) => {
            let mut text = Vec::new();
            if file == "/dev/stdin" || file == "-" {
                io::stdin().read_to_end(&mut text)?;
            } else {
                std::fs::File::open(&file)?.read_to_end(&mut text)?;
            }
            text
        }
        _ => Vec::new(),
    };
    let mut request = Vec::new();
    for arg in args {
        request.extend_from_slice(arg.as_bytes());
        request.push(0);
    }
    request.push(0);
    request.extend_from_slice(&secret);

    let mut stream = UnixStream::connect(CONTROL)?;
    stream.write_all(&request)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let monitor = matches!(
        nmcli::parse(args),
        Ok(nmcli::Command {
            request: nmcli::Request::Monitor,
            ..
        })
    );
    if monitor {
        let mut code = None;
        let mut stdout = io::stdout();
        daemon::read_lines(stream, |line| {
            if code.is_none() {
                code = line.parse::<i32>().ok();
                return code == Some(0);
            }
            writeln!(stdout, "{line}")
                .and_then(|()| stdout.flush())
                .is_ok()
        });
        return Ok(code.unwrap_or(1));
    }

    let mut answer = Vec::new();
    stream.read_to_end(&mut answer)?;
    let answer = String::from_utf8_lossy(&answer);
    let (code, text) = answer.split_once('\n').unwrap_or((&answer, ""));
    let code: i32 = code
        .trim()
        .parse()
        .map_err(|_| io::Error::other("a garbled answer"))?;
    if code == 0 {
        print!("{text}");
    } else {
        eprintln!("Error: {}", text.trim_end());
    }
    Ok(code)
}
