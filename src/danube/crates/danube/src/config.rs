//! Danube's settings: `key = value` lines, as derisk's own are, in the
//! person's `~/.config/danube/settings.conf`; a key it leaves out keeps
//! the default below. An `/etc/danube/settings.conf` from NixOS options
//! was read first once; nothing set them, so it went.
//!
//! There is no `search` key and no reading of what is typed: the window
//! has no address bar. Addresses are typed into derisk's command palette,
//! which opens an address as one and searches for anything else with the
//! desktop's engine, so Danube has nothing of its own to decide.

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// What a new tab opens.
    pub home: String,
    /// JavaScriptCore's JIT compilers. Off by default (docs/danube.md,
    /// "Sandbox and hardening"): the interpreter is slower, and a page
    /// can no longer have the browser write machine code for it, which is
    /// what most WebKit exploits rely on.
    pub jit: bool,
    /// Downloads go through aria2 (downloads.rs); `false` leaves them to
    /// WebKit's own downloader, one connection each.
    pub aria2: bool,
    /// How many connections aria2 splits a download over; one per
    /// hardware thread when unset.
    pub connections: Option<usize>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            home: "about:blank".into(),
            jit: false,
            aria2: true,
            connections: None,
        }
    }
}

impl Settings {
    /// The settings for the person whose home is `home`.
    pub fn load(home: &Path) -> Self {
        let mut settings = Self::default();
        let text = fs::read_to_string(home.join(".config/danube/settings.conf")).unwrap_or_default();
        settings.apply(&text);
        settings
    }

    fn apply(&mut self, text: &str) {
        for (key, value) in pairs(text) {
            match key {
                "home" if !value.is_empty() => self.home = value.into(),
                "javascript.jit" => self.jit = value == "true",
                "downloads.aria2" => self.aria2 = value != "false",
                "downloads.connections" => {
                    self.connections = value.parse().ok().filter(|&n: &usize| n > 0)
                }
                _ => {}
            }
        }
    }
}

fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
}

/// The host of `uri`, for permissions and the window's title; empty for a
/// URI without one.
pub fn host(uri: &str) -> String {
    let Some((_, rest)) = uri.split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if authority.starts_with('[') {
        authority.split_inclusive(']').next().unwrap_or(authority)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.to_ascii_lowercase()
}

/// The origin of `uri`, or `None` when it has no authority.
pub fn origin(uri: &str) -> Option<String> {
    let (scheme, rest) = uri.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if authority.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("https://user@Meet.Example:443/room"), "meet.example");
        assert_eq!(host("http://[::1]:8080/"), "[::1]");
        assert_eq!(host("about:blank"), "");
    }

    #[test]
    fn origins_include_scheme_and_port_but_not_credentials() {
        assert_eq!(
            origin("https://user@Meet.Example:443/room"),
            Some("https://meet.example:443".into())
        );
        assert_eq!(
            origin("http://meet.example/"),
            Some("http://meet.example".into())
        );
        assert_eq!(origin("about:blank"), None);
    }

    #[test]
    fn settings_override_in_order() {
        let mut s = Settings::default();
        s.apply("javascript.jit = true\n# home = x\nhome = \n");
        assert!(s.jit);
        assert_eq!(s.home, "about:blank");
        s.apply("home = https://start.example/\nsearch = google\n");
        assert_eq!(s.home, "https://start.example/");
    }

    #[test]
    fn aria2_is_the_default_downloader() {
        let mut s = Settings::default();
        assert!(s.aria2);
        assert_eq!(s.connections, None);
        s.apply("downloads.connections = 8\ndownloads.aria2 = false\n");
        assert!(!s.aria2);
        assert_eq!(s.connections, Some(8));
        s.apply("downloads.connections = 0\n");
        assert_eq!(s.connections, None);
    }
}
