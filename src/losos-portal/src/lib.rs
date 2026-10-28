//! The portal machine's wire format and what its pieces share
//! (docs/portal-machine.md).
//!
//! The dialogs of the desktop's portals (picking a file, saying yes to an
//! app, taking a screenshot, setting the wallpaper) run in sys-portal, a
//! machine of their own. Everywhere an app asks for one, on the host and in
//! every machine, xdg-desktop-portal's backend for those portals is
//! `losos-portal forward`, which hands each request to the host's relay.
//! The relay knows who asks from where the connection came in, starts the
//! portal machine when it is not running, and passes the request on to
//! `losos-portal serve` there, which asks the real backend (GTK's or
//! derisk's) on the machine's session bus. Files go the other way: a file
//! picked for an app in a machine is copied into that machine by the relay,
//! which lets a machine read and write only the files the person picked for
//! it.
//!
//! Every connection carries frames: a big-endian `u32` length and that many
//! bytes. The first is a JSON [`Request`]; what follows depends on it.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

use serde::{Deserialize, Serialize};

pub mod agent;
pub mod files;
pub mod forward;
pub mod relay;
pub mod serve;
use zvariant::{OwnedValue, Structure, Value};

/// The relay's vsock port on the host, where machines reach it.
pub const RELAY_PORT: u32 = 7360;
/// Where the host's own apps reach the relay.
pub const RELAY_SOCKET: &str = "/run/losos-portal/relay.sock";
/// `losos-portal serve`'s vsock port in the portal machine.
pub const SERVE_PORT: u32 = 7361;
/// The portal machine's vsock address (modules/portal-machine.nix).
pub const PORTAL_CID: u32 = 4104;
/// The host's vsock address.
pub const HOST_CID: u32 = 2;
/// Ports only root can bind: the portal machine takes requests only from
/// one, so only the relay can ask it for anything.
pub const PRIVILEGED_PORTS: std::ops::RangeInclusive<u32> = 512..=1023;

/// The backend's name on a session bus, from `losos.portal`.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.losos";
/// Where every portal backend's interfaces live.
pub const PATH: &str = "/org/freedesktop/portal/desktop";
/// Each request's object, which the frontend can close.
pub const REQUEST: &str = "org.freedesktop.impl.portal.Request";

/// The portals whose dialogs are the portal machine's, and the backend there
/// that answers each. The rest (the theme, which apps run in the
/// background, screen sharing, printing, opening a file in an app) have no
/// dialog of their own or need the host, and stay with the host's or the
/// machine's own backends.
pub const PORTALS: &[(&str, &str)] = &[
    (
        "org.freedesktop.impl.portal.FileChooser",
        "org.freedesktop.impl.portal.desktop.gtk",
    ),
    (
        "org.freedesktop.impl.portal.Access",
        "org.freedesktop.impl.portal.desktop.gtk",
    ),
    (
        "org.freedesktop.impl.portal.Account",
        "org.freedesktop.impl.portal.desktop.gtk",
    ),
    (
        "org.freedesktop.impl.portal.Screenshot",
        "org.freedesktop.impl.portal.desktop.derisk",
    ),
    (
        "org.freedesktop.impl.portal.Wallpaper",
        "org.freedesktop.impl.portal.desktop.derisk",
    ),
];

/// The backend in the portal machine for `interface`, if it is one of
/// [`PORTALS`].
pub fn backend(interface: &str) -> Option<&'static str> {
    PORTALS
        .iter()
        .find(|(i, _)| *i == interface)
        .map(|(_, backend)| *backend)
}

pub const FILE_CHOOSER: &str = "org.freedesktop.impl.portal.FileChooser";
pub const SCREENSHOT: &str = "org.freedesktop.impl.portal.Screenshot";
pub const WALLPAPER: &str = "org.freedesktop.impl.portal.Wallpaper";

/// The first frame of a connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// A portal method: a frame with its body, in D-Bus's little-endian
    /// encoding, follows. While it runs the caller may send a `"close"`
    /// frame, which withdraws it; then a [`Reply`] comes back, and on
    /// success a frame with the reply's body. `caller` is the relay's, on
    /// its way to the portal machine: the machine that asked, or `host`.
    Call {
        interface: String,
        method: String,
        signature: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller: Option<String>,
    },
    /// A file or folder the person picked for this machine, from the host:
    /// a [`Reply`] whose `kind` is `file` or `dir`, then its bytes, a file
    /// as it is and a folder as a tar stream, in frames ended by an empty
    /// one.
    Fetch { path: String },
    /// A file the person picked for this machine to write, written back
    /// whole: its bytes follow in frames ended by an empty one, then a
    /// [`Reply`].
    Store { path: String },
    /// A picture for the Wallpaper portal, which reads it on the host:
    /// its bytes follow as for `store`, and the [`Reply`]'s `path` is where
    /// it landed.
    Upload { name: String },
    /// From the portal machine alone: the screen as the session sees it, a
    /// PNG, as for `fetch`.
    Screenshot,
}

/// The answer to a [`Request`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    /// A D-Bus error's name, when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The reply body's signature, after a call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// `file` or `dir`, before a fetch's bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Where an upload landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Reply {
    pub fn ok() -> Reply {
        Reply {
            ok: true,
            ..Reply::default()
        }
    }

    pub fn failed(message: impl std::fmt::Display) -> Reply {
        Reply::error("org.freedesktop.DBus.Error.Failed", message)
    }

    pub fn error(name: &str, message: impl std::fmt::Display) -> Reply {
        Reply {
            ok: false,
            error: Some(name.to_owned()),
            message: Some(message.to_string()),
            ..Reply::default()
        }
    }
}

/// A withdrawn request, sent while a call runs.
pub const CLOSE: &[u8] = b"{\"op\":\"close\"}";

/// The most one frame carries: a D-Bus message's largest body.
pub const MAX_FRAME: usize = 128 << 20;

/// Writes one frame.
pub fn write_frame(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    out.write_all(&(payload.len() as u32).to_be_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

/// Reads one frame, or `None` where the stream ended between frames.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len[..1]) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    input.read_exact(&mut len[1..])?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes"),
        ));
    }
    let mut payload = vec![0u8; len];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// Reads a frame that must be there.
pub fn expect_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    read_frame(input)?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
}

pub fn write_json(out: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    write_frame(out, &serde_json::to_vec(value).map_err(io::Error::other)?)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(input: &mut impl Read) -> io::Result<T> {
    serde_json::from_slice(&expect_frame(input)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Copies `from` into frames ended by an empty one.
pub fn send_stream(out: &mut impl Write, from: &mut impl Read) -> io::Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        write_frame(out, &buf[..n])?;
        total += n as u64;
    }
    write_frame(out, &[])?;
    Ok(total)
}

/// Copies frames up to the empty one into `to`.
pub fn receive_stream(input: &mut impl Read, to: &mut impl Write) -> io::Result<u64> {
    let mut total = 0;
    loop {
        let frame = expect_frame(input)?;
        if frame.is_empty() {
            to.flush()?;
            return Ok(total);
        }
        to.write_all(&frame)?;
        total += frame.len() as u64;
    }
}

/// A message body, the way it travels: its signature, without the
/// parentheses round the whole, and its bytes in D-Bus's little-endian
/// encoding.
#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    pub signature: String,
    pub bytes: Vec<u8>,
}

fn context() -> zvariant::serialized::Context {
    zvariant::serialized::Context::new_dbus(zvariant::LE, 0)
}

impl Body {
    /// A message's body, in whatever byte order it came.
    pub fn of(message: &zbus::Message) -> zbus::Result<Body> {
        let body = message.body();
        if body.is_empty() {
            return Ok(Body::empty());
        }
        let fields: Structure = body.deserialize()?;
        Body::from_fields(fields.into_fields())
    }

    pub fn empty() -> Body {
        Body {
            signature: String::new(),
            bytes: Vec::new(),
        }
    }

    /// The body holding `fields`, in order.
    pub fn from_fields(fields: Vec<Value<'_>>) -> zbus::Result<Body> {
        if fields.is_empty() {
            return Ok(Body::empty());
        }
        let mut builder = zvariant::StructureBuilder::new();
        for field in fields {
            builder = builder.append_field(field);
        }
        let structure = builder.build()?;
        let signature = structure.signature().to_string_no_parens();
        let bytes = zvariant::to_bytes(context(), &structure)?.to_vec();
        Ok(Body { signature, bytes })
    }

    /// Its fields, in order.
    pub fn fields(&self) -> zbus::Result<Vec<OwnedValue>> {
        if self.signature.is_empty() {
            return Ok(Vec::new());
        }
        let signature = zvariant::Signature::try_from(format!("({})", self.signature).as_str())?;
        let data = zvariant::serialized::Data::new(self.bytes.as_slice(), context());
        let (structure, _): (Structure, _) = data.deserialize_for_dynamic_signature(&signature)?;
        structure
            .into_fields()
            .into_iter()
            .map(|v| OwnedValue::try_from(v).map_err(zbus::Error::from))
            .collect()
    }

    /// Its fields as one structure, to send as a message's body; not for
    /// an empty body, which is `()`.
    pub fn structure(&self) -> zbus::Result<Structure<'static>> {
        let mut structure = zvariant::StructureBuilder::new();
        for field in self.fields()? {
            structure = structure.append_field(Value::from(field));
        }
        Ok(structure.build()?)
    }
}

/// A portal reply's `(u response, a{sv} results)`.
pub fn response(body: &Body) -> Option<(u32, HashMap<String, OwnedValue>)> {
    let fields = body.fields().ok()?;
    let [code, results] = <[OwnedValue; 2]>::try_from(fields).ok()?;
    let code = u32::try_from(&code).ok()?;
    let results: HashMap<String, OwnedValue> = HashMap::try_from(results).ok()?;
    Some((code, results))
}

/// The body of a portal reply.
pub fn response_body(code: u32, results: HashMap<String, OwnedValue>) -> zbus::Result<Body> {
    let results: HashMap<String, Value> = results
        .into_iter()
        .map(|(k, v)| (k, Value::from(v)))
        .collect();
    Body::from_fields(vec![Value::from(code), Value::from(results)])
}

/// The files a portal reply hands the app: `uris` from the file chooser,
/// `uri` from a screenshot.
pub fn result_uris(results: &HashMap<String, OwnedValue>) -> Vec<String> {
    let owned = |key: &str| results.get(key).and_then(|v| v.try_clone().ok());
    let mut uris: Vec<String> = owned("uris")
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .unwrap_or_default();
    uris.extend(owned("uri").and_then(|v| String::try_from(v).ok()));
    uris
}

/// The path a `file://` URI names, decoded; nothing for any other scheme,
/// another host, or a path that is not absolute or climbs with `..`.
pub fn path_from_uri(uri: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    if decoded.contains(&0) {
        return None;
    }
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(decoded));
    (!path
        .components()
        .any(|c| c == std::path::Component::ParentDir))
    .then_some(path)
}

/// A `file://` URI for `path`, escaping everything but RFC 3986's
/// unreserved characters and `/`.
pub fn uri_from_path(path: &std::path::Path) -> String {
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// A file name from somewhere else, made safe to put in a folder: its last
/// part, without NULs or slashes, never empty, `.` or `..`.
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit('/').next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| *c != '\0').collect();
    match cleaned.as_str() {
        "" | "." | ".." => "file".to_owned(),
        _ => cleaned,
    }
}

/// A vsock connection to `cid`:`port` from a port below 1024, which tells
/// the other end root made it. Tries each such port until one is free.
pub fn connect_privileged(cid: u32, port: u32) -> io::Result<vsock::VsockStream> {
    use std::os::fd::AsRawFd;
    let address = |cid: u32, port: u32| {
        // SAFETY: sockaddr_vm is plain data; zero is valid for every field.
        let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        address.svm_cid = cid;
        address.svm_port = port;
        address
    };
    let len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
    let mut last = io::Error::from(io::ErrorKind::AddrInUse);
    for local in PRIVILEGED_PORTS.rev() {
        // SAFETY: a plain socket call; the fd is owned at once.
        let fd =
            unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh socket nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mine = address(libc::VMADDR_CID_ANY, local);
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
        let theirs = address(cid, port);
        // SAFETY: as above.
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&theirs as *const libc::sockaddr_vm).cast(),
                len,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a connected vsock stream socket, handed over whole.
        return Ok(unsafe { vsock::VsockStream::from_raw_fd(fd.into_raw_fd()) });
    }
    Err(last)
}

/// A person on the host: who the relay reads and writes files as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: std::path::PathBuf,
    pub gecos: String,
}

impl Person {
    /// The account called `name`, through NSS.
    pub fn named(name: &str) -> Option<Person> {
        let name = std::ffi::CString::new(name).ok()?;
        let mut buf = vec![0 as libc::c_char; 16 * 1024];
        // SAFETY: passwd is plain data; zero is valid for every field.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is to a live local of the size given.
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut found,
            )
        };
        if rc != 0 || found.is_null() {
            return None;
        }
        let text = |p: *const libc::c_char| {
            if p.is_null() {
                String::new()
            } else {
                // SAFETY: NUL-ended inside buf, alive here.
                unsafe { std::ffi::CStr::from_ptr(p) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        Some(Person {
            name: text(pwd.pw_name),
            uid: pwd.pw_uid,
            gid: pwd.pw_gid,
            home: text(pwd.pw_dir).into(),
            gecos: text(pwd.pw_gecos),
        })
    }

    /// Every group the person is in, for running something as them.
    pub fn groups(&self) -> Vec<libc::gid_t> {
        let Ok(name) = std::ffi::CString::new(self.name.as_str()) else {
            return vec![self.gid];
        };
        let mut count: libc::c_int = 256;
        let mut gids = vec![0 as libc::gid_t; count as usize];
        // SAFETY: name is NUL-ended; gids holds count entries.
        let rc =
            unsafe { libc::getgrouplist(name.as_ptr(), self.gid, gids.as_mut_ptr(), &mut count) };
        if rc < 0 {
            return vec![self.gid];
        }
        gids.truncate(count as usize);
        gids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_json(
            &mut wire,
            &Request::Call {
                interface: FILE_CHOOSER.into(),
                method: "OpenFile".into(),
                signature: "osssa{sv}".into(),
                caller: None,
            },
        )
        .unwrap();
        send_stream(&mut wire, &mut &b"hello"[..]).unwrap();
        let mut input = wire.as_slice();
        let request: Request = read_json(&mut input).unwrap();
        assert!(matches!(request, Request::Call { ref method, .. } if method == "OpenFile"));
        let mut out = Vec::new();
        receive_stream(&mut input, &mut out).unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(read_frame(&mut input).unwrap(), None);
    }

    #[test]
    fn requests_are_tagged() {
        assert_eq!(
            serde_json::to_string(&Request::Fetch { path: "/a".into() }).unwrap(),
            r#"{"op":"fetch","path":"/a"}"#
        );
        assert_eq!(
            serde_json::from_slice::<Request>(br#"{"op":"screenshot"}"#).unwrap(),
            Request::Screenshot
        );
        assert!(serde_json::from_slice::<Request>(CLOSE).is_err());
    }

    #[test]
    fn bodies_keep_their_values() {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("multiple", Value::from(true));
        let body = Body::from_fields(vec![
            Value::from(zvariant::ObjectPath::try_from("/a/b").unwrap()),
            Value::from("org.example.App"),
            Value::from(""),
            Value::from("Open"),
            Value::from(options),
        ])
        .unwrap();
        assert_eq!(body.signature, "osssa{sv}");
        let fields = body.fields().unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(String::try_from(fields[3].try_clone().unwrap()).unwrap(), "Open");
    }

    #[test]
    fn responses_carry_their_uris() {
        let mut results = HashMap::new();
        results.insert(
            "uris".to_owned(),
            OwnedValue::try_from(Value::from(vec!["file:///home/a/x%20y.txt"])).unwrap(),
        );
        let body = response_body(0, results).unwrap();
        assert_eq!(body.signature, "ua{sv}");
        let (code, results) = response(&body).unwrap();
        assert_eq!(code, 0);
        let uris = result_uris(&results);
        assert_eq!(uris, ["file:///home/a/x%20y.txt"]);
        assert_eq!(
            path_from_uri(&uris[0]).unwrap(),
            std::path::Path::new("/home/a/x y.txt")
        );
    }

    #[test]
    fn uris_and_names_are_checked() {
        assert_eq!(path_from_uri("file:///a/../b"), None);
        assert_eq!(path_from_uri("https://x/y"), None);
        assert_eq!(path_from_uri("file://other/x"), None);
        assert_eq!(path_from_uri("file:///a%00b"), None);
        let path = std::path::Path::new("/home/a/ü b#.png");
        assert_eq!(path_from_uri(&uri_from_path(path)).unwrap(), path);
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
    }

    #[test]
    fn every_portal_has_a_backend() {
        assert_eq!(
            backend(FILE_CHOOSER),
            Some("org.freedesktop.impl.portal.desktop.gtk")
        );
        assert_eq!(
            backend(SCREENSHOT),
            Some("org.freedesktop.impl.portal.desktop.derisk")
        );
        assert_eq!(backend("org.freedesktop.impl.portal.Settings"), None);
    }
}
