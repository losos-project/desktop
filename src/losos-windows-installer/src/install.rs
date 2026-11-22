//! The install, once there is room for it: the same steps whether the disk
//! is the running Windows machine's (windows.rs) or an image file
//! (image.rs), with [`Target`] standing for what differs.
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use miette::{Result, WrapErr};

use crate::disk::{self, Device};
use crate::fat;
use crate::layout::Plan;
use crate::release::{Arch, Release};

/// Where GRUB goes on the ESP, beside Windows' own boot manager rather than
/// over the removable-media path, which is Windows' fallback. grub.nix puts
/// the same file at the same path on an ESP of LosOS's own.
pub fn loader_path(arch: &Arch) -> String {
    format!("EFI/losos/grub{}.efi", arch.efi)
}

/// GRUB's boot counter (grub.cfg), beside it. Limine does not read it.
/// It stays so an older boot path, and `--uninstall`, still find the file.
pub const GRUBENV: &str = "EFI/losos/grubenv";

/// Limine's menu beside the loader. The UKI is on the XBOOTLDR partition,
/// so the path names that partition's GUID rather than the ESP.
fn limine_conf(uki: &str, boot: &crate::guid::Guid) -> String {
    format!(
        "\
timeout: 3
serial: yes
wallpaper_style: centered
backdrop: 0a0e12
term_background: 800a0e12
term_foreground: e2e8ee
term_background_bright: 48b3c0
term_foreground_bright: 0a0e12
interface_branding: LosOS Desktop
interface_branding_colour: 48b3c0
interface_help_colour: 94a2b0

/{uki}
    comment: LosOS Desktop
    protocol: efi
    path: guid({boot}):/EFI/Linux/{uki}
"
    )
}

/// The ESP's record of the files this program put there, so `--uninstall`
/// removes those and nothing it found already there.
pub const MARKER: &str = "EFI/losos/losos-windows-installer.txt";

pub struct EspFile<'a> {
    pub path: String,
    pub bytes: &'a [u8],
    /// Whether a file already there is replaced or left as it is.
    pub replace: bool,
}

/// What differs between installing onto Windows' disk and onto an image.
pub trait Target {
    fn device(&mut self) -> &mut Device;
    /// Adds the plan's partitions to the partition table.
    fn add_partitions(&mut self, plan: &Plan) -> Result<()>;
    /// Writes files onto the ESP and returns the paths that were not there
    /// before.
    fn put_esp(&mut self, files: &[EspFile]) -> Result<Vec<String>>;
    /// Registers `loader` on the ESP with the firmware, first in the order.
    fn add_boot_entry(&mut self, description: &str, loader: &str) -> Result<()>;
    /// Takes back whatever the calls above did, as far as it can.
    fn undo(&mut self) -> Result<()>;
}

/// The release's files, checked and on local disk.
pub struct Files {
    pub uki: PathBuf,
    pub usr: PathBuf,
    pub verity: PathBuf,
}

pub struct Boot<'a> {
    /// GRUB, with its menu and theme inside.
    pub loader: &'a [u8],
    /// An empty grubenv.
    pub grubenv: &'a [u8],
}

/// Steps as the person sees them, and a progress percentage on one line.
#[derive(Default)]
pub struct Console {
    last: Option<Instant>,
    /// The percentage on screen. A decoder keeps reporting after its input
    /// is all read, and each of those would print 100% again.
    shown: Option<u64>,
}

impl Console {
    pub fn step(&mut self, title: &str) {
        if self.last.take().is_some() {
            println!();
        }
        self.shown = None;
        println!("==> {title}");
    }

    pub fn progress(&mut self, done: u64, total: u64) {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|t| now - t < Duration::from_millis(500))
            && done < total
        {
            return;
        }
        let percent = (done * 100).checked_div(total).unwrap_or(100).min(100);
        if self.shown == Some(percent) {
            return;
        }
        self.last = Some(now);
        self.shown = Some(percent);
        print!("\r    {percent:3}%");
        let _ = io::stdout().flush();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    target: &mut dyn Target,
    plan: &Plan,
    release: &Release,
    files: &Files,
    arch: &Arch,
    id: &str,
    boot: &Boot,
    report: &mut Console,
) -> Result<()> {
    report.step("Adding LosOS's partitions");
    target.add_partitions(plan)?;

    for (title, part, path) in [
        (
            "Writing the system's hash tree",
            &plan.verity,
            &files.verity,
        ),
        ("Writing the system", &plan.usr, &files.usr),
    ] {
        report.step(title);
        disk::write_xz(
            target.device(),
            part.start,
            part.size,
            path,
            |done, total| report.progress(done, total),
        )?;
    }

    report.step("Writing the boot partition");
    let uki = format!("EFI/Linux/{}", release.uki_name(id));
    fat::format(
        target.device(),
        plan.boot.start,
        plan.boot.size,
        *b"LOSOS BOOT ",
        &[(uki.as_str(), files.uki.as_path())],
    )?;

    report.step("Adding Limine to the EFI system partition");
    let loader = loader_path(arch);
    let conf = limine_conf(&release.uki_name(id), &plan.boot.uuid);
    let mut created = target.put_esp(&[
        EspFile {
            path: loader.clone(),
            bytes: boot.loader,
            replace: true,
        },
        EspFile {
            path: "EFI/losos/limine.conf".into(),
            bytes: conf.as_bytes(),
            replace: true,
        },
        // Empty, so no trial an earlier install left goes on.
        EspFile {
            path: GRUBENV.into(),
            bytes: boot.grubenv,
            replace: true,
        },
    ])?;
    created.push(MARKER.to_owned());
    let marker = created.join("\n") + "\n";
    target
        .put_esp(&[EspFile {
            path: MARKER.into(),
            bytes: marker.as_bytes(),
            replace: true,
        }])
        .wrap_err("recording what was added to the ESP")?;

    report.step("Adding LosOS to the firmware's boot menu");
    target.add_boot_entry("LosOS", &format!("\\{}", loader.replace('/', "\\")))?;
    Ok(())
}

/// The local copies of a release's three files, each checked against its
/// line in the signed SHA256SUMS before it is used.
pub fn fetch_files(
    source: &crate::release::Source,
    release: &Release,
    cache: &Path,
    report: &mut Console,
) -> Result<Files> {
    let mut get = |file: &crate::release::File| -> Result<PathBuf> {
        report.step(&format!("Getting {}", file.name));
        let path = source.fetch(&file.name, cache)?;
        let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let hash = crate::release::sha256_file(&path, |done| report.progress(done, total))?;
        if !hash.eq_ignore_ascii_case(&file.sha256) {
            // A bad download is not kept to be resumed from.
            if source.downloads() {
                let _ = std::fs::remove_file(&path);
            }
            miette::bail!("{} does not match the release's SHA256SUMS", file.name);
        }
        Ok(path)
    };
    Ok(Files {
        uki: get(&release.uki)?,
        verity: get(&release.verity.file)?,
        usr: get(&release.usr.file)?,
    })
}
