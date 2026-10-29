//! Runs derisk's Agent in a machine of each person's own (docs/agent.md,
//! "Its own machine").
//!
//!     losos-agent-machine open [LINK]   open the Agent, starting the machine
//!                                       when it is off (the desktop entry)
//!     losos-agent-machine serve-open    start a person's machine and hand it
//!                                       the link (root, one connection)
//!     losos-agent-machine tools         the person's end of the tools channel
//!                                       (their session)
//!
//! and in the machine:
//!
//!     losos-agent-machine launch        open the Agent's window (one
//!                                       connection from the host's root)
//!     losos-agent-machine relay         derisk's agent socket, carried to the
//!                                       host's tools channel
//!     losos-agent-machine open-uri URI  open a web link on the desktop (the
//!                                       machine's xdg-open)
//!
//! nixos/modules/agent.nix wires the host's half, nixos/guest/agent.nix the
//! machine's.

use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use serde_json::{json, Value};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;
use vsock::{VsockAddr, VsockListener, VsockStream, VMADDR_CID_ANY, VMADDR_CID_HOST};

use losos_agent_machine::{
    agent_link, classify, machine_cid, reply, tools_port, unit, Call, LAUNCH_PORT, MAX_LINE,
    OPEN_SOCKET,
};

/// How long `serve-open` waits for a machine it started to listen. It boots
/// in seconds with KVM; under emulation, with no KVM, it takes minutes.
const BOOT_WAIT: Duration = Duration::from_secs(600);

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
    let result = match (args.first().map(String::as_str), args.get(1)) {
        (Some("open"), link) if args.len() <= 2 => open(link.map_or("", String::as_str)),
        (Some("serve-open"), None) => serve_open(),
        (Some("tools"), None) => tools(),
        (Some("launch"), None) => launch(),
        (Some("relay"), None) => relay(),
        (Some("open-uri"), Some(uri)) if args.len() == 2 => open_uri(uri),
        _ => Err(miette!(
            "usage: losos-agent-machine open [LINK] | serve-open | tools | launch | relay | open-uri URI"
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

/// Reads one line of at most [`MAX_LINE`] bytes, without its newline.
fn read_line(reader: &mut impl Read) -> Result<String> {
    let mut line = Vec::new();
    BufReader::new(reader.take(MAX_LINE + 1))
        .read_until(b'\n', &mut line)
        .into_diagnostic()?;
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if line.len() as u64 > MAX_LINE {
        bail!("line too long");
    }
    String::from_utf8(line).into_diagnostic()
}

/// The answer in a reply line: its result, or its error as the error.
fn answer(line: &str) -> Result<Value> {
    let reply: Value = serde_json::from_str(line)
        .into_diagnostic()
        .wrap_err("bad answer")?;
    if reply["ok"] == true {
        Ok(reply["result"].clone())
    } else {
        Err(miette!(
            "{}",
            reply["error"]
                .as_str()
                .unwrap_or("refused without a reason")
        ))
    }
}

/// `$XDG_RUNTIME_DIR`, the session's.
fn runtime_dir() -> Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| miette!("XDG_RUNTIME_DIR is not set"))
}

/// What the desktop entry runs: asks the host's root end to open the Agent
/// in this person's machine, with `link` when there is one.
fn open(link: &str) -> Result<()> {
    if agent_link(link).is_none() {
        bail!("{link} is not a link the Agent opens");
    }
    let mut stream = UnixStream::connect(OPEN_SOCKET)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot reach the Agent's machine at {OPEN_SOCKET}"))?;
    writeln!(stream, "{link}").into_diagnostic()?;
    answer(&read_line(&mut stream)?).map(drop)
}

/// The uid of the process at the other end of the Unix socket `fd`, which
/// the kernel records when it connects.
fn peer_uid(fd: RawFd) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are a valid buffer for SO_PEERCRED and its size.
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if result == 0 {
        Ok(cred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A vsock connection to `cid:port` from a port under 1024, which only root
/// can bind: how the machine's `launch` knows the host's root sent it.
fn connect_reserved(cid: u32, port: u32) -> io::Result<VsockStream> {
    for local in (600..1024).rev() {
        // SAFETY: plain socket calls on a descriptor this function owns
        // until it hands it to VsockStream or closes it.
        unsafe {
            let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let address = |cid: u32, port: u32| {
                let mut address: libc::sockaddr_vm = std::mem::zeroed();
                address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
                address.svm_cid = cid;
                address.svm_port = port;
                address
            };
            let size = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
            let from = address(VMADDR_CID_ANY, local);
            if libc::bind(fd, (&from as *const libc::sockaddr_vm).cast(), size) != 0 {
                let error = io::Error::last_os_error();
                libc::close(fd);
                if error.raw_os_error() == Some(libc::EADDRINUSE) {
                    continue;
                }
                return Err(error);
            }
            let to = address(cid, port);
            if libc::connect(fd, (&to as *const libc::sockaddr_vm).cast(), size) != 0 {
                let error = io::Error::last_os_error();
                libc::close(fd);
                return Err(error);
            }
            return Ok(VsockStream::from_raw_fd(fd));
        }
    }
    Err(io::Error::from_raw_os_error(libc::EADDRINUSE))
}

/// One connection to [`OPEN_SOCKET`], on stdin: the person at its other end
/// is who the kernel says connected, never anyone the request names. Starts
/// their machine when it is off, waits for it to listen, and hands it the
/// link. The machine's unit binds itself to the person's own systemd, so it
/// only starts while they are signed in, and stops when they sign out.
fn serve_open() -> Result<()> {
    // SAFETY: systemd hands the accepted connection over as stdin.
    let mut person = unsafe { UnixStream::from_raw_fd(0) };
    let result = (|| -> Result<()> {
        let uid = peer_uid(person.as_raw_fd()).into_diagnostic()?;
        let cid = machine_cid(uid).ok_or_else(|| miette!("uid {uid} has no Agent machine"))?;
        let line = read_line(&mut person)?;
        let link = agent_link(&line).ok_or_else(|| miette!("not a link the Agent opens"))?;
        let status = Command::new("systemctl")
            .args(["start", &unit(uid)])
            .status()
            .into_diagnostic()?;
        if !status.success() {
            bail!("could not start {}", unit(uid));
        }
        let deadline = Instant::now() + BOOT_WAIT;
        let mut machine = loop {
            match connect_reserved(cid, LAUNCH_PORT) {
                Ok(machine) => break machine,
                Err(error) if Instant::now() < deadline => {
                    debug!(%error, cid, "not listening yet");
                    std::thread::sleep(Duration::from_secs(1));
                }
                Err(error) => {
                    return Err(error)
                        .into_diagnostic()
                        .wrap_err("the Agent's machine did not start in time")
                }
            }
        };
        info!(uid, cid, link, "opening the Agent");
        writeln!(machine, "{link}").into_diagnostic()?;
        answer(&read_line(&mut machine)?).map(drop)
    })();
    if let Err(report) = &result {
        warn!("{report:?}");
    }
    let line = reply(
        result
            .as_ref()
            .map(|()| Value::Null)
            .map_err(|r| r.to_string()),
    );
    writeln!(person, "{line}").into_diagnostic()
}

/// The person's end of their machine's tools channel, for as long as their
/// session runs: listens on [`tools_port`] and takes connections from their
/// own machine's address alone, which the kernel stamps and the machine
/// cannot choose. Each connection is one request, as on derisk's socket.
fn tools() -> Result<()> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let cid = machine_cid(uid).ok_or_else(|| miette!("uid {uid} has no Agent machine"))?;
    let listener = VsockListener::bind(&VsockAddr::new(VMADDR_CID_ANY, tools_port(cid)))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot listen on vsock port {}", tools_port(cid)))?;
    let session = runtime_dir()?.join("derisk");
    info!(cid, "serving the Agent's machine its tools");
    std::thread::scope(|scope| {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let session = &session;
                    scope.spawn(move || {
                        if let Err(e) = serve_tools(stream, cid, session) {
                            warn!("{e:?}");
                        }
                    });
                }
                Err(e) => warn!(error = %e, "cannot accept"),
            }
        }
    });
    Ok(())
}

fn serve_tools(mut machine: VsockStream, cid: u32, session: &std::path::Path) -> Result<()> {
    let peer = machine.peer_addr().into_diagnostic()?.cid();
    if peer != cid {
        info!(peer, "refused: not this person's Agent machine");
        return Ok(());
    }
    machine
        .set_read_timeout(Some(Duration::from_secs(30)))
        .into_diagnostic()?;
    let line = read_line(&mut machine)?;
    let call = classify(&line);
    // Every request the machine makes of the desktop is in the person's
    // journal, refused or not.
    if let Call::Desktop(request) = &call {
        let method: Value = serde_json::from_str(request).into_diagnostic()?;
        info!(
            method = method["method"].as_str(),
            "the Agent's machine used the desktop"
        );
    }
    let answer = match call {
        Call::Desktop(request) => {
            let socket = session.join("agent.sock");
            let mut desktop = UnixStream::connect(&socket)
                .into_diagnostic()
                .wrap_err_with(|| format!("no derisk session on {}", socket.display()))?;
            writeln!(desktop, "{request}").into_diagnostic()?;
            read_line(&mut desktop)?
        }
        Call::OpenUri(uri) => {
            info!(uri, "opening a link from the Agent's machine");
            let opened = Command::new("xdg-open")
                .arg(&uri)
                .stdin(Stdio::null())
                .spawn()
                .map(drop)
                .map_err(|e| format!("could not run xdg-open: {e}"));
            reply(opened.map(|()| Value::Null))
        }
        Call::Theme => {
            info!("the Agent's machine read the desktop's theme");
            reply(
                std::fs::read(session.join("theme.json"))
                    .map_err(|e| e.to_string())
                    .and_then(|theme| serde_json::from_slice(&theme).map_err(|e| e.to_string())),
            )
        }
        Call::Refused(why) => {
            info!(why, "refused a request from the Agent's machine");
            reply(Err(why))
        }
    };
    writeln!(machine, "{answer}").into_diagnostic()
}

/// One request on the tools channel, from the machine.
fn call_host(request: &Value) -> Result<Value> {
    let cid = vsock::get_local_cid().into_diagnostic()?;
    let mut host = VsockStream::connect(&VsockAddr::new(VMADDR_CID_HOST, tools_port(cid)))
        .into_diagnostic()
        .wrap_err("cannot reach the desktop's tools channel")?;
    writeln!(host, "{request}").into_diagnostic()?;
    answer(&read_line(&mut host)?)
}

/// One connection on [`LAUNCH_PORT`], on stdin: from the host's root, or
/// refused. Draws in the desktop's theme, then opens the Agent's window
/// with the link, or hands the link to the window already open. The window
/// runs in a unit of its own, not this connection's, which ends here.
fn launch() -> Result<()> {
    // SAFETY: systemd hands the accepted connection over as stdin.
    let mut host = unsafe { VsockStream::from_raw_fd(0) };
    let peer = host.peer_addr().into_diagnostic()?;
    if peer.cid() != VMADDR_CID_HOST || peer.port() >= 1024 {
        warn!(
            cid = peer.cid(),
            port = peer.port(),
            "refused: not the host's root"
        );
        return Ok(());
    }
    let result = (|| -> Result<()> {
        let line = read_line(&mut host)?;
        let link = agent_link(&line).ok_or_else(|| miette!("not a link the Agent opens"))?;
        match call_host(&json!({"method": "theme"})) {
            Ok(theme) => {
                let dir = runtime_dir()?.join("derisk");
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&dir)
                    .into_diagnostic()?;
                std::fs::write(dir.join("theme.json"), theme.to_string()).into_diagnostic()?;
            }
            Err(error) => info!(%error, "drawing in the Agent's own theme"),
        }
        let mut agent = Command::new("systemd-run");
        agent.args(["--user", "--collect", "--quiet", "--", "derisk-agent"]);
        if !link.is_empty() {
            agent.arg(link);
        }
        let status = agent.status().into_diagnostic()?;
        if !status.success() {
            bail!("could not start the Agent");
        }
        Ok(())
    })();
    let line = reply(
        result
            .as_ref()
            .map(|()| Value::Null)
            .map_err(|r| r.to_string()),
    );
    writeln!(host, "{line}").into_diagnostic()?;
    result
}

/// derisk's agent socket in the machine, `$XDG_RUNTIME_DIR/derisk/agent.sock`,
/// where `derisk mcp` and the Agent look for it: each connection is carried
/// to the host's tools channel, which decides what reaches the desktop.
fn relay() -> Result<()> {
    let dir = runtime_dir()?.join("derisk");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .into_diagnostic()?;
    let path = dir.join("agent.sock");
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e).into_diagnostic(),
        _ => {}
    }
    let listener = UnixListener::bind(&path).into_diagnostic()?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).into_diagnostic()?;
    let port = tools_port(vsock::get_local_cid().into_diagnostic()?);
    info!(port, "carrying derisk's agent socket to the desktop");
    std::thread::scope(|scope| {
        for stream in listener.incoming() {
            match stream {
                Ok(local) => {
                    scope.spawn(move || {
                        if let Err(e) = splice(local, port) {
                            warn!("{e:?}");
                        }
                    });
                }
                Err(e) => warn!(error = %e, "cannot accept"),
            }
        }
    });
    Ok(())
}

fn splice(local: UnixStream, port: u32) -> Result<()> {
    let host = VsockStream::connect(&VsockAddr::new(VMADDR_CID_HOST, port))
        .into_diagnostic()
        .wrap_err("cannot reach the desktop's tools channel")?;
    let (mut from_local, mut to_local) = (
        local.try_clone().into_diagnostic()?,
        local.try_clone().into_diagnostic()?,
    );
    let (mut from_host, mut to_host) = (host.try_clone().into_diagnostic()?, host);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = io::copy(&mut from_local, &mut to_host);
            let _ = to_host.shutdown(Shutdown::Write);
        });
        let _ = io::copy(&mut from_host, &mut to_local);
        let _ = local.shutdown(Shutdown::Both);
    });
    Ok(())
}

/// The machine's xdg-open: a web link opens in the person's browser on the
/// desktop. Nothing else does; a file here is the person's, in their Agent
/// folder, for them to open on the desktop.
fn open_uri(uri: &str) -> Result<()> {
    if !losos_agent_machine::is_web_url(uri) {
        bail!("the Agent's machine opens web links only, on the desktop: {uri}");
    }
    call_host(&json!({"method": "open_uri", "uri": uri})).map(drop)
}
