//! A pm plugin that teaches pm about image assembly and bundling.
//!
//! pm's built-in fingerprint table knows compilers, build systems, archivers and
//! coreutils. It does not know the tools that turn a staged tree into something
//! bootable, because those are not build systems -- so without this plugin a recipe
//! that calls `qemu-img` or `xorriso` is refused before a single step runs.
//!
//! This plugin names those tools. It also
//! names `patchelf`, which is the other half of a self-contained bundle: setting
//! `RUNPATH` to `$ORIGIN/../lib` is what lets a package carry its own libraries and
//! resolve them from inside itself rather than from whatever the host happens to
//! have installed.
//!
//! # What it deliberately does not do
//!
//! Every tool here is classified as needing [`Capability::Toolchain`] and
//! [`Capability::Coreutils`], and some also [`Capability::Archive`]. **None of them
//! gets [`Capability::Network`]**, and the manifest's ceiling says so, so pm would
//! drop it even if a future edit here asked for it by mistake. An image build that
//! reaches the network is an image build whose contents are not the ones that were
//! reviewed.
//!
//! Nor is any of this a shell. `mkfs.ext4` and friends are named individually rather
//! than by a `mkfs*` prefix so that a typo is an unrecognised command -- which pm
//! reports -- rather than a wildcard that quietly matches something else.
//!
//! # What this can reach
//!
//! Nothing. The component imports one function, `log`, which returns nothing. There is
//! no filesystem here, no clock, no network and no environment: pm hands over a command
//! string and takes an answer back.

#![allow(clippy::same_length_and_capacity)] // wit-bindgen output

wit_bindgen::generate!({ path: "../wit", world: "plugin" });

use pm::plugin::{
    host::{Level, log},
    types::{Capability, Hook, Permission},
};

struct LososImage;

/// pm-fingerprints
///
/// Tools that only read and write ordinary files in the workspace.
///
/// Filesystem builders are here rather than under anything more alarming because
/// that is genuinely all they do: `mkfs.ext4` on a regular file produces a regular
/// file. It is the *kernel* that makes a loop device interesting, and none of these
/// tools can ask pm for one.
const IMAGE_TOOLS: &[&str] = &[
    // Whole-image assembly.
    "qemu-img",
    "xorriso",
    "xorrisofs",
    "genisoimage",
    "mkisofs",
    // Filesystems, each named in full.
    "mkfs.vfat",
    "mkfs.fat",
    "mkfs.ext2",
    "mkfs.ext3",
    "mkfs.ext4",
    "mkfs.erofs",
    "mkfs.btrfs",
    "mksquashfs",
    "mkdosfs",
    "e2fsck",
    "resize2fs",
    "tune2fs",
    // mtools: populate a FAT ESP without root and without a loop device, which is
    // the only way to do it inside a build jail at all.
    "mcopy",
    "mmd",
    "mdir",
    "mformat",
    "mtype",
    // systemd's own image tooling.
    "systemd-repart",
    "ukify",
    "bootctl",
    "systemd-dissect",
    "systemd-measure",
    // Integrity and signing.
    "veritysetup",
    "cryptsetup",
    "sbsign",
    "sbverify",
    "pesign",
    // Bundling: the AppImage technique, which is what makes a package carry its own
    // libraries instead of borrowing the host's.
    "patchelf",
];

/// The tools that also need [`Capability::Archive`].
///
/// Declared separately rather than granting Archive to everything, so `pm explain`
/// shows the smaller set for the tools that do not need it.
const ARCHIVING: &[&str] = &[
    "xorriso",
    "xorrisofs",
    "genisoimage",
    "mkisofs",
    "mksquashfs",
    "systemd-dissect",
];

/// The program name of a command, with any leading path removed.
///
/// pm allows an optional leading path in its own patterns (`/bin/sh`, `./configure`),
/// so a plugin that only matched a bare name would behave differently from the
/// built-in table for no reason a user could predict.
fn program(command: &str) -> Option<&str> {
    let first = command.split_whitespace().next()?;
    Some(first.rsplit('/').next().unwrap_or(first))
}

/// A stable fingerprint name for a tool.
///
/// pm requires 1-32 characters of `[a-z0-9-]`, and records it as
/// `losos-image:<name>`. `mkfs.ext4` therefore becomes `mkfs-ext4`: the dot is not
/// in the permitted set, and a verdict pm cannot use is a verdict pm discards.
fn fingerprint_name(program: &str) -> String {
    program.replace('.', "-")
}

impl Guest for LososImage {
    fn describe() -> Manifest {
        Manifest {
            name: "losos-image".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            summary: "Classifies image assembly, filesystem and bundling tools".into(),
            // classify-command only. This plugin has nothing to say about source
            // files, and claiming the hook would mean pm handing it every file in
            // every tree for no reason.
            hooks: vec![Hook::ClassifyCommand],
            // The ceiling, and the point of it: no Network, no Shell, no
            // VersionControl, ever. Assembling an image is a pure function of the
            // tree that went in.
            grants_at_most: vec![
                Capability::Toolchain,
                Capability::Coreutils,
                Capability::Archive,
            ],
            source_extensions: vec![],
            // No symbols. The contract added these for the paths a build file
            // would otherwise hardcode -- where a distribution puts unit files,
            // where a sysext goes. This plugin classifies tools and knows no
            // such path, and a symbol invented here would be a constant nothing
            // in this tree agreed to.
            symbols: vec![],
        }
    }

    fn classify_command(command: String) -> Option<Verdict> {
        let program = program(&command)?;

        if !IMAGE_TOOLS.contains(&program) {
            // No verdict rather than a guess. pm's answer to "nothing matched" is a
            // diagnostic naming the command, which is a better outcome than a jail
            // sized by a plugin that was not sure either.
            return None;
        }

        let mut capabilities = vec![Capability::Toolchain, Capability::Coreutils];
        if ARCHIVING.contains(&program) {
            capabilities.push(Capability::Archive);
        }

        log(
            Level::Debug,
            &format!("classified `{program}` as an image tool"),
        );

        Some(Verdict {
            fingerprint: fingerprint_name(program),
            capabilities,
        })
    }

    /// Not implemented, but the component model has no optional export, so it exists
    /// and returns the answer that changes nothing.
    fn scan_source(_file: SourceFile) -> Vec<Grant> {
        Vec::new()
    }
}

export!(LososImage);

#[allow(dead_code)]
fn _permission_is_used_by_the_world(_p: Permission) {}
