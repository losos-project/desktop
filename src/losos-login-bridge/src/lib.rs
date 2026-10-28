//! losos-login-bridge: company accounts from a LosOS box's LosOS Login,
//! turned into systemd-homed records on this computer (docs/company.md).
//!
//! A company keeps its people in LosOS Login on one of its boxes, behind
//! the edge gateway on its network. An administrator joins a computer to
//! that box once ([`join`]); from then on anyone with an account there can
//! log in here with the same name and password:
//!
//! - the PAM module ([`pam`]) asks LosOS Login about the password at the
//!   login screen, before pam_systemd_home does ([`login`]);
//! - LosOS Login saying yes makes or updates the person's homed record
//!   with that password ([`homed`]): their realm is the company's, which is
//!   what marks a company account;
//! - LosOS Login saying no keeps them out, while it can be asked;
//! - when it cannot be asked (no network, the box off), homed's own record
//!   decides, so the password of the last login that LosOS Login accepted
//!   still logs in: the cached login a laptop needs on a train.
//!
//! homed keeps each home a LUKS volume whose key is the password, so the
//! password has to reach homed as the person types it; nothing here keeps
//! a copy.

pub mod homed;
pub mod join;
pub mod login;
pub mod pam;
pub mod service;
pub mod tls;

use std::fmt;

/// Where the computer keeps what joining it wrote: root's alone.
pub const STATE_DIR: &str = "/var/lib/losos-login-bridge";
/// The joined box, in [`STATE_DIR`].
pub const JOINED_FILE: &str = "/var/lib/losos-login-bridge/joined.json";
/// The join service's socket, which any local user may open; the service
/// asks polkit about each caller.
pub const SOCKET: &str = "/run/losos-login-bridge/join.sock";

/// What went wrong, as one line a person can read on the login screen or
/// in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    /// The box could not be reached at all, rather than refusing.
    pub unreachable: bool,
}

impl Error {
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

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(code, response) => {
                Self::new(format!("{} answered {code}", response.get_url()))
            }
            ureq::Error::Transport(t) => Self {
                message: t.to_string(),
                unreachable: true,
            },
        }
    }
}

/// A user name both LosOS Login and homed take: what lososd allows
/// (backend/src/people.rs, `validate_name`) narrowed to what homed allows,
/// so no `.`, and at most 32 characters.
pub fn valid_user_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_names_both_sides_take() {
        assert!(valid_user_name("ada"));
        assert!(valid_user_name("ada_l-2"));
        assert!(!valid_user_name(""));
        assert!(!valid_user_name("Ada"));
        assert!(!valid_user_name("2ada"));
        assert!(!valid_user_name("ada.lovelace"));
        assert!(!valid_user_name("root;rm"));
        assert!(!valid_user_name(&"a".repeat(33)));
    }
}
