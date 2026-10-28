//! Joining and leaving, which change what every login trusts, so only root
//! does them: `losos-login-bridge serve`, one process per connection to
//! [`crate::SOCKET`] (nixos/modules/company.nix), asks polkit about the
//! process at the other end (`org.losos.login-bridge.join`, an
//! administrator's password) before it reads a request.
//!
//! One JSON request a connection, one JSON answer back:
//!
//! ```text
//! {"join": {"host", "fingerprint", "user", "password", "code"?, "publicName"?, "adminGroups"?}}
//!   -> {"joined": {...}} | {"error", "needCode"?}
//! {"leave": {}}
//!   -> {"left": true} | {"error"}
//! ```
//!
//! Joining signs in to the box's LosOS Login as `user`, who must be one of
//! its administrators, reads which edge the box goes through (`GET
//! /api/edge`), and refuses a box that is not behind a gateway on the
//! company's network: that gateway is what the computer is joined to.

use std::{
    io::{BufRead, BufReader, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    join::{self, Joined, Way, BOX_ADMINS},
    login::{Outcome, Session},
    tls, Error, JOINED_FILE, SOCKET,
};

/// The polkit action a join or a leave needs.
pub const ACTION: &str = "org.losos.login-bridge.join";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    /// The box's name on the company network.
    pub host: String,
    /// The certificate fingerprint the administrator compared with the
    /// box's pages.
    pub fingerprint: String,
    /// An administrator of the box, and their password.
    pub user: String,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admin_groups: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Request {
    Join(JoinRequest),
    Leave {},
}

/// The answer to a join: the joined box, or why not, and whether a code
/// from the administrator's authenticator app would get further.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Joining {
    Joined(Box<Joined>),
    Failed { error: String, need_code: bool },
}

impl Joining {
    fn failed(error: impl Into<String>) -> Self {
        Joining::Failed {
            error: error.into(),
            need_code: false,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Joining::Joined(j) => json!({ "joined": j }),
            Joining::Failed { error, need_code } => {
                json!({ "error": error, "needCode": need_code })
            }
        }
    }
}

/// Joins this computer to `request.host` and writes `joined_file`.
pub fn join(request: &JoinRequest, joined_file: &Path) -> Joining {
    let host = request
        .host
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !join::dns_name(&host) {
        return Joining::failed(format!("{host} is not a box's name"));
    }
    let pem = match tls::fetch_certificate(&host) {
        Ok(pem) => pem,
        Err(e) => return Joining::failed(e.message),
    };
    let der = tls::pem_to_der(&pem).unwrap_or_default();
    let shown = tls::fingerprint(&der);
    if normalize_fingerprint(&request.fingerprint) != normalize_fingerprint(&shown) {
        return Joining::failed(format!(
            "{host}'s certificate is now {shown}, not the one you compared. Look at the box's pages again."
        ));
    }
    let way = Way::Local {
        host: host.clone(),
        certificate: der,
    };
    let mut asked = false;
    let mut code = |_: &str| {
        asked = true;
        request.code.clone()
    };
    let (session, person) = match Session::open(&way, &request.user, &request.password, &mut code) {
        Ok(signed_in) => signed_in,
        Err(Outcome::Refused(why)) => {
            return Joining::Failed {
                need_code: asked && request.code.is_none(),
                error: why,
            }
        }
        Err(Outcome::Unreachable(why)) => {
            return Joining::failed(format!("LosOS Login at {host} did not answer: {why}"))
        }
        Err(Outcome::Accepted(_)) => unreachable!("open returns a person as Ok"),
    };
    if !person.groups.iter().any(|g| g == BOX_ADMINS) {
        return Joining::failed(format!(
            "{} does not administer {host}; one of its administrators joins computers to it.",
            person.name
        ));
    }
    let edge = match session.get_json("/api/edge") {
        Ok(edge) => edge,
        Err(e) => return Joining::failed(format!("{host} did not say which edge it uses: {e}")),
    };
    let Some(gateway) = join::gateway_from_edge(&edge).filter(|_| join::is_local_gateway(&edge))
    else {
        return Joining::failed(format!(
            "{host} is not behind an edge gateway on this network. Set one up beside it first (LosOS handbook, Edge federation)."
        ));
    };
    let joined = Joined {
        realm: join::realm(&gateway, &host),
        host,
        certificate: pem,
        public_name: request
            .public_name
            .as_deref()
            .map(|n| n.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|n| join::dns_name(n)),
        gateway,
        admin_groups: if request.admin_groups.is_empty() {
            vec![BOX_ADMINS.to_owned()]
        } else {
            request.admin_groups.clone()
        },
        joined_by: person.name,
        joined_at: join::now(),
    };
    match joined.save(joined_file) {
        Ok(()) => Joining::Joined(Box::new(joined)),
        Err(e) => Joining::failed(e.message),
    }
}

/// Fingerprints compare without their colons and case.
pub fn normalize_fingerprint(f: &str) -> String {
    f.chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// The process at the other end of `stream`: its pid and uid.
fn peer(stream: &UnixStream) -> Result<(libc::pid_t, libc::uid_t), Error> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a ucred-sized buffer for SO_PEERCRED on a socket we hold.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((cred.pid, cred.uid))
}

/// The process's start time, field 22 of /proc/<pid>/stat, which pins
/// the pid polkit is asked about to this process and not a later one.
fn start_time(pid: libc::pid_t) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may hold spaces.
    let after = &stat[stat.rfind(')')? + 2..];
    after.split(' ').nth(19).map(str::to_owned)
}

/// Whether polkit lets the process at the other end do [`ACTION`],
/// asking its session's agent for an administrator's password.
fn allowed(stream: &UnixStream) -> Result<bool, Error> {
    let (pid, uid) = peer(stream)?;
    if uid == 0 {
        return Ok(true);
    }
    let start = start_time(pid).ok_or_else(|| Error::new("the caller is gone"))?;
    let status = Command::new("pkcheck")
        .args([
            "--action-id",
            ACTION,
            "--allow-user-interaction",
            "--process",
        ])
        .arg(format!("{pid},{start},{uid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::new(format!("cannot run pkcheck: {e}")))?;
    Ok(status.success())
}

/// Serves the one connection systemd handed over as stdin.
pub fn serve(stream: UnixStream) -> Result<(), Error> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream.try_clone()?;
    let answer = if allowed(&stream)? {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        match serde_json::from_str::<Request>(&line) {
            Ok(Request::Join(request)) => join(&request, Path::new(JOINED_FILE)).to_json(),
            Ok(Request::Leave {}) => match std::fs::remove_file(JOINED_FILE) {
                Ok(()) => json!({ "left": true }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({ "left": true }),
                Err(e) => json!({ "error": e.to_string() }),
            },
            Err(e) => json!({ "error": format!("not a request: {e}") }),
        }
    } else {
        json!({ "error": "Only an administrator of this computer can join it to a company or take it out." })
    };
    writeln!(writer, "{answer}")?;
    Ok(())
}

/// Sends `request` to the service and returns its answer.
pub fn call(request: &Request) -> Result<Value, Error> {
    let stream = UnixStream::connect(SOCKET).map_err(|e| Error::new(format!("{SOCKET}: {e}")))?;
    let mut writer = stream.try_clone()?;
    writeln!(
        writer,
        "{}",
        serde_json::to_string(request).expect("a request serializes")
    )?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line)
        .map_err(|e| Error::new(format!("the service answered {line:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_read_as_the_service_expects() {
        let line = r#"{"join":{"host":"mattbox.local","fingerprint":"AB:CD","user":"matus","password":"pw","code":"123456"}}"#;
        let Request::Join(request) = serde_json::from_str(line).unwrap() else {
            panic!("a join");
        };
        assert_eq!(request.code.as_deref(), Some("123456"));
        assert!(request.admin_groups.is_empty());
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"leave":{}}"#).unwrap(),
            Request::Leave {}
        );
    }

    #[test]
    fn compares_fingerprints_as_people_type_them() {
        assert_eq!(normalize_fingerprint("ab:cd:0f"), "ABCD0F");
        assert_eq!(normalize_fingerprint(" AB CD 0F "), "ABCD0F");
    }

    #[test]
    fn reads_a_start_time_past_a_name_with_spaces() {
        // Only the shape matters: this process's own stat.
        let me = std::process::id() as libc::pid_t;
        assert!(start_time(me).is_some_and(|t| t.chars().all(|c| c.is_ascii_digit())));
    }

    #[test]
    fn a_join_says_when_a_code_would_help() {
        let failed = Joining::Failed {
            error: "LosOS Login asks for a verification code.".into(),
            need_code: true,
        };
        assert_eq!(failed.to_json()["needCode"], true);
    }
}
