//! What both ends of the Agent's machine agree on (docs/agent.md, "Its own
//! machine"): where each person's machine is on vsock, which links open the
//! Agent, and which of derisk's agent requests the machine may make of the
//! person's desktop.
//!
//! Each person's Agent runs in a machine of their own, `agent-<user>`, made
//! by nixos/modules/agent.nix. It has three ways to the host and no other:
//! its windows, through waypipe to derisk's gate like any machine's; the
//! portal machine's dialogs, which hand it copies of the files the person
//! picks, as for every machine; and the tools channel here, which carries
//! the requests [`classify`] lets through.

use serde_json::{json, Value};

/// A person's machine is at vsock address `CID_BASE + uid`. Unique for
/// every account, fixed, so neither end has to ask libvirt, and far above
/// the addresses libvirt hands out from 3 and the service machines' 41xx.
pub const CID_BASE: u32 = 1_000_000;

/// The port the machine's `launch` listens on, for the host's root end
/// (`serve-open`) alone: it refuses any connection that is not from the
/// host and from a port under 1024, which only root can bind.
pub const LAUNCH_PORT: u32 = 7365;

/// The socket a person's session opens the Agent through, served by
/// `serve-open` as root.
pub const OPEN_SOCKET: &str = "/run/losos-agent-machine/open.sock";

/// The longest line either end reads: derisk's own limit for one request.
pub const MAX_LINE: u64 = 1 << 20;

/// The machine's unit for the person with `uid`, which `serve-open` starts.
pub fn unit(uid: u32) -> String {
    format!("losos-agent-machine@{uid}.service")
}

/// The vsock address of the machine of the person with `uid`. Accounts
/// that run nothing, root and the system's, have none.
pub fn machine_cid(uid: u32) -> Option<u32> {
    if uid < 1000 {
        return None;
    }
    CID_BASE.checked_add(uid)
}

/// The port of the tools channel on the host for the machine at `cid`: its
/// own address, which the machine reads from its vsock device and the
/// person's session works out from its uid. One port per person, so two
/// people signed in at once each have their own.
pub fn tools_port(cid: u32) -> u32 {
    cid
}

/// The requests the machine may send derisk's agent socket: the ones
/// `derisk mcp` offers as tools. Looking at the desktop and its
/// accessibility tree, pointer and keyboard input, and actions, of which
/// derisk has the person confirm any destructive one on screen. Not menus,
/// widgets, palette data or `choose`, which would let the machine put
/// things on the desktop that look like the system's.
pub const DESKTOP_METHODS: &[&str] = &[
    "state",
    "dispatch",
    "ask",
    "tree",
    "find",
    "act",
    "screenshot",
    "input",
];

/// What one request on the tools channel is.
#[derive(Debug, PartialEq, Eq)]
pub enum Call {
    /// For derisk's agent socket, as the line to send it.
    Desktop(String),
    /// A web page to open in the person's browser on the desktop.
    OpenUri(String),
    /// The desktop's theme, which the Agent's window draws itself in.
    Theme,
    /// Anything else, with the reason for the machine.
    Refused(String),
}

/// Sorts one request line from the machine. A desktop request is sent on
/// as it was parsed, not as it came: a line with `method` twice could
/// otherwise say one thing here and another to derisk.
pub fn classify(line: &str) -> Call {
    let request: Value = match serde_json::from_str(line) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(_) => return Call::Refused("a request is a JSON object".into()),
        Err(e) => return Call::Refused(format!("bad request: {e}")),
    };
    match request["method"].as_str() {
        Some("open_uri") => match request["uri"].as_str() {
            Some(uri) if is_web_url(uri) => Call::OpenUri(uri.to_owned()),
            _ => Call::Refused("only http and https links open on the desktop".into()),
        },
        Some("theme") => Call::Theme,
        Some(method) if DESKTOP_METHODS.contains(&method) => Call::Desktop(request.to_string()),
        Some(method) => Call::Refused(format!("{method} is not for the Agent's machine")),
        None => Call::Refused("a request names its method".into()),
    }
}

/// An http or https link of one line, which the person's browser may be
/// handed. Not file:, which would open a host file the machine names, nor
/// any scheme whose handler is a program.
pub fn is_web_url(uri: &str) -> bool {
    let lower = uri.get(..8).unwrap_or(uri).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && uri.len() <= 8192
        && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// The link a launch request carries, when it is one the Agent opens: none,
/// for its window, or a `derisk-agent:` link such as the palette's "Ask the
/// agent". Anything else would become an argument to `derisk-agent`.
pub fn agent_link(line: &str) -> Option<&str> {
    if line.chars().any(char::is_control) {
        return None;
    }
    (line.is_empty() || line.starts_with("derisk-agent:")).then_some(line)
}

/// A reply line, in derisk's shape.
pub fn reply(result: Result<Value, String>) -> String {
    match result {
        Ok(result) => json!({"ok": true, "result": result}).to_string(),
        Err(error) => json!({"ok": false, "error": error}).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_person_has_an_address_of_their_own() {
        assert_eq!(machine_cid(60001), Some(1_060_001));
        assert_eq!(machine_cid(1000), Some(1_001_000));
        assert_eq!(machine_cid(0), None);
        assert_eq!(machine_cid(999), None);
        assert_eq!(machine_cid(u32::MAX), None);
        assert_eq!(tools_port(1_060_001), 1_060_001);
        assert_eq!(unit(60001), "losos-agent-machine@60001.service");
    }

    #[test]
    fn desktop_tools_go_through() {
        let call = classify(r#"{"method":"screenshot","inline":true}"#);
        let Call::Desktop(line) = call else {
            panic!("refused: {call:?}")
        };
        let sent: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(sent, json!({"method": "screenshot", "inline": true}));
        for method in DESKTOP_METHODS {
            assert!(matches!(
                classify(&json!({"method": method}).to_string()),
                Call::Desktop(_)
            ));
        }
    }

    #[test]
    fn the_rest_of_the_socket_is_refused() {
        for method in [
            "register_menu",
            "register_widget",
            "remove_widget",
            "register_palette",
            "register_tree",
            "choose",
            "tools",
        ] {
            assert!(
                matches!(
                    classify(&json!({"method": method}).to_string()),
                    Call::Refused(_)
                ),
                "{method}"
            );
        }
        assert!(matches!(classify("[]"), Call::Refused(_)));
        assert!(matches!(classify("{"), Call::Refused(_)));
        assert!(matches!(classify("{}"), Call::Refused(_)));
    }

    #[test]
    fn a_doubled_method_is_sent_as_it_was_judged() {
        // serde_json keeps the last of a repeated key; whatever derisk
        // would make of the original, it gets one method, the judged one.
        let line = r#"{"method":"register_menu","method":"state"}"#;
        assert_eq!(
            classify(line),
            Call::Desktop(r#"{"method":"state"}"#.into())
        );
        let line = r#"{"method":"state","method":"register_menu"}"#;
        assert!(matches!(classify(line), Call::Refused(_)));
    }

    #[test]
    fn only_web_links_open_on_the_desktop() {
        assert_eq!(
            classify(r#"{"method":"open_uri","uri":"https://claude.ai/oauth?x=1"}"#),
            Call::OpenUri("https://claude.ai/oauth?x=1".into())
        );
        assert!(is_web_url("HTTP://example.com"));
        for uri in [
            "file:///etc/shadow",
            "derisk-agent:ask?prompt=x",
            "javascript:alert(1)",
            "https://a b",
            "https://a\nb",
            "https:",
            "",
        ] {
            assert!(!is_web_url(uri), "{uri}");
        }
        assert!(matches!(
            classify(r#"{"method":"open_uri","uri":"file:///"}"#),
            Call::Refused(_)
        ));
        assert!(matches!(
            classify(r#"{"method":"open_uri"}"#),
            Call::Refused(_)
        ));
    }

    #[test]
    fn launches_carry_agent_links_only() {
        assert_eq!(agent_link(""), Some(""));
        assert_eq!(
            agent_link("derisk-agent:ask?prompt=hi"),
            Some("derisk-agent:ask?prompt=hi")
        );
        assert_eq!(agent_link("--help"), None);
        assert_eq!(agent_link("mcp"), None);
        assert_eq!(agent_link("derisk-agent:ask\u{0}"), None);
    }
}
