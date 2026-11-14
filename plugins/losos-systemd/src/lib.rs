//! A pm plugin that reads systemd unit files into a package's permission profile.
//!
//! Every other signal pm has is an inference. ELF analysis reads `DT_NEEDED` and
//! guesses a library path; source scanning finds a string literal that looks like a
//! path and cannot tell a real one from an error message. Both are approximations,
//! and pm's own documentation is careful to say so.
//!
//! A systemd unit is different in kind: it is a **declaration**. `ReadWritePaths=`
//! does not suggest that a service might write somewhere -- it is the list systemd
//! will make writable and nothing else will be. `StateDirectory=` names the one
//! directory systemd creates. So for a package that ships units, the most accurate
//! description of what it needs at run time is already in the package, written by
//! whoever wrote the service, and nothing was reading it.
//!
//! That makes this the one signal in pm that can be *exact* rather than generous,
//! which matters because a profile only becomes useful when someone is willing to
//! promote it out of audit mode.
//!
//! # Conservative on purpose
//!
//! A grant is emitted only where a directive positively says so. In particular, the
//! absence of `PrivateNetwork=yes` is **not** treated as evidence that a service
//! needs the network -- almost no unit sets it, so treating absence as evidence
//! would hand a network grant to everything and mean nothing. Network is granted
//! only for a directive that names networking outright.
//!
//! # What this can reach
//!
//! Nothing. One import, `log`, which returns nothing. pm hands over a file's text and
//! takes a list of grants back.

#![allow(clippy::same_length_and_capacity)] // wit-bindgen output

wit_bindgen::generate!({ path: "../wit", world: "plugin" });

use pm::plugin::{
    host::{Level, log},
    types::{Capability, Hook, Permission},
};

struct LososSystemd;

/// Directives whose value is one or more paths the service may write.
const WRITE_DIRECTIVES: &[&str] = &["ReadWritePaths", "BindPaths", "StateDirectory"];

/// Directives whose value is one or more paths the service may read.
const READ_DIRECTIVES: &[&str] = &[
    "ReadOnlyPaths",
    "BindReadOnlyPaths",
    "EnvironmentFile",
    "WorkingDirectory",
    "ConfigurationDirectory",
    "AssertPathExists",
    "ConditionPathExists",
];

/// Directives naming a program systemd will execute.
const EXEC_DIRECTIVES: &[&str] = &[
    "ExecStart",
    "ExecStartPre",
    "ExecStartPost",
    "ExecStop",
    "ExecStopPost",
    "ExecReload",
    "ExecCondition",
];

/// Directives that are positive evidence of networking.
const NETWORK_DIRECTIVES: &[&str] = &[
    "IPAddressAllow",
    "IPAddressDeny",
    "Sockets",
    "BindToDevice",
];

/// `StateDirectory=foo` means `/var/lib/foo`, and so on for its siblings. systemd
/// creates these; the unit names only the leaf.
const DIRECTORY_ROOTS: &[(&str, &str)] = &[
    ("StateDirectory", "/var/lib/"),
    ("CacheDirectory", "/var/cache/"),
    ("LogsDirectory", "/var/log/"),
    ("RuntimeDirectory", "/run/"),
    ("ConfigurationDirectory", "/etc/"),
];

/// Split a `Key=Value` line, ignoring comments and section headers.
fn directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    if line.starts_with('[') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

/// Strip the prefixes systemd allows on an `Exec*=` value.
///
/// `-` ignores failure, `@` overrides argv[0], `+` runs with full privileges, `!` and
/// `!!` drop or restore them. They are not part of the path, and a grant that kept
/// them would name a file that does not exist.
fn strip_exec_prefixes(value: &str) -> &str {
    let mut rest = value;
    loop {
        let trimmed = rest
            .strip_prefix("!!")
            .or_else(|| rest.strip_prefix('!'))
            .or_else(|| rest.strip_prefix('-'))
            .or_else(|| rest.strip_prefix('@'))
            .or_else(|| rest.strip_prefix('+'));
        match trimmed {
            Some(next) => rest = next.trim_start(),
            None => return rest,
        }
    }
}

/// Whether a value looks like an absolute path rather than a specifier or a socket.
///
/// systemd `%` specifiers (`%t`, `%S`) are left alone rather than expanded: pm
/// normalises paths but deliberately does not canonicalise them, and a guess at what
/// `%t` becomes on the target would be exactly the kind of invention this plugin
/// exists to avoid.
fn is_path(value: &str) -> bool {
    value.starts_with('/')
}

/// The grants one directive implies.
fn grants_for(line_number: usize, key: &str, value: &str, out: &mut Vec<Grant>) {
    let evidence = |what: &str| format!("{line_number}: {key}= {what}");

    if EXEC_DIRECTIVES.contains(&key) {
        let command = strip_exec_prefixes(value);
        if let Some(program) = command.split_whitespace().next()
            && is_path(program)
        {
            out.push(Grant {
                permission: Permission::ExecPath(program.to_string()),
                evidence: evidence(program),
            });
            // Running anything at all means the service forks and execs.
            out.push(Grant {
                permission: Permission::Spawn,
                evidence: evidence("executes a program"),
            });
        }
        return;
    }

    if let Some((_, root)) = DIRECTORY_ROOTS.iter().find(|(name, _)| *name == key) {
        // These name a leaf, and systemd creates the directory under a fixed root.
        for leaf in value.split_whitespace() {
            let path = format!("{root}{leaf}");
            let permission = if key == "ConfigurationDirectory" {
                Permission::ReadPath(path.clone())
            } else {
                Permission::WritePath(path.clone())
            };
            out.push(Grant {
                permission,
                evidence: evidence(&path),
            });
        }
        return;
    }

    if WRITE_DIRECTIVES.contains(&key) {
        for path in value.split_whitespace().filter(|p| is_path(p)) {
            out.push(Grant {
                permission: Permission::WritePath(path.to_string()),
                evidence: evidence(path),
            });
        }
        return;
    }

    if READ_DIRECTIVES.contains(&key) {
        for path in value.split_whitespace() {
            // EnvironmentFile= may be prefixed with `-` meaning "optional".
            let path = path.strip_prefix('-').unwrap_or(path);
            if is_path(path) {
                out.push(Grant {
                    permission: Permission::ReadPath(path.to_string()),
                    evidence: evidence(path),
                });
            }
        }
        return;
    }

    if NETWORK_DIRECTIVES.contains(&key) {
        out.push(Grant {
            permission: Permission::Network,
            evidence: evidence(value),
        });
        return;
    }

    // Listen* is a socket unit's whole purpose. A value starting with `/` is a Unix
    // socket -- a file -- and anything else is a port or an address, which is the
    // network.
    if key.starts_with("Listen") {
        if is_path(value) {
            out.push(Grant {
                permission: Permission::WritePath(value.to_string()),
                evidence: evidence(value),
            });
        } else {
            out.push(Grant {
                permission: Permission::Network,
                evidence: evidence(value),
            });
        }
        return;
    }

    // PrivateNetwork=no is the one negative form worth reading: it is a deliberate
    // statement that this service needs the host's network. Its absence says nothing,
    // and is not treated as evidence.
    if key == "PrivateNetwork" && value.eq_ignore_ascii_case("no") {
        out.push(Grant {
            permission: Permission::Network,
            evidence: evidence("PrivateNetwork=no"),
        });
    }
}

impl Guest for LososSystemd {
    fn describe() -> Manifest {
        Manifest {
            name: "losos-systemd".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            summary: "Derives run-time permissions from systemd unit files".into(),
            hooks: vec![Hook::ScanSource],
            // scan-source only, so the ceiling is unused -- but an empty list is the
            // honest value, and it means a future edit that started classifying
            // commands would grant nothing until someone widened this deliberately.
            grants_at_most: vec![],
            // Unit types that carry paths or executables. `.target` and `.slice` are
            // deliberately absent: they carry ordering and resource limits, never a
            // path, so asking for them would be asking pm for files with nothing in
            // them to read.
            source_extensions: vec![
                "service".into(),
                "socket".into(),
                "mount".into(),
                "swap".into(),
                "path".into(),
                "timer".into(),
            ],
            // No symbols, for the same reason this plugin has an empty ceiling:
            // it reads units, it does not tell a build file where to put them.
            // The paths this OS uses live in nixos/, which is the tree's own
            // statement of them. A second copy here would be a second answer
            // to one question.
            symbols: vec![],
        }
    }

    /// Not implemented; the component model has no optional export.
    fn classify_command(_command: String) -> Option<Verdict> {
        None
    }

    fn scan_source(file: SourceFile) -> Vec<Grant> {
        let mut grants = Vec::new();

        for (index, line) in file.contents.lines().enumerate() {
            // systemd continues a directive onto the next line with a trailing `\`.
            // Joining them properly would need lookahead; a continued line is
            // skipped rather than half-read, because half a path is a grant for a
            // file that does not exist.
            if line.trim_end().ends_with('\\') {
                continue;
            }
            if let Some((key, value)) = directive(line) {
                grants_for(index + 1, key, value, &mut grants);
            }
        }

        // Duplicates are common -- several Exec* lines running the same binary -- and
        // pm would merge them anyway, but sending one of each keeps `pm profile`
        // readable and the evidence lines meaningful.
        grants.dedup_by(|a, b| format!("{:?}", a.permission) == format!("{:?}", b.permission));

        if !grants.is_empty() {
            log(
                Level::Debug,
                &format!("{}: {} grant(s) from unit directives", file.path, grants.len()),
            );
        }
        grants
    }
}

export!(LososSystemd);

#[allow(dead_code)]
fn _capability_is_used_by_the_world(_c: Capability) {}
