//! A pm plugin that teaches pm about Nix.
//!
//! pm is the system manager in this NixOS image (`docs/nixos.md`), and users can
//! use it to drive the flake -- a build file whose
//! steps are `nix flake check` and `nix build .#image`. pm's fingerprint table
//! knows neither word, so before this plugin such a file was refused before any
//! step ran (C2), with a diagnostic naming the command and nothing else.
//!
//! # What gets a verdict
//!
//! The commands that *read or build* a flake: `nix build`, `nix eval`, `nix
//! flake check|show|metadata`, `nix path-info`, `nix log`, `nix derivation
//! show`, the read-only `nix store` subcommands, `nix hash` and `nix nar`, and
//! their older spellings `nix-build`, `nix-instantiate`, `nix-hash` and the
//! querying operations of `nix-store`.
//!
//! # What is refused, and why that is a feature
//!
//! A plugin cannot make pm refuse anything -- `None` only leaves pm's own "no
//! fingerprint matches" in place. So the refusals below are commands this plugin
//! *recognises* and deliberately declines to size a jail for, with a line in the
//! log saying why, rather than commands it has never heard of:
//!
//! * `nixos-rebuild`, `nixos-install`, `nix-env`, `nix profile`: they change a
//!   machine rather than build something. A build step has no business changing
//!   the machine it runs on, and this OS is not updated that way at all -- an
//!   update is a new `/usr` from systemd-sysupdate (`nixos/modules/update.nix`),
//!   and the image carries no Nix to run a switch with.
//! * `nix run|shell|develop|fmt|bundle|repl|env`, `nix-shell`: each runs a
//!   program the step does not name. A jail sized for `nix` is not a jail sized
//!   for whatever the flake's app turns out to be.
//! * `nix copy`, `nix-copy-closure`, `nix-store --serve|--import|--gc|...`: they
//!   write to or serve a store other than reading the build's own.
//! * `nix flake update|lock|prefetch`, `nix-prefetch-url`, `nix-channel`: they
//!   fetch something newer than what is pinned. A pin moves by hand in this tree
//!   (CLAUDE.md, "Keeping the pins current"), and `flake.lock` is one.
//!
//! # nixpkgs, through the pin and nowhere else
//!
//! The flake's `legacyPackages` is the whole of nixpkgs at the revision
//! `flake.lock` pins, built for the image's stock glibc platform -- the same
//! package set it is made of -- and `.#pm-payloads.<name>` is any of those packages linked
//! statically and checked by Nix itself to refer to nothing in the store, which
//! is what a pm package extracted at `/pkg` can actually run (docs/nixos.md,
//! "nixpkgs in a pm build"). That makes all of nixpkgs reachable from a pm build
//! file without any of it being unpinned, provided the build file names it by a
//! reference that cannot move. So a flake reference that can move gets no
//! verdict either, for the same reason `nix flake update` gets none:
//!
//! * a registry name -- `nixpkgs#hello`, `flake:nixpkgs`, `nixpkgs/nixos-26.05`
//!   -- which resolves to whatever the registry says today, and by default
//!   that is nixpkgs' newest commit on GitHub;
//! * `github:`, `gitlab:` and `sourcehut:` without a commit, and the `git+`
//!   schemes without `rev=`, because a branch or a tag is a name, not content;
//! * a tarball URL without `narHash=`, because the server decides what it holds;
//! * a `<nixpkgs>` lookup path, which is a channel by another name.
//!
//! A path (`/build/src`, `.`) is fine: its own `flake.lock` pins its inputs. So is
//! `--file`, which names a file rather than a flake. `--override-input` replaces
//! one input of a locked flake, so its reference is held to the same rule.
//!
//! # Network, and `--offline`
//!
//! Evaluating a flake can fetch its inputs and building one substitutes from
//! cache.nixos.org, so a verdict normally carries [`Capability::Network`], as
//! pm's own table gives it to `cargo`. The exception is a `nix` command that
//! says `--offline`: that one gets no network, so the jail holds the recipe to
//! what it claimed. `--offline` on its own only turns substituters off and trusts
//! whatever was already downloaded; it is pm's jail that turns "should not fetch"
//! into "cannot fetch". The older `nix-*` commands have no such switch and always
//! get the network. `nix hash` and `nix nar` never do: they read a file.
//!
//! Network in a pm build file is per *file*, not per step (C8), so one step that
//! gets it puts every step of that file on the host network. A build file meant
//! to stay offline has to say `--offline` on every step, and `pm explain` shows
//! whether it did: the fingerprint of an offline step ends in `-offline`.
//!
//! # What this can reach
//!
//! Nothing. The component imports one function, `log`, which returns nothing. pm
//! hands over a command string and takes an answer back.

#![allow(clippy::same_length_and_capacity)] // wit-bindgen output

wit_bindgen::generate!({ path: "../wit", world: "plugin" });

use pm::plugin::{
    host::{Level, log},
    types::{Capability, Hook, Permission, Symbol},
};

struct LososNix;

/// What this plugin concluded about one command.
#[derive(Debug, PartialEq)]
enum Decision {
    /// Recognised and classified.
    Classify { fingerprint: String, network: bool },
    /// Recognised, and deliberately given no verdict. The reason goes in the log.
    Refuse(&'static str),
    /// Not a command this plugin knows.
    Unknown,
}

/// `nix` subcommands that read or build and never reach the network: they read a
/// file or a NAR the step already has.
const LOCAL: &[&str] = &["hash", "nar"];

/// `nix` subcommands that read or build and may fetch while doing so.
const FETCHING: &[&str] = &["build", "eval", "path-info", "log", "why-depends"];

/// `nix` subcommands that group further subcommands, and the ones of those that
/// read or build.
const GROUPS: &[(&str, &[&str])] = &[
    ("flake", &["check", "show", "metadata", "info"]),
    ("derivation", &["show"]),
    (
        "store",
        &[
            "ls",
            "cat",
            "dump-path",
            "verify",
            "diff-closures",
            "path-from-hash-part",
        ],
    ),
];

/// Why a machine-changing command gets no verdict.
const CHANGES_THE_MACHINE: &str = "changes a machine rather than building something; \
     this OS updates by systemd-sysupdate, never by activating a new generation";

/// Why a command that runs something else gets no verdict.
const RUNS_SOMETHING_ELSE: &str =
    "runs a program the step does not name, and a jail sized for nix is not sized for that";

/// Why a command that writes to or serves another store gets no verdict.
const OTHER_STORE: &str = "writes to or serves a store other than the build's own";

/// Why a command that moves an input past its pin gets no verdict.
const UNPINNED: &str = "fetches something newer than what is pinned; a pin moves by hand";

/// Why a flake reference that can move gets no verdict.
const UNLOCKED: &str = "names a flake by something that can move (a registry name, a branch, \
     a URL without narHash=); name a commit, or use this flake's pinned nixpkgs: .#<package>";

/// `nix` subcommands recognised and refused.
const REFUSED: &[(&str, &str)] = &[
    ("profile", CHANGES_THE_MACHINE),
    ("upgrade-nix", CHANGES_THE_MACHINE),
    ("registry", CHANGES_THE_MACHINE),
    ("daemon", OTHER_STORE),
    ("copy", OTHER_STORE),
    ("run", RUNS_SOMETHING_ELSE),
    ("shell", RUNS_SOMETHING_ELSE),
    ("develop", RUNS_SOMETHING_ELSE),
    ("fmt", RUNS_SOMETHING_ELSE),
    ("bundle", RUNS_SOMETHING_ELSE),
    ("repl", RUNS_SOMETHING_ELSE),
    ("env", RUNS_SOMETHING_ELSE),
];

/// `nix flake` subcommands recognised and refused.
const REFUSED_FLAKE: &[&str] = &["update", "lock", "prefetch", "clone"];

/// Programs other than `nix` recognised and refused.
const REFUSED_PROGRAMS: &[(&str, &str)] = &[
    ("nixos-rebuild", CHANGES_THE_MACHINE),
    ("nixos-install", CHANGES_THE_MACHINE),
    ("nixos-enter", RUNS_SOMETHING_ELSE),
    ("nix-env", CHANGES_THE_MACHINE),
    ("nix-collect-garbage", CHANGES_THE_MACHINE),
    ("nix-shell", RUNS_SOMETHING_ELSE),
    ("nix-daemon", OTHER_STORE),
    ("nix-copy-closure", OTHER_STORE),
    ("nix-channel", UNPINNED),
    ("nix-prefetch-url", UNPINNED),
];

/// `nix-store` operations that only query or realise the build's own store.
const STORE_OPERATIONS: &[&str] = &[
    "--realise",
    "-r",
    "--query",
    "-q",
    "--dump",
    "--export",
    "--verify-path",
    "--read-log",
    "-l",
    "--print-env",
];

/// Of [`STORE_OPERATIONS`], the ones that may substitute.
const STORE_FETCHING: &[&str] = &["--realise", "-r"];

/// Options that may come before a subcommand and take values, and how many.
///
/// Without these, `nix --store /build/nix build` would read `/build/nix` as the
/// subcommand. An option missing from here fails safe: its value is taken for
/// the subcommand, matches nothing, and the command gets no verdict.
const VALUED_OPTIONS: &[(&str, usize)] = &[
    ("--option", 2),
    ("--arg", 2),
    ("--argstr", 2),
    ("--override-input", 2),
    ("--store", 1),
    ("--eval-store", 1),
    ("--experimental-features", 1),
    ("--extra-experimental-features", 1),
    ("--log-format", 1),
    ("--max-jobs", 1),
    ("-j", 1),
    ("--cores", 1),
    ("--system", 1),
    ("-I", 1),
    ("--include", 1),
    ("--file", 1),
    ("-f", 1),
    ("--expr", 1),
    ("--out-link", 1),
    ("-o", 1),
    ("--profile", 1),
];

/// Paths NixOS fixes, for a build file that installs something referring to them.
///
/// Each is where every NixOS system puts it, not a choice this distribution made:
/// a path a build chooses stays in its build file. Only the store is visible
/// inside pm's build jail (pm mounts it read-only when it exists); the rest are run-time
/// paths, for a launcher or a unit a package installs.
const SYMBOLS: &[(&str, &str, &str)] = &[
    (
        "store",
        "/nix/store",
        "the Nix store; on this OS, the dm-verity /usr partition",
    ),
    (
        "current-system",
        "/run/current-system",
        "the running NixOS system's closure",
    ),
    (
        "booted-system",
        "/run/booted-system",
        "the NixOS system the machine booted, which an update does not move",
    ),
    (
        "system-bin",
        "/run/current-system/sw/bin",
        "where the running system's programs are on PATH",
    ),
];

/// The program name of a command, with any leading path removed.
///
/// pm allows an optional leading path in its own patterns (`/bin/sh`,
/// `./configure`), and a Nix-provisioned `nix` is usually spelled as a store
/// path, so a bare-name match would miss the commonest spelling.
fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// `args` split into the words that are operands and the options that stand
/// on their own, with every option's values consumed.
///
/// A value is neither: in `nix eval --argstr mode --offline`, `--offline` is the
/// string a Nix expression receives, not the switch. After `--` every word is
/// an operand.
struct Parsed<'a> {
    operands: Vec<&'a str>,
    flags: Vec<&'a str>,
}

fn parse<'a>(args: &[&'a str]) -> Parsed<'a> {
    let mut parsed = Parsed {
        operands: Vec::new(),
        flags: Vec::new(),
    };
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if *word == "--" {
            parsed.operands.extend(words.by_ref());
            break;
        }
        if !word.starts_with('-') {
            parsed.operands.push(*word);
            continue;
        }
        // `--store=/build/nix` carries its value in the same word.
        if word.contains('=') {
            parsed.flags.push(*word);
            continue;
        }
        let takes = VALUED_OPTIONS
            .iter()
            .find(|(name, _)| name == word)
            .map_or(0, |(_, n)| *n);
        if takes == 0 {
            parsed.flags.push(*word);
        }
        for _ in 0..takes {
            words.next();
        }
    }
    parsed
}

/// Whether a flake reference names content rather than a name for content.
///
/// The part after `#` is an attribute path and pins nothing either way, so
/// only what comes before it is read.
fn locked(reference: &str) -> bool {
    let flake = reference.split('#').next().unwrap_or(reference);
    let (base, query) = flake.split_once('?').unwrap_or((flake, ""));
    let has = |key: &str| {
        query
            .split('&')
            .any(|pair| pair.strip_prefix(key).is_some_and(|v| v.starts_with('=')))
    };
    let commit = |word: &str| word.len() == 40 && word.bytes().all(|b| b.is_ascii_hexdigit());

    // A path: whatever is there, its own flake.lock pins its inputs.
    if base.starts_with('/') || base.starts_with('.') {
        return true;
    }
    let Some((scheme, rest)) = base.split_once(':') else {
        // No scheme and not a path: a registry name, `nixpkgs/<branch>` included.
        return false;
    };
    match scheme {
        "path" | "git+file" | "file" => true,
        "github" | "gitlab" | "sourcehut" => {
            rest.split('/').nth(2).is_some_and(commit) || has("rev")
        }
        "git+https" | "git+ssh" | "git+http" | "git" | "hg+https" | "hg+ssh" | "hg+http" => {
            has("rev")
        }
        "https" | "http" | "tarball+https" | "tarball+http" | "tarball+file" => has("narHash"),
        // `flake:`, and anything this does not know, fails safe.
        _ => false,
    }
}

/// The value of every `--override-input <name> <reference>` in `args`.
fn overrides<'a>(args: &[&'a str]) -> impl Iterator<Item = &'a str> {
    args.windows(3)
        .filter(|w| w[0] == "--override-input")
        .map(|w| w[2])
}

/// Classify the new `nix` command line.
fn decide_nix(args: &[&str]) -> Decision {
    let Parsed { operands, flags } = parse(args);
    let Some(&subcommand) = operands.first() else {
        return Decision::Unknown;
    };
    let offline = flags.contains(&"--offline");

    if let Some((_, reason)) = REFUSED.iter().find(|(name, _)| *name == subcommand) {
        return Decision::Refuse(reason);
    }

    let fingerprint = if LOCAL.contains(&subcommand) {
        return Decision::Classify {
            fingerprint: subcommand.into(),
            network: false,
        };
    } else if FETCHING.contains(&subcommand) {
        subcommand.to_string()
    } else if let Some((_, allowed)) = GROUPS.iter().find(|(name, _)| *name == subcommand) {
        let Some(&inner) = operands.get(1) else {
            return Decision::Unknown;
        };
        if subcommand == "flake" && REFUSED_FLAKE.contains(&inner) {
            return Decision::Refuse(UNPINNED);
        }
        if !allowed.contains(&inner) {
            return Decision::Unknown;
        }
        format!("{subcommand}-{inner}")
    } else {
        return Decision::Unknown;
    };

    // What the subcommand operates on: flake references, unless `--file` or
    // `--expr` made them attribute paths. The store subcommands take store
    // paths, which `locked` reads as paths.
    let skip = if GROUPS.iter().any(|(name, _)| *name == subcommand) {
        2
    } else {
        1
    };
    let attributes = args
        .iter()
        .any(|a| matches!(*a, "--file" | "-f" | "--expr") || a.starts_with("--file="));
    let installables = if attributes {
        &[][..]
    } else {
        &operands[skip..]
    };
    if args.iter().any(|a| a.starts_with('<'))
        || installables
            .iter()
            .copied()
            .chain(overrides(args))
            .any(|r| !locked(r))
    {
        return Decision::Refuse(UNLOCKED);
    }

    if offline {
        Decision::Classify {
            fingerprint: format!("{fingerprint}-offline"),
            network: false,
        }
    } else {
        Decision::Classify {
            fingerprint,
            network: true,
        }
    }
}

/// Classify `nix-store`, whose operation is its first word after the program.
fn decide_nix_store(args: &[&str]) -> Decision {
    let Some(&operation) = args.first() else {
        return Decision::Unknown;
    };
    if !STORE_OPERATIONS.contains(&operation) {
        return if operation.starts_with('-') {
            // `--gc`, `--delete`, `--serve`, `--import`, `--optimise`, ...: every
            // other operation changes or serves the store.
            Decision::Refuse(OTHER_STORE)
        } else {
            Decision::Unknown
        };
    }
    Decision::Classify {
        fingerprint: "nix-store".into(),
        network: STORE_FETCHING.contains(&operation),
    }
}

/// What this plugin makes of `command`. Pure, so it can be tested off wasm.
fn decide(command: &str) -> Decision {
    let words: Vec<&str> = command.split_whitespace().collect();
    let Some((&first, args)) = words.split_first() else {
        return Decision::Unknown;
    };
    let program = program(first);

    if let Some((_, reason)) = REFUSED_PROGRAMS.iter().find(|(name, _)| *name == program) {
        return Decision::Refuse(reason);
    }

    match program {
        "nix" => decide_nix(args),
        "nix-store" => decide_nix_store(args),
        // Evaluation alone can fetch (`fetchTarball`, flake inputs), and neither
        // has an `--offline` that stops it.
        // `<nixpkgs>` is NIX_PATH's channel, which moves on its own.
        "nix-build" | "nix-instantiate" if args.iter().any(|a| a.starts_with('<')) => {
            Decision::Refuse(UNLOCKED)
        }
        "nix-build" | "nix-instantiate" => Decision::Classify {
            fingerprint: program.into(),
            network: true,
        },
        "nix-hash" => Decision::Classify {
            fingerprint: program.into(),
            network: false,
        },
        _ => Decision::Unknown,
    }
}

impl Guest for LososNix {
    fn describe() -> Manifest {
        Manifest {
            name: "losos-nix".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            summary: "Classifies the Nix commands that read or build a flake".into(),
            // classify-command only. A .nix file does say what a package needs,
            // but only once evaluated, and a plugin has no evaluator; reading it
            // as text would be guessing, which is what losos-systemd exists to
            // improve on.
            hooks: vec![Hook::ClassifyCommand],
            // Network is in the ceiling because a flake build without offline
            // inputs needs it; Shell and VersionControl are not. Nix runs its
            // builders' shells inside its own sandbox, and reads a flake in a git
            // checkout through libgit2 rather than a git process.
            grants_at_most: vec![
                Capability::Toolchain,
                Capability::Coreutils,
                Capability::Archive,
                Capability::Network,
            ],
            source_extensions: vec![],
            symbols: SYMBOLS
                .iter()
                .map(|(name, value, summary)| Symbol {
                    name: (*name).into(),
                    value: (*value).into(),
                    summary: (*summary).into(),
                })
                .collect(),
        }
    }

    fn classify_command(command: String) -> Option<Verdict> {
        match decide(&command) {
            Decision::Unknown => None,
            Decision::Refuse(reason) => {
                let program = command.split_whitespace().next().map(program)?;
                log(
                    Level::Info,
                    &format!("not classifying `{program}`: it {reason}"),
                );
                None
            }
            Decision::Classify {
                fingerprint,
                network,
            } => {
                // Archive because Nix unpacks what it fetches and packs NARs itself.
                let mut capabilities = vec![
                    Capability::Toolchain,
                    Capability::Coreutils,
                    Capability::Archive,
                ];
                if network {
                    capabilities.push(Capability::Network);
                }
                log(
                    Level::Debug,
                    &format!("classified `{command}` as {fingerprint}"),
                );
                Some(Verdict {
                    fingerprint,
                    capabilities,
                })
            }
        }
    }

    /// Not implemented, but the component model has no optional export, so it
    /// exists and returns the answer that changes nothing.
    fn scan_source(_file: SourceFile) -> Vec<Grant> {
        Vec::new()
    }
}

export!(LososNix);

#[allow(dead_code)]
fn _permission_is_used_by_the_world(_p: Permission) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn classified(fingerprint: &str, network: bool) -> Decision {
        Decision::Classify {
            fingerprint: fingerprint.into(),
            network,
        }
    }

    #[test]
    fn a_flake_build_gets_the_network_unless_it_says_offline() {
        assert_eq!(decide("nix build .#image"), classified("build", true));
        assert_eq!(
            decide("nix build --offline .#image"),
            classified("build-offline", false)
        );
    }

    #[test]
    fn only_the_switch_is_offline_not_a_value_or_an_operand() {
        assert_eq!(
            decide("nix eval --argstr mode --offline .#x"),
            classified("eval", true)
        );
        // After `--` it is an installable, and a name that resolves through
        // the registry at that.
        assert_eq!(
            decide("nix build .#x -- --offline"),
            Decision::Refuse(UNLOCKED)
        );
        assert_eq!(
            decide("nix eval --offline --argstr mode x .#x"),
            classified("eval-offline", false)
        );
    }

    #[test]
    fn a_store_path_program_and_leading_options_are_understood() {
        assert_eq!(
            decide(
                "/nix/store/abc-nix-2.31/bin/nix --store /build/nix --option sandbox false flake check"
            ),
            classified("flake-check", true)
        );
        assert_eq!(
            decide("nix --extra-experimental-features=flakes eval .#x"),
            classified("eval", true)
        );
    }

    #[test]
    fn an_unknown_option_value_fails_safe() {
        assert_eq!(decide("nix --builders ssh://x build"), Decision::Unknown);
    }

    #[test]
    fn reading_a_file_never_gets_the_network() {
        assert_eq!(decide("nix hash file x.tar"), classified("hash", false));
        assert_eq!(
            decide("nix-hash --type sha256 ."),
            classified("nix-hash", false)
        );
        assert_eq!(
            decide("nix-store --query --references /nix/store/a"),
            classified("nix-store", false)
        );
        assert_eq!(
            decide("nix-store -r /nix/store/a.drv"),
            classified("nix-store", true)
        );
    }

    #[test]
    fn machine_changes_runners_and_unpinned_fetches_are_refused() {
        assert_eq!(
            decide("nixos-rebuild switch --flake ."),
            Decision::Refuse(CHANGES_THE_MACHINE)
        );
        assert_eq!(
            decide("nix profile install .#pm"),
            Decision::Refuse(CHANGES_THE_MACHINE)
        );
        assert_eq!(
            decide("nix run .#pm"),
            Decision::Refuse(RUNS_SOMETHING_ELSE)
        );
        assert_eq!(
            decide("nix copy --to s3://x .#image"),
            Decision::Refuse(OTHER_STORE)
        );
        assert_eq!(decide("nix-store --gc"), Decision::Refuse(OTHER_STORE));
        assert_eq!(decide("nix flake update"), Decision::Refuse(UNPINNED));
        assert_eq!(
            decide("nix-prefetch-url https://x"),
            Decision::Refuse(UNPINNED)
        );
    }

    #[test]
    fn nixpkgs_through_this_flake_or_a_commit_is_classified() {
        assert_eq!(
            decide("nix --store /build/nix build /build/src#pm-payloads.ripgrep"),
            classified("build", true)
        );
        assert_eq!(
            decide("nix build github:NixOS/nixpkgs/0123456789abcdef0123456789abcdef01234567#hello"),
            classified("build", true)
        );
        assert_eq!(
            decide(
                "nix eval git+https://example.org/x?rev=0123456789abcdef0123456789abcdef01234567#v"
            ),
            classified("eval", true)
        );
        assert_eq!(
            decide("nix build -f /build/src/default.nix hello"),
            classified("build", true)
        );
        assert_eq!(
            decide("nix path-info --recursive /nix/store/abc-hello"),
            classified("path-info", true)
        );
        assert_eq!(
            decide("nix flake show --offline /build/src"),
            classified("flake-show-offline", false)
        );
    }

    #[test]
    fn nixpkgs_by_a_name_that_can_move_is_refused() {
        for command in [
            "nix build nixpkgs#hello",
            "nix build flake:nixpkgs#hello",
            "nix build nixpkgs/nixos-26.05#hello",
            "nix build github:NixOS/nixpkgs#hello",
            "nix build github:NixOS/nixpkgs/nixos-26.05#hello",
            "nix eval https://channels.nixos.org/nixos-26.05/nixexprs.tar.xz#hello.version",
            "nix flake check github:dasmatus/losos-desktop",
            "nix build --override-input nixpkgs nixpkgs /build/src#image",
            "nix build -f <nixpkgs> hello",
            "nix-build <nixpkgs> -A hello",
            "nix-instantiate --eval <nixpkgs/lib>",
        ] {
            assert_eq!(decide(command), Decision::Refuse(UNLOCKED), "{command}");
        }
    }

    #[test]
    fn anything_else_is_left_to_pm() {
        assert_eq!(decide("nix"), Decision::Unknown);
        assert_eq!(decide("nix flake"), Decision::Unknown);
        assert_eq!(decide("nix store gc"), Decision::Unknown);
        assert_eq!(decide("nixfmt flake.nix"), Decision::Unknown);
    }
}
