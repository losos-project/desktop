//! The wire format and the access rules of `losos-vm-call`.
//!
//! A call is one connection. The caller sends its arguments, then its
//! standard input, and the machine sends back the command's standard output
//! and standard error as they come, then how it exited. Each piece is a
//! frame: a tag byte, a big-endian `u32` length, and that many bytes.

use std::io::{self, Read, Write};
use std::path::Path;

/// The caller's arguments, each ended by a NUL. Always the first frame.
pub const ARGS: u8 = b'A';
/// A piece of the caller's standard input.
pub const STDIN: u8 = b'I';
/// The caller's standard input ended. Nothing follows from the caller; the
/// connection stays open until the command exits, and closing it early
/// stops the command.
pub const STDIN_END: u8 = b'E';
/// A piece of the command's standard output.
pub const STDOUT: u8 = b'O';
/// A piece of the command's standard error.
pub const STDERR: u8 = b'R';
/// The command exited: its status as a big-endian `i32`, 128 plus the
/// signal's number if a signal ended it, as a shell reports it. The last
/// frame.
pub const EXIT: u8 = b'X';

/// The most one frame carries. Arguments are a few hundred bytes, and pipe
/// reads come in pieces of 64 KiB at most.
pub const MAX_FRAME: u32 = 1 << 20;

/// The highest port the host side calls from. Linux lets only a process with
/// CAP_NET_BIND_SERVICE bind a vsock port below 1024, so a machine that
/// sees a call from one knows root on the host made it, and refuses any
/// other: an unprivileged host process can open a vsock connection to any
/// machine, but not from such a port.
pub const PRIVILEGED_PORTS: std::ops::RangeInclusive<u32> = 512..=1023;

/// The vsock address of the host, the only peer a machine takes calls from.
pub const HOST_CID: u32 = 2;

/// Where the host's relay listens for each call, as `<name>.sock`.
pub const SOCKET_DIR: &str = "/run/losos-vm-call";

/// Writes one frame.
pub fn write_frame(out: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut head = [0u8; 5];
    head[0] = tag;
    head[1..].copy_from_slice(&len.to_be_bytes());
    out.write_all(&head)?;
    out.write_all(payload)?;
    out.flush()
}

/// Reads one frame, or `None` where the stream ended between frames.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match input.read_exact(&mut head[..1]) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    input.read_exact(&mut head[1..])?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes"),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    input.read_exact(&mut payload)?;
    Ok(Some((head[0], payload)))
}

/// The payload of an [`ARGS`] frame.
pub fn encode_args<S: AsRef<str>>(args: &[S]) -> Vec<u8> {
    let mut out = Vec::new();
    for arg in args {
        out.extend_from_slice(arg.as_ref().as_bytes());
        out.push(0);
    }
    out
}

/// The arguments in an [`ARGS`] frame. An argument cannot hold a NUL, so
/// none is lost; one that is not UTF-8 is refused, since a command line
/// built from it would not be the one the caller wrote.
pub fn decode_args(payload: &[u8]) -> io::Result<Vec<String>> {
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = payload.strip_suffix(&[0]) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "arguments not NUL-ended",
        ));
    };
    body.split(|b| *b == 0)
        .map(|arg| {
            String::from_utf8(arg.to_vec())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "argument not UTF-8"))
        })
        .collect()
}

/// Who may make a call through the host's relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allow {
    /// Members of this group.
    Group(String),
    /// Anyone signed in at this computer right now, in the foreground: the
    /// person at the screen, which is who NetworkManager's own polkit rules
    /// let change the network.
    ActiveSession,
}

/// One call the host's relay offers, a line of its configuration:
/// `<name> <cid> <port> <allow>...`, where each allow is `group:<name>` or
/// `active-session`. Root may always call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub name: String,
    pub cid: u32,
    pub port: u32,
    pub allow: Vec<Allow>,
}

/// Reads the relay's configuration. Blank lines and `#` comments are
/// skipped; any other line that is not a call is an error, so a typo does
/// not quietly drop a call.
pub fn parse_calls(text: &str) -> Result<Vec<Call>, String> {
    let mut calls = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| format!("line {}: {what}: {line}", number + 1);
        let mut words = line.split_whitespace();
        let name = words.next().ok_or_else(|| bad("no name"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(bad("a name is letters, digits, - and _"));
        }
        let number_of = |word: Option<&str>, what: &str| {
            word.and_then(|w| w.parse::<u32>().ok())
                .ok_or_else(|| bad(what))
        };
        let cid = number_of(words.next(), "no vsock address")?;
        let port = number_of(words.next(), "no vsock port")?;
        let allow = words
            .map(|word| match word {
                "active-session" => Ok(Allow::ActiveSession),
                _ => word
                    .strip_prefix("group:")
                    .filter(|g| !g.is_empty())
                    .map(|g| Allow::Group(g.to_owned()))
                    .ok_or_else(|| bad("allow is group:<name> or active-session")),
            })
            .collect::<Result<_, _>>()?;
        calls.push(Call {
            name: name.to_owned(),
            cid,
            port,
            allow,
        });
    }
    Ok(calls)
}

/// Whether logind's record of a user, `/run/systemd/users/<uid>`, says they
/// have a session in the foreground.
pub fn user_state_active(record: &str) -> bool {
    record.lines().any(|line| line.trim() == "STATE=active")
}

/// Whether the caller may make `call`: root, or a user one of its allows
/// covers. `groups` are the caller's group names, `logind` the directory of
/// logind's user records.
pub fn allowed(call: &Call, uid: u32, groups: &[String], logind: &Path) -> bool {
    uid == 0
        || call.allow.iter().any(|allow| match allow {
            Allow::Group(group) => groups.iter().any(|g| g == group),
            Allow::ActiveSession => std::fs::read_to_string(logind.join(uid.to_string()))
                .is_ok_and(|record| user_state_active(&record)),
        })
}

/// The status a shell would report for `status`.
pub fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, ARGS, &encode_args(&["-t", "device", ""])).unwrap();
        write_frame(&mut wire, STDIN_END, &[]).unwrap();
        let mut input = wire.as_slice();
        let (tag, payload) = read_frame(&mut input).unwrap().unwrap();
        assert_eq!(tag, ARGS);
        assert_eq!(decode_args(&payload).unwrap(), ["-t", "device", ""]);
        assert_eq!(read_frame(&mut input).unwrap(), Some((STDIN_END, vec![])));
        assert_eq!(read_frame(&mut input).unwrap(), None);
    }

    #[test]
    fn no_arguments() {
        assert!(decode_args(&encode_args::<&str>(&[])).unwrap().is_empty());
    }

    #[test]
    fn refuses_bad_frames() {
        let mut huge = vec![STDOUT];
        huge.extend_from_slice(&(MAX_FRAME + 1).to_be_bytes());
        assert!(read_frame(&mut huge.as_slice()).is_err());
        // Cut off inside a frame is an error, not the end.
        assert!(read_frame(&mut [STDOUT, 0, 0].as_slice()).is_err());
        assert!(decode_args(b"no-nul").is_err());
        assert!(decode_args(&[0xff, 0]).is_err());
    }

    #[test]
    fn reads_calls() {
        let calls = parse_calls(
            "# the network machine\n\
             nmcli 4100 5000 group:networkmanager group:wheel active-session\n\
             \n\
             net-mode 4100 5001\n",
        )
        .unwrap();
        assert_eq!(
            calls,
            [
                Call {
                    name: "nmcli".into(),
                    cid: 4100,
                    port: 5000,
                    allow: vec![
                        Allow::Group("networkmanager".into()),
                        Allow::Group("wheel".into()),
                        Allow::ActiveSession,
                    ],
                },
                Call {
                    name: "net-mode".into(),
                    cid: 4100,
                    port: 5001,
                    allow: vec![],
                },
            ]
        );
        assert!(parse_calls("nmcli 4100").is_err());
        assert!(parse_calls("../x 4100 5000").is_err());
        assert!(parse_calls("nmcli 4100 5000 everyone").is_err());
    }

    #[test]
    fn who_may_call() {
        let dir = std::env::temp_dir().join(format!("losos-vm-call-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("1000"), "NAME=ana\nSTATE=active\n").unwrap();
        std::fs::write(dir.join("1001"), "NAME=bo\nSTATE=online\n").unwrap();
        let call = |allow| Call {
            name: "nmcli".into(),
            cid: 4100,
            port: 5000,
            allow,
        };
        let session = call(vec![Allow::ActiveSession]);
        assert!(allowed(&session, 1000, &[], &dir));
        assert!(!allowed(&session, 1001, &[], &dir));
        assert!(!allowed(&session, 1002, &[], &dir));
        assert!(allowed(&session, 0, &[], &dir));
        let wheel = call(vec![Allow::Group("wheel".into())]);
        assert!(allowed(&wheel, 1001, &["users".into(), "wheel".into()], &dir));
        assert!(!allowed(&wheel, 1000, &["users".into()], &dir));
        assert!(!allowed(&call(vec![]), 1000, &["wheel".into()], &dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
