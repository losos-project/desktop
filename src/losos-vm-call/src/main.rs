//! Runs a command in one of the OS's own machines as if it ran on the host.
//!
//!     losos-vm-call serve <port> <command> [<arg>...]
//!                                     in a machine: run <command> for each
//!                                     call from the host on vsock <port>
//!     losos-vm-call relay <config>    on the host: let the calls <config>
//!                                     lists through, each to whom it allows
//!     losos-vm-call call <name> [<arg>...]
//!                                     on the host: make a call, as if
//!                                     <name>'s command ran here
//!
//! The OS keeps parts of itself in machines of their own (docs/service-machines.md):
//! the network's hardware and NetworkManager in the network machine, for
//! one. The host's programs still ask for those parts by their usual
//! commands, so `nmcli` on the host is `losos-vm-call call nmcli`, and
//! derisk's Network pages, Danube's captive portal watch and a person typing
//! `nmcli` in a terminal reach the NetworkManager that has the Wi-Fi card
//! without knowing it lives elsewhere. The command's arguments, standard
//! input and output, and exit status go across; nothing else does.
//!
//! A machine takes calls only from the host and only from a vsock port
//! below 1024, which only root on the host can call from. The relay is that
//! root: it listens on a socket in /run/losos-vm-call for each call, asks
//! the kernel who connected, and lets through the people the call allows.

use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Mutex;

use crossbeam_channel::bounded;
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use losos_vm_call::{
    allowed, decode_args, encode_args, exit_code, parse_calls, read_frame, write_frame, Call,
    ARGS, EXIT, HOST_CID, PRIVILEGED_PORTS, SOCKET_DIR, STDERR, STDIN, STDIN_END, STDOUT,
};

/// Where logind keeps a record of each signed-in user, by uid.
const LOGIND_USERS: &str = "/run/systemd/users";

fn main() -> ExitCode {
    // stderr, which is the journal for a service. A caller's stderr is the
    // command's, so `call` logs nothing unless asked.
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
        Some("call") if args.len() >= 2 => match call(&args[1], &args[2..]) {
            Ok(code) => return ExitCode::from(u8::try_from(code).unwrap_or(1)),
            Err(e) => Err(e),
        },
        Some("serve") if args.len() >= 3 => args[1]
            .parse::<u32>()
            .into_diagnostic()
            .wrap_err("the port is a number")
            .and_then(|port| serve(port, &args[2], &args[3..])),
        Some("relay") if args.len() == 2 => relay(Path::new(&args[1])),
        _ => Err(miette!(
            "usage: losos-vm-call serve <port> <command> [<arg>...]\n       \
             losos-vm-call relay <config>\n       \
             losos-vm-call call <name> [<arg>...]"
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

/// Makes the call `name` through the host's relay and passes the command's
/// output on as it comes. Returns the command's exit status.
fn call(name: &str, args: &[String]) -> Result<i32> {
    let path = Path::new(SOCKET_DIR).join(format!("{name}.sock"));
    let socket = UnixStream::connect(&path)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot reach {} (is the machine running?)", path.display()))?;
    let mut to_relay = socket.try_clone().into_diagnostic()?;
    write_frame(&mut to_relay, ARGS, &encode_args(args)).into_diagnostic()?;

    // A terminal is a person, not input: the commands called this way read
    // input from a pipe (nmcli's passwd-file /dev/stdin), and forwarding a
    // terminal would take keystrokes the shell is owed after the command
    // ends. So a terminal's input ends at once.
    let stdin = io::stdin();
    let forward_stdin = !stdin.is_terminal();
    std::thread::spawn(move || {
        if forward_stdin {
            let mut stdin = stdin.lock();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if write_frame(&mut to_relay, STDIN, &buf[..n]).is_err() {
                            return;
                        }
                    }
                }
            }
        }
        let _ = write_frame(&mut to_relay, STDIN_END, &[]);
    });

    let mut from_relay = socket;
    let (mut stdout, mut stderr) = (io::stdout().lock(), io::stderr().lock());
    loop {
        let Some((tag, payload)) = read_frame(&mut from_relay).into_diagnostic()? else {
            bail!("the machine hung up before {name} finished");
        };
        match tag {
            STDOUT => {
                // A reader that went away (`nmcli ... | head`) ends the call,
                // as it would end the command here.
                if stdout.write_all(&payload).and_then(|()| stdout.flush()).is_err() {
                    return Ok(141);
                }
            }
            STDERR => {
                let _ = stderr.write_all(&payload);
                let _ = stderr.flush();
            }
            EXIT => {
                let code = <[u8; 4]>::try_from(payload.as_slice())
                    .map(i32::from_be_bytes)
                    .map_err(|_| miette!("a malformed exit status"))?;
                return Ok(code);
            }
            other => bail!("an unknown frame {other:#x} from the machine"),
        }
    }
}

/// Runs `command` with `fixed` and then each caller's arguments, for every
/// call from root on the host to vsock `port`.
fn serve(port: u32, command: &str, fixed: &[String]) -> Result<()> {
    let listener =
        vsock::VsockListener::bind(&vsock::VsockAddr::new(vsock::VMADDR_CID_ANY, port))
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot listen on vsock port {port}"))?;
    info!(port, command, "taking calls from the host");
    std::thread::scope(|scope| {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    warn!(error = %e, "cannot accept");
                    continue;
                }
            };
            let peer = match stream.peer_addr() {
                Ok(peer) => peer,
                Err(e) => {
                    warn!(error = %e, "no peer address");
                    continue;
                }
            };
            // The kernel stamps both on the connection: a machine beside this
            // one, or a host process that is not root, cannot pass.
            if peer.cid() != HOST_CID || !PRIVILEGED_PORTS.contains(&peer.port()) {
                warn!(cid = peer.cid(), port = peer.port(), "refused");
                continue;
            }
            scope.spawn(move || {
                if let Err(e) = run_one(stream, command, fixed) {
                    warn!("{e:?}");
                }
            });
        }
    });
    Ok(())
}

/// One call: its arguments, then its input in, its output out, its status.
fn run_one(stream: vsock::VsockStream, command: &str, fixed: &[String]) -> Result<()> {
    let mut from_host = stream.try_clone().into_diagnostic()?;
    let closer = stream.try_clone().into_diagnostic()?;
    let args = match read_frame(&mut from_host).into_diagnostic()? {
        Some((ARGS, payload)) => decode_args(&payload).into_diagnostic()?,
        _ => bail!("a call that did not start with its arguments"),
    };
    debug!(command, ?args, "call");
    let mut child = Command::new(command)
        .args(fixed)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot run {command}"))?;
    let mut stdin = child.stdin.take();
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let child = Mutex::new(child);

    // One writer, so frames from the two pipes never interleave inside each
    // other. Bounded: a host that stops reading stalls the command's output,
    // as a full pipe would.
    let (frames, queue) = bounded::<(u8, Vec<u8>)>(16);
    std::thread::scope(|scope| {
        let mut to_host = stream;
        let writer = scope.spawn(move || {
            for (tag, payload) in queue {
                if write_frame(&mut to_host, tag, &payload).is_err() {
                    break;
                }
            }
        });
        let pump = |pipe: &mut dyn Read, tag: u8, frames: crossbeam_channel::Sender<_>| {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if frames.send((tag, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        };
        let (out_frames, err_frames) = (frames.clone(), frames.clone());
        let outputs = [
            scope.spawn(move || pump(&mut out, STDOUT, out_frames)),
            scope.spawn(move || pump(&mut err, STDERR, err_frames)),
        ];

        // The caller's input, and after it the caller hanging up: a call
        // whose caller went away (Ctrl-C on `nmcli monitor`) stops its
        // command rather than running on with nobody to read it.
        let child = &child;
        scope.spawn(move || {
            loop {
                match read_frame(&mut from_host) {
                    Ok(Some((STDIN, data))) => {
                        if let Some(pipe) = stdin.as_mut() {
                            if pipe.write_all(&data).is_err() {
                                stdin = None;
                            }
                        }
                    }
                    Ok(Some((STDIN_END, _))) => stdin = None,
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        stop(&mut child.lock().unwrap());
                        break;
                    }
                }
            }
        });

        for output in outputs {
            let _ = output.join();
        }
        let status = child.lock().unwrap().wait();
        let code = status.map(exit_code).unwrap_or(1);
        let _ = frames.send((EXIT, code.to_be_bytes().to_vec()));
        drop(frames);
        let _ = writer.join();
        // Ends the input thread's read, whatever the host does next.
        let _ = closer.shutdown(std::net::Shutdown::Both);
    });
    Ok(())
}

/// Stops a call's command if it still runs.
fn stop(child: &mut Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
    }
}

/// Lets each call in `config` through from its socket to its machine.
fn relay(config: &Path) -> Result<()> {
    let text = std::fs::read_to_string(config)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot read {}", config.display()))?;
    let calls = parse_calls(&text).map_err(|e| miette!("{}: {e}", config.display()))?;
    std::fs::create_dir_all(SOCKET_DIR).into_diagnostic()?;
    let listeners = calls
        .into_iter()
        .map(|call| {
            let path = PathBuf::from(SOCKET_DIR).join(format!("{}.sock", call.name));
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot listen on {}", path.display()))?;
            // Anyone may connect; who gets through is decided per caller.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
                .into_diagnostic()?;
            info!(name = call.name, cid = call.cid, port = call.port, "relaying");
            Ok((call, listener))
        })
        .collect::<Result<Vec<_>>>()?;
    std::thread::scope(|scope| {
        for (call, listener) in &listeners {
            scope.spawn(move || {
                std::thread::scope(|connections| {
                    for stream in listener.incoming() {
                        match stream {
                            Ok(stream) => {
                                connections.spawn(move || {
                                    if let Err(e) = relay_one(call, stream) {
                                        warn!(name = call.name, "{e:?}");
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

/// One call through the relay: who is asking, then the bytes both ways.
fn relay_one(call: &Call, mut caller: UnixStream) -> Result<()> {
    let (uid, gid) = peer_credentials(&caller)?;
    let groups = group_names(uid, gid);
    if !allowed(call, uid, &groups, Path::new(LOGIND_USERS)) {
        info!(name = call.name, uid, "refused");
        let _ = write_frame(
            &mut caller,
            STDERR,
            format!(
                "{}: only an administrator, or someone signed in at this computer, may do this\n",
                call.name
            )
            .as_bytes(),
        );
        let _ = write_frame(&mut caller, EXIT, &1i32.to_be_bytes());
        return Ok(());
    }
    debug!(name = call.name, uid, "relaying a call");
    let machine = match connect_privileged(call.cid, call.port) {
        Ok(machine) => machine,
        Err(e) => {
            let _ = write_frame(
                &mut caller,
                STDERR,
                format!("{}: the machine that does this is not running ({e})\n", call.name)
                    .as_bytes(),
            );
            let _ = write_frame(&mut caller, EXIT, &1i32.to_be_bytes());
            return Ok(());
        }
    };
    let (mut from_machine, mut to_machine) = (
        machine.try_clone().into_diagnostic()?,
        machine.try_clone().into_diagnostic()?,
    );
    let (mut from_caller, mut to_caller) = (caller.try_clone().into_diagnostic()?, caller);
    // Either side hanging up ends both, so a caller that goes away stops
    // the command in the machine.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = io::copy(&mut from_caller, &mut to_machine);
            let _ = to_machine.shutdown(std::net::Shutdown::Both);
        });
        let _ = io::copy(&mut from_machine, &mut to_caller);
        let _ = to_caller.shutdown(std::net::Shutdown::Both);
        let _ = from_machine.shutdown(std::net::Shutdown::Both);
    });
    Ok(())
}

/// The uid and gid of the process at the other end of `stream`.
fn peer_credentials(stream: &UnixStream) -> Result<(u32, u32)> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid for writes of the sizes given.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error())
            .into_diagnostic()
            .wrap_err("cannot ask who connected");
    }
    Ok((cred.uid, cred.gid))
}

/// The names of every group `uid` is in, through NSS, so systemd-homed's
/// people are found as well as /etc/group's. Empty where the user is
/// unknown, which allows nothing but an active session.
fn group_names(uid: u32, gid: u32) -> Vec<String> {
    use std::ffi::CStr;
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to a live local of the size given.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() {
        return Vec::new();
    }
    let mut count: libc::c_int = 256;
    let mut gids = vec![0 as libc::gid_t; count as usize];
    // SAFETY: pw_name is NUL-ended inside buf; gids holds count entries.
    let rc = unsafe { libc::getgrouplist(pwd.pw_name, gid, gids.as_mut_ptr(), &mut count) };
    if rc < 0 {
        return Vec::new();
    }
    gids.truncate(count as usize);
    gids.into_iter()
        .filter_map(|gid| {
            let mut buf = vec![0 as libc::c_char; 16 * 1024];
            let mut grp: libc::group = unsafe { std::mem::zeroed() };
            let mut found: *mut libc::group = std::ptr::null_mut();
            // SAFETY: as above.
            let rc =
                unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut found) };
            (rc == 0 && !found.is_null()).then(|| {
                // SAFETY: gr_name is NUL-ended inside buf, alive here.
                unsafe { CStr::from_ptr(grp.gr_name) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

/// A vsock connection to `cid`:`port` from a port below 1024, which tells
/// the machine root made it. Tries each such port until one is free.
fn connect_privileged(cid: u32, port: u32) -> io::Result<vsock::VsockStream> {
    let mut last = io::Error::from(io::ErrorKind::AddrInUse);
    for local in PRIVILEGED_PORTS.rev() {
        // SAFETY: a plain socket call; the fd is owned at once.
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh socket nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let addr = |cid: u32, port: u32| {
            let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
            addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
            addr.svm_cid = cid;
            addr.svm_port = port;
            addr
        };
        use std::os::fd::AsRawFd;
        let len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
        let mine = addr(libc::VMADDR_CID_ANY, local);
        // SAFETY: mine is a valid sockaddr_vm of len bytes.
        if unsafe { libc::bind(fd.as_raw_fd(), (&mine as *const libc::sockaddr_vm).cast(), len) }
            != 0
        {
            last = io::Error::last_os_error();
            if last.kind() == io::ErrorKind::AddrInUse {
                continue;
            }
            return Err(last);
        }
        let theirs = addr(cid, port);
        // SAFETY: as above.
        if unsafe {
            libc::connect(fd.as_raw_fd(), (&theirs as *const libc::sockaddr_vm).cast(), len)
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a connected vsock stream socket, handed over whole.
        return Ok(unsafe { vsock::VsockStream::from_raw_fd(std::os::fd::IntoRawFd::into_raw_fd(fd)) });
    }
    Err(last)
}
