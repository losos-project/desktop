//! losos-backup: each person's backups to a LosOS box on the local network
//! (docs/backup.md). Git repositories in the home go to the box's LosOS Git,
//! each pushed to a private repository of the person's own, and everything
//! else in the home goes to their LosOS cloud over WebDAV, only what
//! changed since the last run. The person signs both in at once through
//! LosOS Login ([`login`]).
//!
//! It runs on the host as the person, from a user timer, because that is
//! where the home is and only while they are signed in is it unlocked
//! (systemd-homed). It reaches the box the way every machine does: through
//! the LosOS boxes machine where one routes boxes ([`boxes`]), straight to
//! the box's name where none does, as on a phone.

pub mod boxes;
pub mod cloud;
pub mod config;
pub mod forge;
pub mod login;
pub mod run;
pub mod walk;

use std::fmt;

/// Why a request to the box, or a step of a run, failed: one line, which
/// the report keeps and Settings shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// What went wrong.
    pub message: String,
    /// Whether the box could not be reached at all, rather than refusing
    /// one request: a run stops at the first few of those instead of
    /// failing every file in turn.
    pub unreachable: bool,
}

impl Error {
    /// An error saying `what`.
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            message: what.into(),
            unreachable: false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(e.to_string())
    }
}

/// A failed HTTP request as one line: the status and the first line of
/// what the server said, which for Nextcloud and Forgejo names the reason.
impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(code, response) => {
                let url = response.get_url().to_owned();
                let body = response.into_string().unwrap_or_default();
                let reason = server_message(&body);
                Self::new(if reason.is_empty() {
                    format!("{url} answered {code}")
                } else {
                    format!("{url} answered {code}: {reason}")
                })
            }
            ureq::Error::Transport(t) => Self {
                message: t.to_string(),
                unreachable: true,
            },
        }
    }
}

/// The message in a server's error body: Forgejo's JSON `message`,
/// Nextcloud's WebDAV `<s:message>`, or else the first non-empty line,
/// cut short.
pub fn server_message(body: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(m) = json.get("message").and_then(|m| m.as_str()) {
            return m.to_owned();
        }
        if let Some(m) = json.pointer("/ocs/meta/message").and_then(|m| m.as_str()) {
            return m.to_owned();
        }
    }
    if let Some(start) = body.find("<s:message>") {
        let rest = &body[start + "<s:message>".len()..];
        if let Some(end) = rest.find("</s:message>") {
            return rest[..end].trim().to_owned();
        }
    }
    let line = body.lines().map(str::trim).find(|l| !l.is_empty());
    line.unwrap_or_default().chars().take(160).collect()
}

/// `s` with every byte but RFC 3986's unreserved ones percent-encoded, for
/// one segment of a URL's path.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A relative path, `/`-separated, with each segment encoded.
pub fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Seconds since the epoch, now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// This computer's name, which the backups are filed under on the box so
/// that two computers of one person do not overwrite each other.
pub fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "computer".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_path_segment_by_segment() {
        assert_eq!(
            encode_path("Documents/My file#1.txt"),
            "Documents/My%20file%231.txt"
        );
        assert_eq!(encode_segment("a/b"), "a%2Fb");
        assert_eq!(encode_segment("čaj"), "%C4%8Daj");
    }

    #[test]
    fn finds_the_reason_in_a_server_answer() {
        assert_eq!(
            server_message(r#"{"message":"repository already exists"}"#),
            "repository already exists"
        );
        let dav = "<?xml version=\"1.0\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\">\n  <s:exception>Sabre\\DAV\\Exception\\Forbidden</s:exception>\n  <s:message>.htaccess is a forbidden file or folder name.</s:message>\n</d:error>";
        assert_eq!(
            server_message(dav),
            ".htaccess is a forbidden file or folder name."
        );
        assert_eq!(server_message("\n  Bad gateway\nmore"), "Bad gateway");
    }
}
