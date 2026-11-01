//! Captive portals: the sign-in page a hotel or train network shows before
//! it lets anything through.
//!
//! `danube captive-watch` runs in each session (a user service from
//! nixos/modules/danube.nix). Whenever NetworkManager reports a change and
//! says the machine is connected, it fetches [`CHECK_URL`] over plain HTTP, which a portal intercepts. An
//! answer other than [`EXPECT`] means a portal is in the way, and it opens
//! `danube --captive-portal`: one window, no tabs, a throwaway profile and
//! no content blockers, which closes by itself once the check passes. The
//! desktop entry's "Sign in to network" action opens the same window by
//! hand.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// GNOME's check, as NetworkManager's GNOME setup uses; each check sends
/// the request line, the host and "Danube" as the User-Agent, nothing
/// else. It and [`EXPECT`] were NixOS options, written to
/// /etc/danube/captive-portal.conf, which nothing ever set.
pub const CHECK_URL: &str = "http://nmcheck.gnome.org/check_network_status.txt";

/// The body of [`CHECK_URL`]'s 200 answer when the network lets traffic
/// through.
const EXPECT: &str = "NetworkManager is online";

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Online,
    /// Something answered, but not what the check expects: a portal.
    Portal,
    /// No answer at all: offline, which no browser window can fix.
    Offline,
}

/// Splits `http://host[:port]/path` into its parts; only plain HTTP, the
/// only scheme a portal can intercept without a certificate error.
fn parse(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, 80),
    };
    Some((host.to_owned(), port, path.to_owned()))
}

/// Reads an HTTP answer's status and body. The body is matched line by
/// line, so a chunked answer, which HTTP/1.1 allows, passes too: its chunk
/// sizes sit on lines of their own.
fn judge(response: &[u8]) -> Verdict {
    let text = String::from_utf8_lossy(response);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Verdict::Portal;
    };
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status == 200 && body.lines().any(|line| line.trim() == EXPECT) {
        Verdict::Online
    } else {
        Verdict::Portal
    }
}

pub fn check() -> Verdict {
    let Some((host, port, path)) = parse(CHECK_URL) else {
        return Verdict::Offline;
    };
    let Some(addr) = (host.as_str(), port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    else {
        return Verdict::Offline;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(5)) else {
        return Verdict::Offline;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    // HTTP/1.1: a proxy that speaks nothing older, as Envoy by default,
    // answers HTTP/1.0 with 426 Upgrade Required, which read as a portal and
    // opened the sign-in window at every login on such a network.
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Danube\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return Verdict::Offline;
    }
    let mut response = Vec::new();
    // Enough for any check answer; a portal page past it changes nothing.
    let _ = stream.take(64 * 1024).read_to_end(&mut response);
    if response.is_empty() {
        return Verdict::Offline;
    }
    judge(&response)
}

/// Whether `nmcli -t -g STATE general` says the machine is connected:
/// `connected`, or `connected (site only)` or `(local only)`, which is what
/// a portal that answers nothing but its own page can look like.
fn connected(state: &str) -> bool {
    state.trim().starts_with("connected")
}

fn network_state() -> String {
    Command::new("nmcli")
        .args(["-t", "-g", "STATE", "general"])
        .stderr(Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// Watches for network changes and opens the sign-in window when a portal
/// is in the way. Runs until killed.
///
/// `nmcli monitor` prints a line for every change NetworkManager makes,
/// and a join prints several in a row; they are read on a thread of their
/// own so the burst can be let settle, and checked once. networkd's state
/// file was polled here before, and went with networkd (network.nix).
pub fn watch(danube: &std::path::Path) -> ! {
    loop {
        let (changes, changed) = mpsc::channel();
        let monitor = Command::new("nmcli")
            .arg("monitor")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut monitor = match monitor {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, "cannot watch NetworkManager");
                std::thread::sleep(Duration::from_secs(30));
                continue;
            }
        };
        if let Some(stdout) = monitor.stdout.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if line.is_err() || changes.send(()).is_err() {
                        return;
                    }
                }
            });
        }
        // Once at the start, for the network the session began on, then
        // after every change.
        loop {
            // A just-joined network may take a moment to hand out DNS, and
            // the rest of the burst comes meanwhile.
            std::thread::sleep(Duration::from_secs(2));
            while changed.try_recv().is_ok() {}
            if connected(&network_state()) && check() == Verdict::Portal {
                tracing::info!("a captive portal is in the way; opening the sign-in window");
                match Command::new(danube).arg("--captive-portal").status() {
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "cannot open the sign-in window"),
                }
            }
            // nmcli ended: NetworkManager restarted, or went away.
            if changed.recv().is_err() {
                break;
            }
        }
        let _ = monitor.wait();
        std::thread::sleep(Duration::from_secs(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judges_answers() {
        assert_eq!(
            judge(b"HTTP/1.1 302 Found\r\nLocation: http://portal/\r\n\r\n"),
            Verdict::Portal
        );
        assert_eq!(
            judge(b"HTTP/1.1 200 OK\r\n\r\nNetworkManager is online\n"),
            Verdict::Online
        );
        assert_eq!(
            judge(b"HTTP/1.1 200 OK\r\n\r\n<html>Log in</html>"),
            Verdict::Portal
        );
        assert_eq!(
            judge(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                  19\r\nNetworkManager is online\n\r\n0\r\n\r\n"
            ),
            Verdict::Online
        );
    }

    #[test]
    fn reads_networkmanagers_state() {
        assert!(connected("connected\n"));
        assert!(connected("connected (site only)"));
        assert!(!connected("connecting"));
        assert!(!connected("disconnected"));
        assert!(!connected(""));
    }

    #[test]
    fn parses_urls() {
        assert_eq!(
            parse(CHECK_URL),
            Some((
                "nmcheck.gnome.org".into(),
                80,
                "/check_network_status.txt".into()
            ))
        );
        assert_eq!(
            parse("http://a.example:8080"),
            Some(("a.example".into(), 8080, "/".into()))
        );
        assert_eq!(parse("https://a.example/"), None);
    }
}
