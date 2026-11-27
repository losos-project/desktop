//! `losos-windows-installer.exe`: install LosOS beside Windows.
//!
//! Run on Windows, it checks the machine can dual-boot, asks how much space
//! to give LosOS, shrinks C: by that much, writes the newest signed release
//! into the space, and adds Limine, which then offers LosOS and Windows at
//! every start. `--uninstall` takes all of that back.
//!
//! On Linux it does the same install into a disk image laid out like a
//! Windows disk (`--image`), which is how it is tested.
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use miette::{IntoDiagnostic, Result, bail, miette};
use tracing_subscriber::EnvFilter;

use losos_windows_installer::config;
use losos_windows_installer::guid::Guid;
use losos_windows_installer::install::{self, Boot, Console};
use losos_windows_installer::layout::{self, GIB};
use losos_windows_installer::release::{self, Arch, Release, Source};

const USAGE: &str = "\
usage: losos-windows-installer [--size GB] [--release DIR] [--channel URL] [--dry-run] [--yes]
       losos-windows-installer --uninstall [--yes]
       losos-windows-installer --image DISK --release DIR [--loader FILE] [--pubring FILE] [--ram GB] [--yes]";

#[derive(Default)]
struct Options {
    size: Option<u64>,
    release: Option<PathBuf>,
    channel: Option<String>,
    dry_run: bool,
    yes: bool,
    uninstall: bool,
    image: Option<PathBuf>,
    loader: Option<PathBuf>,
    pubring: Option<PathBuf>,
    ram: Option<u64>,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options> {
    let mut o = Options::default();
    let gb = |v: String| -> Result<u64> {
        v.trim_end_matches(['G', 'g', 'B', 'b'])
            .parse::<u64>()
            .map(|n| n * GIB)
            .map_err(|_| miette!(help = USAGE, "{v} is not a whole number of GB"))
    };
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| miette!(help = USAGE, "{flag} needs a value"))
        };
        match flag.as_str() {
            "--size" => o.size = Some(gb(value()?)?),
            "--release" => o.release = Some(value()?.into()),
            "--channel" => o.channel = Some(value()?),
            "--image" => o.image = Some(value()?.into()),
            "--loader" => o.loader = Some(value()?.into()),
            "--pubring" => o.pubring = Some(value()?.into()),
            "--ram" => o.ram = Some(gb(value()?)?),
            "--dry-run" => o.dry_run = true,
            "--yes" => o.yes = true,
            "--uninstall" => o.uninstall = true,
            "--help" | "-h" => bail!(help = USAGE, "LosOS's installer for Windows machines"),
            _ => bail!(help = USAGE, "unknown argument {flag}"),
        }
    }
    Ok(o)
}

fn ask(question: &str) -> Result<String> {
    print!("{question} ");
    io::stdout().flush().into_diagnostic()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).into_diagnostic()?;
    Ok(line.trim().to_owned())
}

/// Everything build.rs compiled in, with the command line's overrides.
struct Settings {
    arch: Arch,
    keyring: Option<Vec<u8>>,
    loader: Vec<u8>,
    source: Source,
}

fn settings(o: &Options) -> Result<Settings> {
    let arch_name = config::ARCH.unwrap_or(std::env::consts::ARCH);
    let arch = Arch::named(arch_name).ok_or_else(|| miette!("no LosOS for {arch_name}"))?;
    let read = |p: &PathBuf| std::fs::read(p).into_diagnostic();
    let keyring = match &o.pubring {
        Some(p) => Some(read(p)?),
        None => config::PUBRING.map(<[u8]>::to_vec),
    };
    let loader = match &o.loader {
        Some(p) => read(p)?,
        None => config::LOADER
            .map(<[u8]>::to_vec)
            .ok_or_else(|| miette!(help = USAGE, "this build carries no Limine; pass --loader"))?,
    };
    let source = match (
        &o.release,
        &o.channel,
        config::UPDATE_URL,
        config::UPDATE_INDEX,
    ) {
        (Some(dir), _, _, _) => Source::Directory(dir.clone()),
        (None, Some(url), _, _) => Source::Channel(url.clone()),
        (None, None, Some(url), _) => Source::Channel(url.into()),
        (None, None, None, Some(url)) => Source::Index(url.into()),
        (None, None, None, None) => bail!(
            help = USAGE,
            "this build has no channel; pass --channel or --release"
        ),
    };
    Ok(Settings {
        arch,
        keyring,
        loader,
        source,
    })
}

/// The newest release the source offers, with its manifest's signature
/// checked as sysupdate checks it.
fn choose_release(s: &Settings, cache: &std::path::Path) -> Result<Release> {
    let (sums, signature) = s.source.manifest(cache)?;
    match &s.keyring {
        Some(keyring) => release::verify_signature(&sums, &signature, keyring)?,
        // As the OS's own updates do without a key (update.nix), and said
        // as loudly.
        None => eprintln!("warning: this build has no release key, so the release is not verified"),
    }
    release::newest(&String::from_utf8_lossy(&sums), config::IMAGE_ID, &s.arch)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
    let result = parse(std::env::args().skip(1)).and_then(|o| {
        if o.image.is_some() {
            image_install(&o)
        } else {
            platform::run(&o)
        }
    });
    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    };
    // Double-clicked, the console closes with the program; leave it up long
    // enough to be read.
    if cfg!(windows) && io::stdin().is_terminal() {
        let _ = ask("\nPress Enter to close this window.");
    }
    code
}

#[cfg(unix)]
fn image_install(o: &Options) -> Result<()> {
    use losos_windows_installer::image::Image;
    use losos_windows_installer::install::Target;

    let path = o.image.as_ref().expect("checked by the caller");
    let s = settings(o)?;
    let cache = std::env::temp_dir().join("losos-windows-installer");
    let release = choose_release(&s, &cache)?;
    let mut image = Image::open(path)?;
    let region = image.free_behind_windows()?;
    let ram = o.ram.unwrap_or(6 * GIB);
    let plan = layout::plan(
        region,
        &release,
        &s.arch,
        config::IMAGE_ID,
        config::USR_SIZE,
        ram,
        Guid::new_v4(),
    )?;
    println!(
        "Installing LosOS {} into {} behind Windows on {}",
        release.version,
        layout::size_text(region.len()),
        path.display()
    );
    let mut console = Console::default();
    let files = install::fetch_files(&s.source, &release, &cache, &mut console)?;
    let boot = Boot {
        loader: &s.loader,
    };
    if let Err(e) = install::run(
        &mut image,
        &plan,
        &release,
        &files,
        &s.arch,
        config::IMAGE_ID,
        &boot,
        &mut console,
    ) {
        if let Err(undo) = image.undo() {
            eprintln!("{undo:?}");
        }
        return Err(e);
    }
    println!("\nDone.");
    Ok(())
}

#[cfg(not(unix))]
fn image_install(_: &Options) -> Result<()> {
    bail!("--image is for testing on Linux")
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn run(_: &Options) -> Result<()> {
        bail!(
            help = USAGE,
            "this installs LosOS beside Windows; run it on Windows, or pass --image"
        )
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use losos_windows_installer::install::Target;
    use losos_windows_installer::layout::{MIB, required, size_text};
    use losos_windows_installer::windows::{self as win, BitLocker, Disk, Layout, WindowsTarget};
    use windows_sys::Win32::System::Ioctl::PARTITION_INFORMATION_EX;

    /// Free space Windows keeps on C: whatever is asked for: below this,
    /// Windows Update stops installing.
    const KEEP_FREE: u64 = 10 * GIB;
    /// How many Windows starts BitLocker stays suspended for (suspend_bitlocker).
    const BITLOCKER_REBOOTS: u32 = 2;

    fn cache() -> PathBuf {
        let data = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(data).join("LosOS").join("Downloads")
    }

    /// C:, the disk it is on, that disk's partition table, and C:'s entry in
    /// the table.
    fn system_disk() -> Result<(String, Disk, Layout, PARTITION_INFORMATION_EX)> {
        let drive = win::system_drive();
        let extent = win::volume_extent(&drive)?;
        let disk = Disk::open(extent.DiskNumber)?;
        let table = disk.read_layout()?;
        let windows = *table
            .at(extent.StartingOffset as u64)
            .ok_or_else(|| miette!("{drive} is not a partition of disk {}", disk.number))?;
        Ok((drive, disk, table, windows))
    }

    pub fn run(o: &Options) -> Result<()> {
        if !win::ensure_admin()? {
            return Ok(());
        }
        if o.uninstall {
            return uninstall(o);
        }
        println!("LosOS installer\n");
        let s = settings(o)?;
        let machine = win::machine()?;
        check(&machine, &s.arch)?;

        let (drive, disk, table, windows) = system_disk()?;
        let offset = windows.StartingOffset as u64;
        if Layout::type_of(&windows) != losos_windows_installer::guid::types::MICROSOFT_BASIC_DATA {
            bail!("{drive} is not an ordinary Windows partition");
        }
        if table.esp().is_none() {
            bail!("the disk Windows is on has no EFI system partition");
        }
        let c_size = windows.PartitionLength as u64;
        let behind = table
            .free_behind(offset)
            .expect("the partition is in the table");

        let release = choose_release(&s, &cache())?;
        println!(
            "Newest release: LosOS {} from {}",
            release.version,
            s.source.describe()
        );

        let (size_min, _) = win::supported_size(&drive)?;
        let used = c_size.saturating_sub(win::free_space(std::path::Path::new(&format!(
            "{drive}\\"
        )))?);
        let floor = size_min.max(used + KEEP_FREE);
        let most = c_size.saturating_sub(floor) / GIB * GIB + behind.len() / GIB * GIB;
        let least = required(config::USR_SIZE, machine.ram).div_ceil(GIB) * GIB;
        if most < least {
            bail!(
                help = "Free some space on Windows' drive (Disk Cleanup, or move files off it), \
                        or turn hibernation and System Restore off for the install, then run this again.",
                "LosOS needs {} and Windows can give up only {}",
                size_text(least),
                size_text(most)
            );
        }
        let size = match o.size {
            Some(s) if (least..=most).contains(&s) => s,
            Some(s) => bail!(
                "--size {} is outside what fits: {} to {}",
                size_text(s),
                size_text(least),
                size_text(most)
            ),
            None => {
                let suggested = (64 * GIB).clamp(least, most);
                let answer = ask(&format!(
                    "How much space should LosOS get, in GB? ({} to {}, Enter for {})",
                    least / GIB,
                    most / GIB,
                    suggested / GIB
                ))?;
                if answer.is_empty() {
                    suggested
                } else {
                    let n: u64 = answer
                        .parse()
                        .map_err(|_| miette!("{answer} is not a whole number of GB"))?;
                    if !(least..=most).contains(&(n * GIB)) {
                        bail!("{n} GB is outside {} to {}", least / GIB, most / GIB);
                    }
                    n * GIB
                }
            }
        };
        let shrink = size.saturating_sub(behind.len()).div_ceil(MIB) * MIB;
        let new_c = c_size - shrink;

        println!("\nThis will:");
        if shrink > 0 {
            println!(
                "  - shrink {drive} from {} to {}",
                size_text(c_size),
                size_text(new_c)
            );
        }
        println!(
            "  - install LosOS {} into the {} behind it",
            release.version,
            size_text(shrink + behind.len())
        );
        println!("  - add Limine to the EFI system partition and make it start first,");
        println!("    so every start offers LosOS and Windows");
        if machine.fast_startup {
            println!("  - turn Windows' fast startup off");
        }
        if machine.bitlocker == BitLocker::On {
            println!(
                "  - suspend BitLocker on {drive} until Windows has started {BITLOCKER_REBOOTS} times,"
            );
            println!("    so it does not ask for the recovery key after the boot menu changes.");
            println!("    Have the recovery key at hand anyway (aka.ms/myrecoverykey).");
        }
        println!("Back up anything you cannot lose first. Nothing changes until you confirm.");
        if o.dry_run {
            println!("\n--dry-run: stopping here.");
            return Ok(());
        }
        if !o.yes && ask("\nType install to go ahead:")? != "install" {
            bail!("nothing was changed");
        }

        let mut console = Console::default();
        let files = install::fetch_files(&s.source, &release, &cache(), &mut console)?;
        // The download took space on C:, so how far it shrinks is asked again.
        let (size_min, _) = win::supported_size(&drive)?;
        if new_c < size_min {
            bail!(
                "with the download on {drive}, it can only shrink to {}; free some space and run this again",
                size_text(size_min)
            );
        }

        if machine.fast_startup {
            console.step("Turning fast startup off");
            win::disable_fast_startup()?;
        }
        if machine.bitlocker == BitLocker::On {
            console.step("Suspending BitLocker");
            win::suspend_bitlocker(&drive, BITLOCKER_REBOOTS)?;
        }
        if shrink > 0 {
            console.step(&format!("Shrinking {drive} (this can take a while)"));
            win::resize(&drive, new_c)?;
        }

        let result = (|| -> Result<()> {
            let table = disk.read_layout()?;
            let region = table
                .free_behind(offset)
                .ok_or_else(|| miette!("{drive} moved while it was being shrunk"))?;
            let plan = layout::plan(
                region,
                &release,
                &s.arch,
                config::IMAGE_ID,
                config::USR_SIZE,
                machine.ram,
                Guid::new_v4(),
            )?;
            let mut target = WindowsTarget::new(Disk::open(disk.number)?, &table)?;
            let boot = Boot {
                loader: &s.loader,
            };
            install::run(
                &mut target,
                &plan,
                &release,
                &files,
                &s.arch,
                config::IMAGE_ID,
                &boot,
                &mut console,
            )
            .inspect_err(|_| {
                if let Err(undo) = target.undo() {
                    eprintln!("{undo:?}");
                }
            })
        })();
        if let Err(e) = result {
            if shrink > 0 {
                eprintln!("\nGiving {drive} its space back.");
                if let Err(grow) = win::resize(&drive, c_size) {
                    eprintln!("{grow:?}");
                }
            }
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(cache());

        println!("\n\nLosOS is installed. At the next start the boot menu offers LosOS and");
        println!(
            "Windows; LosOS's first start finishes setting up its disk and asks for an account."
        );
        if !o.yes && ask("Restart now? [y/N]")?.eq_ignore_ascii_case("y") {
            std::process::Command::new("shutdown")
                .args(["/r", "/t", "0"])
                .status()
                .into_diagnostic()?;
        }
        Ok(())
    }

    fn check(m: &win::Machine, arch: &Arch) -> Result<()> {
        if !m.uefi {
            bail!(
                help = "Switch the firmware from legacy/CSM to UEFI boot, which needs Windows on a GPT disk.",
                "Windows started in legacy BIOS mode; LosOS boots only with UEFI"
            );
        }
        if m.arch() != Some(arch.name) {
            bail!(
                "this installer is for {} machines, and this one is {}",
                arch.name,
                m.arch().unwrap_or("neither x86_64 nor arm64")
            );
        }
        if m.secure_boot {
            bail!(
                help = "Turn Secure Boot off in the firmware settings (and, with BitLocker on, have the \
                        recovery key at hand when Windows next starts), then run this again.",
                "Secure Boot is on, and LosOS's boot loader and kernel are not signed yet (docs/not-done.md)"
            );
        }
        if m.bitlocker == BitLocker::Converting {
            bail!(
                "BitLocker is still encrypting or decrypting the drive; run this again once it is done"
            );
        }
        Ok(())
    }

    fn uninstall(o: &Options) -> Result<()> {
        println!("Removing LosOS\n");
        let s = settings(o)?;
        let (drive, disk, table, windows) = system_disk()?;
        let windows_end = (windows.StartingOffset + windows.PartitionLength) as u64;
        let usr = [s.arch.usr_type, s.arch.verity_type, s.arch.root_type];
        let ours = win::losos_partitions(&table, windows_end, &usr);
        let size: u64 = ours
            .iter()
            .map(|&i| table.entries[i].PartitionLength as u64)
            .sum();
        let esp = table
            .esp()
            .ok_or_else(|| miette!("the disk Windows is on has no EFI system partition"))?;
        println!("This will remove Limine and LosOS's boot entry, and delete");
        println!(
            "{} partitions ({}) behind {drive}, giving their space back to it.",
            ours.len(),
            size_text(size)
        );
        println!("Everything in LosOS, its users' homes included, is lost.");
        if !o.yes && ask("\nType remove to go ahead:")? != "remove" {
            bail!("nothing was changed");
        }
        let mut console = Console::default();
        if win::bitlocker(&drive) == BitLocker::On {
            console.step("Suspending BitLocker");
            win::suspend_bitlocker(&drive, BITLOCKER_REBOOTS)?;
        }
        console.step("Removing the boot entry");
        let loader = format!("\\{}", install::loader_path(&s.arch).replace('/', "\\"));
        win::remove_boot_entry(&loader)?;
        console.step("Removing Limine from the EFI system partition");
        win::remove_esp_files(disk.number, esp.StartingOffset as u64)?;
        if !ours.is_empty() {
            console.step("Deleting LosOS's partitions");
            win::remove_partitions(&disk, &table, &ours)?;
            console.step(&format!("Growing {drive}"));
            let (_, most) = win::supported_size(&drive)?;
            win::resize(&drive, most)?;
        }
        println!("\nLosOS is removed.");
        Ok(())
    }
}
