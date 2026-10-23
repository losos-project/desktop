//! The install itself: systemd-repart lays out the disk, systemd-sysupdate
//! fills it.
//!
//! repart creates the ESP, with GRUB and its grubenv copied in, and
//! both slots' /usr partitions, empty and labelled `_empty`. That label is
//! what sysupdate looks for when it needs a partition to write a version
//! into, so the install that follows is an ordinary update: sysupdate reads
//! SHA256SUMS from the channel, downloads the newest UKI and both /usr halves,
//! writes the halves into one slot, relabels them with the version, and puts
//! the UKI on the ESP. Everything else -- root, /home, swap -- is made by the
//! installed system's first boot, as it is for an image written with dd.
//!
//! Online, the installer pulls the release artifact for the ISO's version
//! directly from GHCR with oras. The artifact carries the signed manifest and
//! its files; the installer verifies them and lets sysupdate read the selected
//! files from a scratch partition on the target disk. If the ISO's version
//! has no artifact (for example, a branch build), it pulls the channel's
//! current artifact instead. No hosted proxy or GitHub release-asset download
//! is needed.
//!
//! With a release disk attached, the same transfers read from it instead: their sources become local files, and the files are checked
//! against the release's signed SHA256SUMS here first, since sysupdate only
//! verifies signatures on what it downloads. That is how an install works
//! where the channel cannot be reached, such as a VM whose network
//! re-signs TLS with a certificate authority the installer does not know.
//!
//! sysupdate's partition targets name the disk they write to, which the OS
//! cannot know when it writes the transfers, so they say [`TARGET`] and are
//! rendered for the chosen disk here. The UKI's target is the ESP's mount
//! point, which the OS does know, because it passes the same path to this
//! program.
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use crate::github;
use crate::stage::{self, Scratch};

/// Stands for the target disk's device node in the transfer templates.
pub const TARGET: &str = "@TARGET@";

#[derive(Debug, Clone)]
pub struct Plan {
    pub disk: PathBuf,
    pub repart_definitions: PathBuf,
    pub sysupdate_templates: PathBuf,
    pub esp: PathBuf,
    pub work: PathBuf,
    /// The channel's URL, as the transfers' sources name it.
    pub source: Option<String>,
    /// The channel's index on the documentation site, when the OS updates
    /// from GitHub Releases through it (update-source.nix): which release
    /// assets make up each file, so the installer can fetch them itself.
    pub index: Option<String>,
    /// The GitHub release to install from, when the ISO knows its own.
    pub release: Option<GitHub>,
    /// A directory holding a release (its SHA256SUMS, SHA256SUMS.gpg and the
    /// files they list), used in place of the channel when [`has_release`]
    /// says one is there.
    pub local: Option<PathBuf>,
    /// A program that makes the disk start on a legacy BIOS PC, run with
    /// the disk as its argument once the system is on it. The OS passes
    /// its GRUB installer here on x86_64 (bios.nix); UEFI firmware ignores
    /// what it writes.
    pub bios_boot: Option<PathBuf>,
}

/// Which GitHub release an online install reads ([`crate::github`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHub {
    /// `owner/name`.
    pub repo: String,
    /// The cadence the ISO was built for, `losos.channel`.
    pub channel: String,
    /// The version the ISO was built as, `losos.version`.
    pub version: String,
    /// The architecture's name in release file names.
    pub arch: String,
}

/// Where the release signing key is, as sysupdate itself reads it.
const PUBRING: &str = "/etc/systemd/import-pubring.pgp";

/// Whether `dir` holds a release to install from.
pub fn has_release(dir: &Path) -> bool {
    dir.join("SHA256SUMS").is_file()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Partition,
    Mount,
    Download,
    Finish,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Partition, Step::Mount, Step::Download, Step::Finish];

    /// What the installer's page says while this step runs.
    pub fn title(self) -> &'static str {
        match self {
            Step::Partition => "Partitioning the disk",
            Step::Mount => "Mounting the new boot partition",
            Step::Download => "Downloading and writing the system",
            Step::Finish => "Finishing",
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Step(Step),
    Line(String),
    Finished(Result<(), String>),
}

/// Runs the whole install, reporting as it goes. Always ends with
/// [`Event::Finished`].
pub fn run(plan: &Plan, events: &Sender<Event>) {
    let result = install(plan, events);
    let _ = events.send(Event::Finished(result));
}

fn install(plan: &Plan, events: &Sender<Event>) -> Result<(), String> {
    let step = |s| {
        let _ = events.send(Event::Step(s));
    };

    step(Step::Partition);
    let disk = plan.disk.to_string_lossy().into_owned();
    forget_partitions(&disk, events)?;
    let definitions = format!("--definitions={}", plan.repart_definitions.display());
    // --empty=force writes a fresh partition table whatever was there; the
    // person typed `erase` to get here.
    let table = command(
        "systemd-repart",
        &[
            "--dry-run=no",
            "--empty=force",
            &definitions,
            "--json=short",
            &disk,
        ],
        true,
        events,
    )?;
    let esp = esp_node(&table)?;

    step(Step::Mount);
    // The kernel has the new table; udev still has to make the node.
    command("udevadm", &["settle"], false, events)?;
    wait_for(&esp, Duration::from_secs(10))?;
    fs::create_dir_all(&plan.esp).map_err(|e| format!("{}: {e}", plan.esp.display()))?;
    let esp_text = esp.to_string_lossy().into_owned();
    let mount_point = plan.esp.to_string_lossy().into_owned();
    command(
        "mount",
        &["-t", "vfat", "-o", "umask=0077", &esp_text, &mount_point],
        false,
        events,
    )?;

    step(Step::Download);
    let downloaded = download(plan, &disk, events);
    if downloaded.is_ok() {
        step(Step::Finish);
    }
    // Unmount even when the download failed, so the next attempt can
    // repartition the disk.
    let unmounted = command("umount", &[&mount_point], false, events).map(|_| ());
    match (downloaded, unmounted) {
        (Ok(()), Ok(())) => {}
        (Err(download), Ok(())) => return Err(download),
        (Ok(()), Err(unmount)) => return Err(unmount),
        (Err(download), Err(unmount)) => {
            return Err(format!("{download}; cleanup also failed: {unmount}"));
        }
    }
    command("sync", &[], false, events)?;
    Ok(())
}

/// Erases `disk`'s partition table and has the kernel drop the partitions it
/// knew from it, before repart writes the new one.
///
/// repart formats the ESP through a partition device it adds to the kernel
/// itself, and that fails with "Failed to create new partition '/dev/vda2':
/// Device or resource busy" whenever the kernel still has a partition 2 from
/// the table that was there: any disk with a table on it, not only one left
/// by a failed install. Asking the kernel to reread the table first also
/// stops an install, before anything is written, when a partition on the
/// disk is mounted or otherwise open, since the kernel then refuses.
fn forget_partitions(disk: &str, events: &Sender<Event>) -> Result<(), String> {
    let in_use = |e: String| format!("{disk} has a partition in use, so it cannot be erased: {e}");
    command("blockdev", &["--rereadpt", disk], false, events).map_err(in_use)?;
    command("wipefs", &["--all", "--force", disk], false, events)?;
    command("blockdev", &["--rereadpt", disk], false, events).map_err(in_use)?;
    command("udevadm", &["settle"], false, events)?;
    Ok(())
}

fn download(plan: &Plan, disk: &str, events: &Sender<Event>) -> Result<(), String> {
    let linux = plan.esp.join("EFI/Linux");
    fs::create_dir_all(&linux).map_err(|e| format!("{}: {e}", linux.display()))?;
    let transfers = plan.work.join("sysupdate.d");
    match (&plan.source, &plan.local, &plan.release) {
        (Some(url), Some(dir), _) if has_release(dir) => {
            let _ = events.send(Event::Line(format!(
                "Installing from the release in {} instead of {url}",
                dir.display()
            )));
            verify_release(dir, Path::new(PUBRING), &plan.work, events)?;
            render_transfers(
                &plan.sysupdate_templates,
                &transfers,
                disk,
                Source::Disk(Local { url, dir }),
            )?;
            sysupdate(plan, events)?;
        }
        (Some(url), _, Some(release)) => {
            let image = format!(
                "ghcr.io/{}/images:{}-{}-{}",
                release.repo, release.channel, release.version, release.arch
            );
            let current = format!(
                "ghcr.io/{}/images:{}-{}",
                release.repo, release.channel, release.arch
            );
            let exact = github::release_tag(&release.channel, &release.version)
                .map(|_| image.as_str());
            let (reference, manifest, version) = match exact {
                Some(reference) => match oci_manifest(reference, events) {
                    Ok(manifest) => (reference.to_owned(), manifest, Some(release.version.as_str())),
                    Err(e) => {
                        let _ = events.send(Event::Line(format!(
                            "No GHCR image for this ISO version ({e}); trying the current {} image",
                            release.channel
                        )));
                        (current.clone(), oci_manifest(&current, events)?, None)
                    }
                },
                None => {
                    let _ = events.send(Event::Line(format!(
                        "{} is not a published release version; pulling the current {} image",
                        release.version, release.channel
                    )));
                    (current.clone(), oci_manifest(&current, events)?, None)
                }
            };
            let files = oci_files(&manifest, &release.arch)?;
            let _ = events.send(Event::Line(format!("Installing directly from GHCR artifact {reference}")));
            online_oci(plan, disk, url, &reference, &files, version, events)?;
        }
        (Some(url), _, None) => channel(plan, disk, url, events)?,
        _ => {
            render_transfers(&plan.sysupdate_templates, &transfers, disk, Source::Channel)?;
            sysupdate(plan, events)?;
        }
    }
    // sysupdate exits 0 saying "No update needed" when no version has a
    // source for every transfer, which is how a release missing a file looks
    // (a release disk without x86_64's BIOS halves). The install then ended
    // in "Installed" over an empty ESP and two empty slots.
    if !has_uki(&linux) {
        return Err(format!(
            "systemd-sysupdate installed nothing: no release there has every file its transfers need ({} is empty)",
            linux.display()
        ));
    }
    // After sysupdate, so a disk whose download failed is left with no boot
    // code that would start GRUB on an empty ESP.
    if let Some(program) = &plan.bios_boot {
        let program = program.to_string_lossy().into_owned();
        command(&program, &[disk], false, events)?;
    }
    Ok(())
}

/// `systemd-sysupdate update` with the transfers rendered under `plan.work`.
fn sysupdate(plan: &Plan, events: &Sender<Event>) -> Result<(), String> {
    let definitions = format!("--definitions={}", plan.work.join("sysupdate.d").display());
    command(
        "systemd-sysupdate",
        &[&definitions, "update"],
        false,
        events,
    )
    .map(|_| ())
}

/// Fetches an OCI image manifest from GHCR. The artifact is public, so
/// oras can pull it without a token.
fn oci_manifest(reference: &str, events: &Sender<Event>) -> Result<serde_json::Value, String> {
    let text = command("oras", &["manifest", "fetch", reference], true, events)?;
    serde_json::from_str(&text).map_err(|e| format!("{reference}: invalid OCI manifest: {e}"))
}

/// Files in the release artifact, keyed by their original file names. ORAS
/// records each pushed file's name in the standard OCI title annotation.
fn oci_files(
    manifest: &serde_json::Value,
    arch: &str,
) -> Result<BTreeMap<String, github::File>, String> {
    let layers = manifest["layers"]
        .as_array()
        .ok_or_else(|| "GHCR image manifest has no layers".to_owned())?;
    let mut files = BTreeMap::new();
    for layer in layers {
        let name = layer["annotations"]["org.opencontainers.image.title"]
            .as_str()
            .ok_or_else(|| "GHCR image layer has no file-name annotation".to_owned())?;
        let path = Path::new(name);
        if path.components().count() != 1 || name == "." || name == ".." {
            return Err(format!("GHCR image contains an unsafe file name: {name}"));
        }
        let size = layer["size"]
            .as_u64()
            .ok_or_else(|| format!("GHCR image layer {name} has no size"))?;
        let logical_name = if name == format!("SHA256SUMS-{arch}") {
            "SHA256SUMS"
        } else if name == format!("SHA256SUMS-{arch}.gpg") {
            "SHA256SUMS.gpg"
        } else {
            name
        };
        files.insert(
            logical_name.to_owned(),
            github::File {
                // Store the OCI digest here; online_oci fetches each selected
                // layer directly, writing it under its sysupdate file name.
                urls: vec![layer["digest"]
                    .as_str()
                    .ok_or_else(|| format!("GHCR image layer {name} has no digest"))?
                    .to_owned()],
                size,
            },
        );
    }
    Ok(files)
}

/// Pull only the files sysupdate needs from an OCI release artifact. The
/// selected layers are staged on the target disk, verified against the
/// release's signed SHA256SUMS, and served to sysupdate as local files.
fn online_oci(
    plan: &Plan,
    disk: &str,
    url: &str,
    reference: &str,
    files: &BTreeMap<String, github::File>,
    version: Option<&str>,
    events: &Sender<Event>,
) -> Result<(), String> {
    let patterns = stage::source_patterns(&plan.sysupdate_templates)?;
    let wanted = stage::wanted(files, &patterns, version)?;
    let scratch = Scratch::create(disk, stage::scratch_size(&wanted), &plan.work, events)?;
    let installed = (|| {
        // A full `oras pull` would also download the ISO and disk image.
        // Fetch only the selected layer digests into their logical file names.
        let repository = reference
            .rsplit_once(':')
            .map(|(name, _)| name)
            .unwrap_or(reference);
        for (name, file) in &wanted {
            let digest = file
                .urls
                .first()
                .ok_or_else(|| format!("GHCR image layer {name} has no digest"))?;
            let output = scratch.mount.join(name).to_string_lossy().into_owned();
            let blob = format!("{repository}@{digest}");
            command("oras", &["blob", "fetch", "--output", &output, &blob], false, events)?;
        }
        verify_release(&scratch.mount, Path::new(PUBRING), &plan.work, events)?;
        render_transfers(
            &plan.sysupdate_templates,
            &plan.work.join("sysupdate.d"),
            disk,
            Source::Disk(Local {
                url,
                dir: &scratch.mount,
            }),
        )?;
        sysupdate(plan, events)
    })();
    let removed = scratch.remove(disk, events);
    match (installed, removed) {
        (Ok(()), removed) => removed,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(cleanup)) => Err(format!("{e}; cleanup also failed: {cleanup}")),
    }
}

/// The channel's newest release: fetched with aria2 from the assets the
/// index names when there is one, and otherwise read by sysupdate from the
/// channel's URL itself, as an update reads it.
fn channel(plan: &Plan, disk: &str, url: &str, events: &Sender<Event>) -> Result<(), String> {
    let Some(index) = &plan.index else {
        let transfers = plan.work.join("sysupdate.d");
        render_transfers(&plan.sysupdate_templates, &transfers, disk, Source::Channel)?;
        return sysupdate(plan, events);
    };
    let text = command(
        "curl",
        &[
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--retry",
            "3",
            index,
        ],
        true,
        events,
    )?;
    online(plan, disk, url, &stage::index_files(&text), None, events)
}

/// Installs `files` (the release or the channel's index, by the names
/// sysupdate asks for), `version`'s or the newest: aria2 fetches what the
/// transfers read into a scratch partition on `disk`, and sysupdate reads it
/// from [`github::Mirror`] in place of the channel at `url`.
fn online(
    plan: &Plan,
    disk: &str,
    url: &str,
    files: &BTreeMap<String, github::File>,
    version: Option<&str>,
    events: &Sender<Event>,
) -> Result<(), String> {
    let patterns = stage::source_patterns(&plan.sysupdate_templates)?;
    let wanted = stage::wanted(files, &patterns, version)?;
    let scratch = Scratch::create(disk, stage::scratch_size(&wanted), &plan.work, events)?;
    let installed = (|| {
        let staged = stage::fetch(&wanted, &scratch.mount, &plan.work, events)?;
        let mirror = github::Mirror::new(staged)
            .map_err(|e| format!("serving the release to sysupdate: {e}"))?;
        let local = mirror.url().map_err(|e| format!("{e}"))?;
        render_transfers(
            &plan.sysupdate_templates,
            &plan.work.join("sysupdate.d"),
            disk,
            Source::Mirror {
                url,
                mirror: &local,
            },
        )?;
        mirror.serve(events, || sysupdate(plan, events))
    })();
    // Removed even when the install failed, though the next attempt erases
    // the disk anyway, so a disk left as it is has no scratch partition in
    // the space first boot gives root.
    let removed = scratch.remove(disk, events);
    match (installed, removed) {
        (Ok(()), removed) => removed,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(cleanup)) => Err(format!("{e}; cleanup also failed: {cleanup}")),
    }
}

/// Whether `dir` holds a UKI, without which the installed disk has nothing
/// to boot.
fn has_uki(dir: &Path) -> bool {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "efi"))
}

/// The ESP's device node, from repart's JSON summary of the table it wrote.
pub fn esp_node(json: &str) -> Result<PathBuf, String> {
    let table: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("systemd-repart's output: {e}"))?;
    table
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["type"] == "esp")
        .and_then(|p| p["node"].as_str())
        .map(PathBuf::from)
        .ok_or_else(|| "systemd-repart reported no ESP".to_owned())
}

/// A release directory standing in for the channel at `url`.
#[derive(Debug, Clone, Copy)]
pub struct Local<'a> {
    pub url: &'a str,
    pub dir: &'a Path,
}

/// Where the rendered transfers read from.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    /// The channel, as the OS wrote them.
    Channel,
    /// A release disk's directory.
    Disk(Local<'a>),
    /// The channel's files at another URL, the release served by
    /// [`crate::github::Mirror`].
    Mirror { url: &'a str, mirror: &'a str },
}

/// Copies every transfer in `templates` into `out`, naming `disk` as the
/// partition target, and reading from `source`.
pub fn render_transfers(
    templates: &Path,
    out: &Path,
    disk: &str,
    source: Source,
) -> Result<(), String> {
    let fail = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    if out.exists() {
        fs::remove_dir_all(out).map_err(|e| fail(out, e))?;
    }
    fs::create_dir_all(out).map_err(|e| fail(out, e))?;
    let mut entries: Vec<_> = fs::read_dir(templates)
        .map_err(|e| fail(templates, e))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "transfer"))
        .collect();
    entries.sort();
    if entries.is_empty() {
        return Err(format!("{}: no transfers", templates.display()));
    }
    for template in entries {
        let text = fs::read_to_string(&template).map_err(|e| fail(&template, e))?;
        let text = match source {
            Source::Channel => text,
            Source::Disk(local) => localise(&text, local),
            Source::Mirror { url, mirror } => relocate(&text, url, mirror),
        };
        let target = out.join(template.file_name().unwrap_or_default());
        fs::write(&target, text.replace(TARGET, disk)).map_err(|e| fail(&target, e))?;
    }
    Ok(())
}

/// A transfer whose source is the channel, turned into one whose source is
/// the same files in a directory. Only the source's own two lines change:
/// the patterns stay, so sysupdate picks the newest version there exactly as
/// it would from the channel, and `Verify=` stays too, though sysupdate only
/// acts on it for downloads ([`verify_release`] stands in for it).
fn localise(text: &str, local: Local) -> String {
    let path = format!("Path={}", local.url);
    text.lines()
        .map(|line| match line.trim() {
            "Type=url-file" => "Type=regular-file".to_owned(),
            trimmed if trimmed == path => format!("Path={}", local.dir.display()),
            _ => line.to_owned(),
        })
        .map(|line| line + "\n")
        .collect()
}

/// A transfer whose source is the channel, reading the same files from
/// another URL. Only `Path=` changes: it is still a download, so sysupdate
/// checks `SHA256SUMS.gpg` and every hash itself.
fn relocate(text: &str, url: &str, mirror: &str) -> String {
    let path = format!("Path={url}");
    text.lines()
        .map(|line| match line.trim() {
            trimmed if trimmed == path => format!("Path={mirror}"),
            _ => line.to_owned(),
        })
        .map(|line| line + "\n")
        .collect()
}

/// Checks a release directory as sysupdate checks a download: SHA256SUMS
/// signed by the release key, and then every file beside it listed there
/// with the hash it has. A file it does not list is refused rather than
/// skipped, because sysupdate would install it all the same if its name
/// matched. Without a key on the system, as without one sysupdate verifies
/// nothing, only the hashes are checked.
pub fn verify_release(
    dir: &Path,
    pubring: &Path,
    work: &Path,
    events: &Sender<Event>,
) -> Result<(), String> {
    let sums = dir.join("SHA256SUMS");
    let sums_text = sums.to_string_lossy().into_owned();
    if pubring.exists() {
        let home = work.join("gnupg");
        fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
        let signature = dir.join("SHA256SUMS.gpg").to_string_lossy().into_owned();
        // The options systemd-pull passes: only the given keyring, and any
        // key in it is trusted, since being in it is what trust means here.
        command(
            "gpg",
            &[
                "--homedir",
                &home.to_string_lossy(),
                "--no-options",
                "--no-default-keyring",
                "--no-auto-key-locate",
                "--no-auto-check-trustdb",
                "--batch",
                "--trust-model=always",
                &format!("--keyring={}", pubring.display()),
                "--verify",
                &signature,
                &sums_text,
            ],
            false,
            events,
        )?;
    } else {
        let _ = events.send(Event::Line(format!(
            "{} is missing, so the release's signature is not checked",
            pubring.display()
        )));
    }

    let listed = parse_sums(&fs::read_to_string(&sums).map_err(|e| format!("{sums_text}: {e}"))?);
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != "SHA256SUMS" && name != "SHA256SUMS.gpg")
        .collect();
    names.sort();
    for name in names {
        let Some(expected) = listed.iter().find(|(_, n)| *n == name).map(|(h, _)| h) else {
            return Err(format!("{name} is not in the release's SHA256SUMS"));
        };
        let path = dir.join(&name).to_string_lossy().into_owned();
        let out = command("sha256sum", &[&path], true, events)?;
        let actual = out.split_whitespace().next().unwrap_or_default();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("{name} does not match the release's SHA256SUMS"));
        }
        let _ = events.send(Event::Line(format!("{name}: OK")));
    }
    Ok(())
}

/// The (hash, file name) pairs of a SHA256SUMS file, in either of
/// sha256sum's text (`hash  name`) or binary (`hash *name`) forms.
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.split_once(' ')?;
            let name = name.strip_prefix([' ', '*']).unwrap_or(name);
            (hash.len() == 64 && !name.is_empty()).then(|| (hash.to_owned(), name.to_owned()))
        })
        .collect()
}

fn wait_for(path: &Path, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    while !path.exists() {
        if start.elapsed() > timeout {
            return Err(format!("{} did not appear", path.display()));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Runs `program`, sending each line it logs to the installer. With `capture`,
/// standard output is returned rather than shown.
pub(crate) fn command(
    program: &str,
    args: &[&str],
    capture: bool,
    events: &Sender<Event>,
) -> Result<String, String> {
    let _ = events.send(Event::Line(format!("$ {program} {}", args.join(" "))));
    let mut child = Command::new(program)
        .args(args)
        // Plain text: the installer's details are not a terminal and show
        // escapes as-is.
        .env("SYSTEMD_COLORS", "0")
        .env("SYSTEMD_PAGER", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;

    let forward = |stream: Box<dyn Read + Send>| {
        let events = events.clone();
        thread::spawn(move || {
            // systemd-pull reports progress with carriage returns.
            for line in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                for part in String::from_utf8_lossy(&line).split('\r') {
                    if !part.trim().is_empty() {
                        let _ = events.send(Event::Line(part.trim_end().to_owned()));
                    }
                }
            }
        })
    };
    let stderr = forward(Box::new(child.stderr.take().expect("piped")));
    let mut captured = String::new();
    let stdout = child.stdout.take().expect("piped");
    let stdout_thread = if capture {
        let mut stdout = stdout;
        stdout
            .read_to_string(&mut captured)
            .map_err(|e| format!("{program}: {e}"))?;
        None
    } else {
        Some(forward(Box::new(stdout)))
    };

    let status = child.wait().map_err(|e| format!("{program}: {e}"))?;
    let _ = stderr.join();
    if let Some(t) = stdout_thread {
        let _ = t.join();
    }
    if status.success() {
        Ok(captured)
    } else {
        Err(format!("{program} failed ({status})"))
    }
}
