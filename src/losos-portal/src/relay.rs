//! `losos-portal relay`, on the host as root: the one way to the portal
//! machine, for the host's apps over a socket and for every machine's over
//! vsock.
//!
//! It knows who asks from the connection itself: a machine by the vsock
//! address libvirt gave it, the host's apps by their uid, which must be the
//! person whose session is in front. It starts the portal machine for that
//! person when it is not running, with their home and their drives shared
//! into it, and stops it once nothing has asked for a while. And it keeps
//! the only record of which files the person picked for which machine:
//! a machine may fetch those and write them back, and nothing else.

use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use miette::{Context, IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use vsock::{VsockListener, VMADDR_CID_ANY};

use crate::{
    connect_privileged, expect_frame, read_frame, read_json, receive_stream, response,
    result_uris, safe_name, send_stream, write_frame, write_json, Body, Person, Reply, Request,
    CLOSE, FILE_CHOOSER, PORTAL_CID, RELAY_PORT, RELAY_SOCKET, SCREENSHOT, SERVE_PORT,
};

/// Where the relay keeps what it must not forget across a restart: the
/// files each machine was given.
const GRANTS: &str = "/run/losos-portal/grants.json";
/// What the portal machine is told about the person it runs for.
const PERSON_DIR: &str = "/run/losos-portal/person";

pub struct Config {
    /// The portal machine's name, whose unit is
    /// `losos-service-machine-<name>.service`.
    pub machine: String,
    /// How long it runs with nothing asked of it before it is stopped.
    pub idle: Duration,
    /// How long it may take to boot, from being started to answering.
    pub boot: Duration,
}

/// Who is asking.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Caller {
    /// An app on the host, run by the person in front.
    Host,
    /// A machine, by its libvirt name.
    Machine(String),
    /// The portal machine itself, for the screen.
    Portal,
}

impl Caller {
    fn name(&self) -> &str {
        match self {
            Caller::Host => "host",
            Caller::Machine(name) => name,
            Caller::Portal => "portal",
        }
    }
}

/// What a machine may do with one file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Access {
    read: bool,
    write: bool,
}

/// Each machine's files, by host path.
type Grants = HashMap<String, HashMap<PathBuf, Access>>;

struct Relay {
    config: Config,
    grants: Mutex<Grants>,
    /// Requests the portal machine is answering now; it is never stopped
    /// under one.
    busy: AtomicUsize,
    last: Mutex<Instant>,
    /// One start at a time.
    starting: Mutex<()>,
}

/// Counts a request as running for as long as it lives.
struct Busy<'a>(&'a Relay);

impl<'a> Busy<'a> {
    fn new(relay: &'a Relay) -> Busy<'a> {
        relay.busy.fetch_add(1, Ordering::SeqCst);
        Busy(relay)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *self.0.last.lock().unwrap() = Instant::now();
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn run(config: Config) -> Result<()> {
    let grants = std::fs::read(GRANTS)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let relay = Relay {
        config,
        grants: Mutex::new(grants),
        busy: AtomicUsize::new(0),
        last: Mutex::new(Instant::now()),
        starting: Mutex::new(()),
    };

    let _ = std::fs::remove_file(RELAY_SOCKET);
    let local = UnixListener::bind(RELAY_SOCKET)
        .into_diagnostic()
        .wrap_err_with(|| format!("listening on {RELAY_SOCKET}"))?;
    // Any account may connect; who it is decides what it gets.
    std::fs::set_permissions(RELAY_SOCKET, std::fs::Permissions::from_mode(0o666))
        .into_diagnostic()?;
    let machines = VsockListener::bind_with_cid_port(VMADDR_CID_ANY, RELAY_PORT)
        .into_diagnostic()
        .wrap_err_with(|| format!("listening on vsock port {RELAY_PORT}"))?;
    info!(port = RELAY_PORT, socket = RELAY_SOCKET, "relaying portal requests");

    thread::scope(|scope| {
        let relay = &relay;
        scope.spawn(move || relay.idle());
        scope.spawn(move || {
            for stream in local.incoming() {
                let Ok(stream) = stream else { continue };
                scope.spawn(move || {
                    let caller = match peer_uid(&stream) {
                        Ok(uid) if active_person().is_some_and(|p| p.uid == uid) => Caller::Host,
                        Ok(uid) => {
                            warn!(uid, "refused: not the person in front");
                            return;
                        }
                        Err(e) => {
                            warn!(error = %e, "refused: no peer credentials");
                            return;
                        }
                    };
                    if let Err(e) = relay.one(caller, stream) {
                        debug!(error = %e, "host request");
                    }
                });
            }
        });
        for stream in machines.incoming() {
            let Ok(stream) = stream else { continue };
            scope.spawn(move || {
                let caller = match stream.peer_addr().map(|a| a.cid()) {
                    Ok(PORTAL_CID) => Caller::Portal,
                    Ok(cid) => match machine_at(cid) {
                        Some(name) if name != relay.config.machine => Caller::Machine(name),
                        _ => {
                            warn!(cid, "refused: no machine has this address");
                            return;
                        }
                    },
                    Err(e) => {
                        warn!(error = %e, "refused: no peer address");
                        return;
                    }
                };
                if let Err(e) = relay.one(caller, stream) {
                    debug!(error = %e, "machine request");
                }
            });
        }
    });
    Ok(())
}

/// A stream either listener gives.
trait Stream: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    fn shutdown(&self);
}

impl Stream for UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }
    fn shutdown(&self) {
        let _ = UnixStream::shutdown(self, std::net::Shutdown::Both);
    }
}

impl Stream for vsock::VsockStream {
    fn try_clone(&self) -> io::Result<Self> {
        vsock::VsockStream::try_clone(self)
    }
    fn shutdown(&self) {
        let _ = vsock::VsockStream::shutdown(self, std::net::Shutdown::Both);
    }
}

impl Relay {
    fn one(&self, caller: Caller, mut stream: impl Stream) -> io::Result<()> {
        let request: Request = read_json(&mut stream)?;
        let person = match active_person() {
            Some(person) => person,
            None => {
                return write_json(&mut stream, &Reply::failed("nobody is signed in"));
            }
        };
        match (&caller, request) {
            (
                Caller::Host | Caller::Machine(_),
                Request::Call {
                    interface,
                    method,
                    signature,
                    ..
                },
            ) => {
                let body = Body {
                    signature,
                    bytes: expect_frame(&mut stream)?,
                };
                self.call(&caller, &person, &interface, &method, body, stream)
            }
            (Caller::Machine(machine), Request::Fetch { path }) => {
                let path = PathBuf::from(path);
                if !self.allowed(machine, &path, |a| a.read) {
                    warn!(%machine, path = %path.display(), "fetch refused");
                    return write_json(&mut stream, &Reply::failed("not a file picked for this machine"));
                }
                self.fetch(&person, &["read".as_ref(), path.as_os_str()], &mut stream)
            }
            (Caller::Machine(machine), Request::Store { path }) => {
                let path = PathBuf::from(path);
                if !self.allowed(machine, &path, |a| a.write) {
                    warn!(%machine, path = %path.display(), "store refused");
                    // Read and drop what was sent, so the caller hears why.
                    receive_stream(&mut stream, &mut io::sink())?;
                    return write_json(&mut stream, &Reply::failed("not a file picked for this machine to write"));
                }
                info!(%machine, path = %path.display(), "written back");
                let reply = store(&person, &path, &mut stream);
                write_json(&mut stream, &reply)
            }
            (Caller::Machine(machine), Request::Upload { name }) => {
                let path = person
                    .home
                    .join(".cache/losos-portal/uploads")
                    .join(safe_name(machine))
                    .join(safe_name(&name));
                let mut reply = store(&person, &path, &mut stream);
                if reply.ok {
                    reply.path = Some(path.to_string_lossy().into_owned());
                }
                write_json(&mut stream, &reply)
            }
            (Caller::Portal, Request::Screenshot) => {
                self.fetch(&person, &["screenshot".as_ref()], &mut stream)
            }
            (caller, request) => {
                warn!(caller = caller.name(), ?request, "refused");
                write_json(&mut stream, &Reply::failed("not allowed from here"))
            }
        }
    }

    fn call(
        &self,
        caller: &Caller,
        person: &Person,
        interface: &str,
        method: &str,
        body: Body,
        mut stream: impl Stream,
    ) -> io::Result<()> {
        let _busy = Busy::new(self);
        let mut portal = match self.portal(person) {
            Ok(portal) => portal,
            Err(e) => {
                warn!(error = %e, "the portal machine is not answering");
                return write_json(
                    &mut stream,
                    &Reply::failed(format!("the portal machine is not answering: {e}")),
                );
            }
        };
        write_json(
            &mut portal,
            &Request::Call {
                interface: interface.to_owned(),
                method: method.to_owned(),
                signature: body.signature.clone(),
                caller: Some(caller.name().to_owned()),
            },
        )?;
        write_frame(&mut portal, &body.bytes)?;

        // The caller withdraws the request with `close`, or by going away.
        let mut from_caller = stream.try_clone()?;
        let mut to_portal = portal.try_clone()?;
        thread::spawn(move || {
            let frame = read_frame(&mut from_caller);
            if !matches!(frame, Ok(Some(ref f)) if f != CLOSE) {
                let _ = write_frame(&mut to_portal, CLOSE);
            }
        });

        let reply: Reply = read_json(&mut portal)?;
        if !reply.ok {
            return write_json(&mut stream, &reply);
        }
        let body = Body {
            signature: reply.signature.clone().unwrap_or_default(),
            bytes: expect_frame(&mut portal)?,
        };
        if let Caller::Machine(machine) = caller {
            self.grant(machine, interface, &body);
        }
        write_json(&mut stream, &reply)?;
        write_frame(&mut stream, &body.bytes)?;
        Stream::shutdown(&portal);
        Ok(())
    }

    /// Records the files a reply hands a machine.
    fn grant(&self, machine: &str, interface: &str, body: &Body) {
        let access = match interface {
            FILE_CHOOSER => Access {
                read: true,
                write: true,
            },
            SCREENSHOT => Access {
                read: true,
                write: false,
            },
            _ => return,
        };
        let Some((0, results)) = response(body) else {
            return;
        };
        let paths: Vec<PathBuf> = result_uris(&results)
            .iter()
            .filter_map(|uri| crate::path_from_uri(uri))
            .collect();
        if paths.is_empty() {
            return;
        }
        let mut grants = self.grants.lock().unwrap();
        let files = grants.entry(machine.to_owned()).or_default();
        for path in paths {
            info!(%machine, path = %path.display(), "picked for the machine");
            files.insert(path, access);
        }
        if let Ok(bytes) = serde_json::to_vec(&*grants) {
            let _ = std::fs::write(GRANTS, bytes);
        }
    }

    fn allowed(&self, machine: &str, path: &Path, check: impl Fn(&Access) -> bool) -> bool {
        self.grants
            .lock()
            .unwrap()
            .get(machine)
            .and_then(|files| files.get(path))
            .is_some_and(check)
    }

    /// Runs a helper as the person and passes on what it prints: its
    /// first line, the kind, and then its bytes.
    fn fetch(&self, person: &Person, args: &[&std::ffi::OsStr], stream: &mut impl Stream) -> io::Result<()> {
        let mut child = match as_person(person, args).stdout(Stdio::piped()).spawn() {
            Ok(child) => child,
            Err(e) => return write_json(stream, &Reply::failed(e)),
        };
        let mut out = io::BufReader::new(child.stdout.take().unwrap());
        let mut kind = String::new();
        out.read_line(&mut kind)?;
        let kind = kind.trim();
        if !matches!(kind, "file" | "dir") {
            let _ = child.wait();
            return write_json(stream, &Reply::failed("cannot read it"));
        }
        write_json(
            stream,
            &Reply {
                kind: Some(kind.to_owned()),
                ..Reply::ok()
            },
        )?;
        send_stream(stream, &mut out)?;
        let _ = child.wait();
        Ok(())
    }

    /// A connection to the portal machine, started for `person` first if it
    /// is not running for them.
    fn portal(&self, person: &Person) -> io::Result<vsock::VsockStream> {
        {
            let _one = self.starting.lock().unwrap();
            let unit = self.unit();
            let running = systemctl(&["is-active", "--quiet", &unit]);
            let ours = std::fs::read_to_string(Path::new(PERSON_DIR).join("person"))
                .is_ok_and(|line| line == person_line(person));
            if running && !ours {
                info!(person = %person.name, "the portal machine is someone else's; starting it again");
                systemctl(&["stop", &unit]);
            }
            if !running || !ours {
                self.prepare(person)?;
                info!(person = %person.name, "starting the portal machine");
                if !systemctl(&["start", &unit]) {
                    return Err(io::Error::other(format!("{unit} did not start")));
                }
            }
        }
        // It may still be booting: try until it answers.
        let deadline = Instant::now() + self.config.boot;
        loop {
            match connect_privileged(PORTAL_CID, SERVE_PORT) {
                Ok(stream) => return Ok(stream),
                Err(e) if Instant::now() < deadline => {
                    debug!(error = %e, "waiting for the portal machine");
                    thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn unit(&self) -> String {
        format!("losos-service-machine-{}.service", self.config.machine)
    }

    /// What the machine needs for `person`: who they are, and their home
    /// and drives shared into it at the same paths as here.
    fn prepare(&self, person: &Person) -> io::Result<()> {
        std::fs::create_dir_all(PERSON_DIR)?;
        std::fs::write(Path::new(PERSON_DIR).join("person"), person_line(person))?;
        let media = Path::new("/run/media").join(&person.name);
        if !media.exists() {
            std::fs::create_dir_all(&media)?;
            std::os::unix::fs::chown(&media, Some(person.uid), Some(person.gid))?;
            std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o750))?;
        }
        let devices = Path::new("/run/losos-service-machines")
            .join(&self.config.machine)
            .join("devices");
        std::fs::create_dir_all(&devices)?;
        std::fs::write(
            devices.join("portal-person.xml"),
            format!(
                "<filesystem type='mount' accessmode='squash'>\n  <source dir='{PERSON_DIR}'/>\n  <target dir='person'/>\n  <readonly/>\n</filesystem>\n"
            ),
        )?;
        for (tag, dir) in [("home", person.home.as_path()), ("media", media.as_path())] {
            std::fs::write(
                devices.join(format!("portal-{tag}.xml")),
                format!(
                    "<filesystem type='mount' accessmode='passthrough'>\n  <driver type='virtiofs'/>\n  <source dir='{}'/>\n  <target dir='{tag}'/>\n</filesystem>\n",
                    xml_escape(&dir.to_string_lossy())
                ),
            )?;
        }
        Ok(())
    }

    /// Stops the machine once it has had nothing to do for the idle time.
    fn idle(&self) {
        loop {
            thread::sleep(Duration::from_secs(15));
            if self.busy.load(Ordering::SeqCst) > 0
                || self.last.lock().unwrap().elapsed() < self.config.idle
            {
                continue;
            }
            let _one = self.starting.lock().unwrap();
            let unit = self.unit();
            if systemctl(&["is-active", "--quiet", &unit]) && self.busy.load(Ordering::SeqCst) == 0
            {
                info!("the portal machine is idle; stopping it");
                systemctl(&["stop", &unit]);
            }
        }
    }
}

/// Writes what the caller sends into `path` as the person.
fn store(person: &Person, path: &Path, stream: &mut impl Read) -> Reply {
    let child = as_person(person, &["write".as_ref(), path.as_os_str()])
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            let _ = receive_stream(stream, &mut io::sink());
            return Reply::failed(e);
        }
    };
    let sent = receive_stream(stream, child.stdin.as_mut().unwrap());
    drop(child.stdin.take());
    match (sent, child.wait()) {
        (Ok(_), Ok(status)) if status.success() => Reply::ok(),
        (Err(e), _) => Reply::failed(e),
        _ => Reply::failed(format!("cannot write {}", path.display())),
    }
}

/// This program, run as `person` with nothing of root's: their uid, gid
/// and groups, and an environment of their own.
fn as_person(person: &Person, args: &[&std::ffi::OsStr]) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap_or_else(|_| "losos-portal".into()));
    command
        .args(args)
        .env_clear()
        .env("HOME", &person.home)
        .env("USER", &person.name)
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", person.uid))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir("/");
    let (uid, gid, groups) = (person.uid, person.gid, person.groups());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

/// The person file's one line: `name:uid:gid:gecos:home`.
fn person_line(person: &Person) -> String {
    format!(
        "{}:{}:{}:{}:{}\n",
        person.name,
        person.uid,
        person.gid,
        person.gecos.replace([':', '\n'], " "),
        person.home.display()
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .args(args)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The person whose session is in front, on seat0.
pub fn active_person() -> Option<Person> {
    let output = |args: &[&str]| -> Option<String> {
        let out = Command::new("loginctl").args(args).output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (out.status.success() && !text.is_empty()).then_some(text)
    };
    let session = output(&["show-seat", "seat0", "-p", "ActiveSession", "--value"])?;
    let name = output(&["show-session", &session, "-p", "Name", "--value"])?;
    Person::named(&name)
}

/// The libvirt machine whose vsock address is `cid`.
fn machine_at(cid: u32) -> Option<String> {
    let names = Command::new("virsh").args(["-q", "list", "--name"]).output().ok()?;
    let wanted = format!("address='{cid}'");
    let wanted_dq = format!("address=\"{cid}\"");
    String::from_utf8_lossy(&names.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .find(|name| {
            Command::new("virsh")
                .args(["-q", "dumpxml", name])
                .output()
                .is_ok_and(|xml| {
                    String::from_utf8_lossy(&xml.stdout)
                        .lines()
                        .any(|line| line.contains("<cid ") && (line.contains(&wanted) || line.contains(&wanted_dq)))
                })
        })
        .map(str::to_owned)
}

fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
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
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zvariant::{OwnedValue, Value};

    fn relay() -> Relay {
        Relay {
            config: Config {
                machine: "sys-portal".into(),
                idle: Duration::from_secs(60),
                boot: Duration::from_secs(1),
            },
            grants: Mutex::new(Grants::new()),
            busy: AtomicUsize::new(0),
            last: Mutex::new(Instant::now()),
            starting: Mutex::new(()),
        }
    }

    fn picked(uris: &[&str]) -> Body {
        let mut results = HashMap::new();
        results.insert(
            "uris".to_owned(),
            OwnedValue::try_from(Value::from(uris.to_vec())).unwrap(),
        );
        crate::response_body(0, results).unwrap()
    }

    #[test]
    fn a_machine_gets_only_what_was_picked_for_it() {
        let relay = relay();
        relay.grant("work", FILE_CHOOSER, &picked(&["file:///home/a/Doc.odt"]));
        let doc = Path::new("/home/a/Doc.odt");
        assert!(relay.allowed("work", doc, |a| a.read && a.write));
        assert!(!relay.allowed("flatpak", doc, |a| a.read));
        assert!(!relay.allowed("work", Path::new("/home/a/.ssh/id_ed25519"), |a| a.read));
    }

    #[test]
    fn a_screenshot_is_only_read() {
        let relay = relay();
        let mut results = HashMap::new();
        results.insert(
            "uri".to_owned(),
            OwnedValue::try_from(Value::from("file:///home/a/Pictures/s.png")).unwrap(),
        );
        relay.grant("work", SCREENSHOT, &crate::response_body(0, results).unwrap());
        let shot = Path::new("/home/a/Pictures/s.png");
        assert!(relay.allowed("work", shot, |a| a.read));
        assert!(!relay.allowed("work", shot, |a| a.write));
    }

    #[test]
    fn a_cancelled_dialog_grants_nothing() {
        let relay = relay();
        let mut results = HashMap::new();
        results.insert(
            "uris".to_owned(),
            OwnedValue::try_from(Value::from(vec!["file:///home/a/x"])).unwrap(),
        );
        relay.grant("work", FILE_CHOOSER, &crate::response_body(1, results).unwrap());
        assert!(!relay.allowed("work", Path::new("/home/a/x"), |a| a.read));
    }

    #[test]
    fn the_person_line_has_no_stray_separators() {
        let person = Person {
            name: "alice".into(),
            uid: 1000,
            gid: 100,
            home: "/home/alice".into(),
            gecos: "Alice: B\nC".into(),
        };
        assert_eq!(person_line(&person), "alice:1000:100:Alice  B C:/home/alice\n");
    }
}
