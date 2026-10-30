//! The files machine's one program (docs/files-machine.md).
//!
//!     losos-filesd init [<config>]     PID 1: starts the daemon, reaps
//!     losos-filesd daemon [<config>]   mounts what is plugged in, powers
//!                                      off when nothing has been for a while
//!     losos-filesd list                the volumes: name, type, size, device
//!     losos-filesd sftp <name>         serves a volume over SFTP on stdio
//!     losos-filesd eject <name>        unmounts it and lets the device go
//!     losos-filesd add <name> <url>    keeps a network share, its user and
//!                                      password on stdin, one per line
//!     losos-filesd remove <name>       forgets a network share

use std::collections::BTreeMap;
use std::io::{self, IsTerminal};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use losos_filesd::{
    device_of, mount_name, mount_options, names, parse_blkid, parse_url, valid_name, volume_line,
    Config, Share, Volume, EJECTED, MEDIA, VOLUMES,
};

/// Where the configuration is in the initrd.
const CONFIG: &str = "/etc/losos-filesd.conf";
/// How often the devices are looked at.
const TICK: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);
    // The daemon logs to the console, which is the machine's serial port
    // and so libvirt's log on the host; a call's stderr is its caller's.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(match command {
                Some("init" | "daemon") => "info",
                _ => "warn",
            })
        }))
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .without_time()
        .init();

    let config_path = || args.get(1).map(String::as_str).unwrap_or(CONFIG);
    let result = match (command, args.len()) {
        (Some("init"), _) => init(config_path()),
        (Some("daemon"), _) => daemon(config_path()),
        (Some("list"), 1) => list(),
        (Some("sftp"), 2) => sftp(&args[1]),
        (Some("eject"), 2) => eject(&args[1]),
        (Some("add"), 3) => add_share(&args[1], &args[2]),
        (Some("remove"), 2) => remove_share(&args[1]),
        _ => Err(miette!(
            "usage: losos-filesd init|daemon [<config>]\n       \
             losos-filesd list\n       \
             losos-filesd sftp|eject|remove <name>\n       \
             losos-filesd add <name> <url>"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(program: &str, args: &[&str]) -> bool {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            debug!(program, ?args, stderr = %String::from_utf8_lossy(&out.stderr).trim(), "failed");
            false
        }
        Err(e) => {
            warn!(program, error = %e, "cannot run");
            false
        }
    }
}

fn read_config(path: &str) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot read {path}"))?;
    Config::parse(&text).map_err(|e| miette!("{path}: {e}"))
}

/// Writes out what the disks hold and powers the machine off; libvirt sees
/// it stop, and the host's Files sees its volumes go.
fn power_off() -> ! {
    // SAFETY: sync and reboot take no pointers; reboot returns only on
    // failure, which leaves nothing to do but stop.
    unsafe {
        libc::sync();
        libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF);
    }
    std::process::exit(1)
}

/// PID 1, as in losos-netd: starts the daemon and reaps what is orphaned to
/// it, and a daemon that exits is started again after what it left is gone.
fn init(config: &str) -> Result<()> {
    let exe = std::env::current_exe().into_diagnostic()?;
    loop {
        let daemon = Command::new(&exe)
            .args(["daemon", config])
            .spawn()
            .into_diagnostic()
            .wrap_err("cannot start the daemon")?;
        let pid = daemon.id() as libc::pid_t;
        // Reaped by the waitpid(-1) below, with everything else.
        std::mem::forget(daemon);
        loop {
            let mut status = 0;
            // SAFETY: status is a valid int for waitpid to write.
            let reaped = unsafe { libc::waitpid(-1, &mut status, 0) };
            if reaped == pid {
                warn!(status, "the daemon exited");
                break;
            }
            if reaped < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        // SAFETY: plain syscalls on integers.
        unsafe { libc::kill(-1, libc::SIGKILL) };
        // SAFETY: as above.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        // Mounts outlive their daemon; the next one mounts afresh.
        for name in std::fs::read_dir(MEDIA).into_iter().flatten().flatten() {
            run("umount", &["-l", &name.path().to_string_lossy()]);
        }
        if let Err(e) = Command::new("mdev").arg("-d").status() {
            warn!(error = %e, "cannot start mdev");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

struct Mounted {
    name: String,
    volume: Volume,
}

/// The block devices that could hold files: not the machine's own virtio
/// disks or RAM disks, and not a disk whose partitions are listed instead.
fn candidates() -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/sys/class/block")
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if ["vd", "loop", "ram", "zram", "nbd"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        let sys = entry.path();
        let has_partitions = std::fs::read_dir(&sys)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.path().join("partition").exists());
        if has_partitions {
            continue;
        }
        // An empty card reader or a drive with no medium is 0 sectors.
        let size = std::fs::read_to_string(sys.join("size")).unwrap_or_default();
        if size.trim() == "0" {
            continue;
        }
        out.push(name);
    }
    out.sort();
    out
}

fn size_of(device: &str) -> u64 {
    std::fs::read_to_string(format!("/sys/class/block/{device}/size"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(0, |sectors| sectors * 512)
}

fn probe(device: &str) -> Option<Volume> {
    let out = Command::new("blkid")
        .arg(format!("/dev/{device}"))
        .stdin(Stdio::null())
        .output()
        .ok()?;
    parse_blkid(String::from_utf8_lossy(&out.stdout).lines().next()?)
}

fn write_volumes(mounted: &BTreeMap<String, Mounted>) {
    let text: String = mounted
        .values()
        .map(|m| volume_line(&m.name, &m.volume, size_of(&m.volume.device)))
        .collect();
    let partial = format!("{VOLUMES}.new");
    if let Err(e) = std::fs::write(&partial, text).and_then(|()| std::fs::rename(&partial, VOLUMES))
    {
        warn!(error = %e, "cannot write the volume list");
    }
}

/// Is something here that a person may still want: a volume, a device
/// that could become one, or a card reader that could be given a card?
fn busy(mounted: &BTreeMap<String, Mounted>, candidates: &[String]) -> bool {
    !mounted.is_empty()
        || !candidates.is_empty()
        || std::fs::read_dir("/sys/class/mmc_host")
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .is_some()
}

fn daemon(config_path: &str) -> Result<()> {
    let config = read_config(config_path)?;
    for dir in [MEDIA, EJECTED, "/run/losos-filesd"] {
        std::fs::create_dir_all(dir).into_diagnostic()?;
    }
    if let Some((device, path)) = &config.state_disk {
        mount_state(device, path)?;
    }
    if let Some((mac, address, router)) = &config.link {
        link_up(mac, address, router, config.nameserver.as_deref())?;
    }
    let mut mounted: BTreeMap<String, Mounted> = BTreeMap::new();
    let mut tried: BTreeMap<String, Instant> = BTreeMap::new();
    let mut running: Vec<(Vec<String>, Option<Child>, Instant)> = config
        .run
        .iter()
        .map(|c| (c.clone(), None, Instant::now() - Duration::from_secs(1)))
        .collect();
    let mut idle_since = Instant::now();
    write_volumes(&mounted);
    loop {
        let mut present = candidates();
        let shares = config
            .shares
            .as_deref()
            .map(load_shares)
            .unwrap_or_default();
        // A share is "present" as a device of its own name.
        present.extend(shares.iter().map(|s| format!("share:{}", s.name)));

        // Gone: unmounted at once, lazily, since there is nothing to
        // write back to a device that left. An ejected one is unmounted
        // already, and goes from the list.
        let gone: Vec<String> = mounted
            .keys()
            .filter(|d| !present.contains(d) || Path::new(EJECTED).join(d).exists())
            .cloned()
            .collect();
        let mut changed = false;
        for device in gone {
            if let Some(m) = mounted.remove(&device) {
                let path = Path::new(MEDIA).join(&m.name);
                run("umount", &["-l", &path.to_string_lossy()]);
                let _ = std::fs::remove_dir(&path);
                info!(device, name = m.name, "gone");
                changed = true;
            }
        }
        // A device that would not mount waits to be plugged in again; a
        // share, whose server may come back, is tried again each minute.
        tried.retain(|d, at| {
            present.contains(d)
                && !(d.starts_with("share:") && at.elapsed() > Duration::from_secs(60))
        });
        for device in std::fs::read_dir(EJECTED).into_iter().flatten().flatten() {
            let name = device.file_name().to_string_lossy().into_owned();
            if !present.contains(&name) {
                let _ = std::fs::remove_file(device.path());
            }
        }

        // New: mounted under its name, once; one that will not mount is
        // tried again only after it has been plugged in again.
        for device in &present {
            if mounted.contains_key(device)
                || tried.contains_key(device)
                || Path::new(EJECTED).join(device).exists()
            {
                continue;
            }
            tried.insert(device.clone(), Instant::now());
            if let Some(share) = device
                .strip_prefix("share:")
                .and_then(|n| shares.iter().find(|s| s.name == n))
            {
                if mount_share(share) {
                    let volume = Volume {
                        device: device.clone(),
                        label: share.name.clone(),
                        uuid: String::new(),
                        fstype: share.fstype.clone(),
                    };
                    let name = share.name.clone();
                    info!(name, source = share.source, "share mounted");
                    mounted.insert(device.clone(), Mounted { name, volume });
                    changed = true;
                }
                continue;
            }
            let Some(volume) = probe(device) else {
                continue;
            };
            let Some((fstype, options)) = mount_options(&volume.fstype) else {
                info!(device, fstype = volume.fstype, "not files; left alone");
                continue;
            };
            let taken: Vec<String> = mounted.values().map(|m| m.name.clone()).collect();
            let name = mount_name(&volume, &taken);
            let path = Path::new(MEDIA).join(&name);
            let _ = std::fs::create_dir_all(&path);
            let dev = format!("/dev/{device}");
            if run(
                "mount",
                &["-t", fstype, "-o", options, &dev, &path.to_string_lossy()],
            ) {
                info!(device, name, fstype, "mounted");
                mounted.insert(device.clone(), Mounted { name, volume });
                changed = true;
            } else {
                warn!(device, fstype, "cannot mount");
                let _ = std::fs::remove_dir(&path);
            }
        }
        if changed {
            write_volumes(&mounted);
        }

        // The programs the configuration keeps running.
        for (command, child, since) in &mut running {
            if let Some(c) = child {
                if let Ok(Some(status)) = c.try_wait() {
                    warn!(?command, %status, "exited");
                    *child = None;
                    *since = Instant::now();
                }
            }
            if child.is_none() && since.elapsed() >= Duration::from_secs(1) {
                match Command::new(&command[0])
                    .args(&command[1..])
                    .stdin(Stdio::null())
                    .spawn()
                {
                    Ok(c) => *child = Some(c),
                    Err(e) => {
                        warn!(?command, error = %e, "cannot start");
                        *since = Instant::now();
                    }
                }
            }
        }

        // Nothing here for a while: off, until the host starts the machine
        // again for the next thing plugged in.
        if busy(&mounted, &present) {
            idle_since = Instant::now();
        } else if config
            .idle
            .is_some_and(|s| idle_since.elapsed() >= Duration::from_secs(s))
        {
            info!("nothing plugged in for a while: powering off");
            for (_, child, _) in &mut running {
                if let Some(c) = child {
                    let _ = c.kill();
                }
            }
            power_off();
        }
        std::thread::sleep(TICK);
    }
}

/// Mounts the state disk, giving it a filesystem the first time, as
/// losos-netd does.
fn mount_state(device: &Path, path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).into_diagnostic()?;
    let (device, path) = (device.to_string_lossy(), path.to_string_lossy());
    for _ in 0..50 {
        if Path::new(&*device).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let options = "noatime,nodev,nosuid,noexec";
    if run("mount", &["-o", options, &device, &path]) {
        return Ok(());
    }
    info!(device = %device, "making the state disk's filesystem");
    if !run("mke2fs", &["-L", "losos-state", &device])
        || !run("mount", &["-o", options, &device, &path])
    {
        bail!("cannot mount the state disk {device} on {path}");
    }
    Ok(())
}

/// Brings up the one network card, by its MAC, with its address and
/// router, and names the DNS server.
fn link_up(mac: &str, address: &str, router: &str, nameserver: Option<&str>) -> Result<()> {
    let mut name = None;
    for _ in 0..100 {
        name = std::fs::read_dir("/sys/class/net")
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| {
                std::fs::read_to_string(e.path().join("address"))
                    .is_ok_and(|a| a.trim().eq_ignore_ascii_case(mac))
            })
            .map(|e| e.file_name().to_string_lossy().into_owned());
        if name.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let name = name.ok_or_else(|| miette!("no network card {mac}"))?;
    run("ip", &["link", "set", "dev", "lo", "up"]);
    run("ip", &["link", "set", "dev", &name, "up"]);
    run("ip", &["addr", "replace", address, "dev", &name]);
    run(
        "ip",
        &["route", "replace", "default", "via", router, "dev", &name],
    );
    if let Some(server) = nameserver {
        std::fs::write("/etc/resolv.conf", format!("nameserver {server}\n")).into_diagnostic()?;
    }
    Ok(())
}

fn load_shares(dir: &Path) -> Vec<Share> {
    let mut all: Vec<Share> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "conf"))
        .filter_map(|e| Share::from_text(&std::fs::read_to_string(e.path()).ok()?))
        .collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

fn mount_share(share: &Share) -> bool {
    let path = Path::new(MEDIA).join(&share.name);
    let _ = std::fs::create_dir_all(&path);
    let target = path.to_string_lossy();
    let ok = if share.fstype == "davfs" {
        // davfs2 reads its credentials from its secrets file, never the
        // command line.
        let secrets = format!(
            "\"{}\" \"{}\" \"{}\"\n",
            share.source,
            share.user.replace('"', "\\\""),
            share.password.replace('"', "\\\"")
        );
        let _ = std::fs::create_dir_all("/etc/davfs2");
        let written = std::fs::write("/etc/davfs2/secrets", secrets).is_ok()
            && std::fs::write(
                "/etc/davfs2/davfs2.conf",
                "dav_user root\ndav_group root\nuse_locks 0\n",
            )
            .is_ok();
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            "/etc/davfs2/secrets",
            std::fs::Permissions::from_mode(0o600),
        );
        written
            && run(
                "mount.davfs",
                &[&share.source, &target, "-o", "nosuid,nodev,noexec"],
            )
    } else {
        run(
            "mount",
            &[
                "-t",
                &share.fstype,
                "-o",
                &share.options(),
                &share.source,
                &target,
            ],
        )
    };
    if !ok {
        warn!(
            name = share.name,
            source = share.source,
            "cannot mount the share"
        );
        let _ = std::fs::remove_dir(&path);
    }
    ok
}

fn shares_dir() -> Result<PathBuf> {
    read_config(CONFIG)?
        .shares
        .ok_or_else(|| miette!("this machine keeps no network shares"))
}

/// Keeps a share; the daemon mounts it at its next look.
fn add_share(name: &str, url: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("{name:?} cannot be a share's name");
    }
    let (fstype, source) = parse_url(url)
        .ok_or_else(|| miette!("{url} is not an smb://, nfs:// or davs:// address"))?;
    let mut input = String::new();
    io::Read::read_to_string(&mut io::stdin(), &mut input).into_diagnostic()?;
    let mut lines = input.lines();
    let share = Share {
        name: name.to_owned(),
        fstype: fstype.to_owned(),
        source,
        user: lines.next().unwrap_or("").trim().to_owned(),
        password: lines.next().unwrap_or("").to_owned(),
    };
    if share.user.contains([',', '\n']) {
        bail!("a user name cannot hold a comma");
    }
    let dir = shares_dir()?;
    std::fs::create_dir_all(&dir).into_diagnostic()?;
    let path = dir.join(format!("{name}.conf"));
    let partial = path.with_extension("partial");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)
            .into_diagnostic()?;
        file.write_all(share.to_text().as_bytes())
            .into_diagnostic()?;
        file.sync_all().into_diagnostic()?;
    }
    std::fs::rename(&partial, &path).into_diagnostic()?;
    Ok(())
}

/// Forgets a share; the daemon unmounts it at its next look.
fn remove_share(name: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("no share {name}");
    }
    let path = shares_dir()?.join(format!("{name}.conf"));
    std::fs::remove_file(&path)
        .into_diagnostic()
        .wrap_err_with(|| format!("no share {name}"))
}

fn volumes() -> String {
    std::fs::read_to_string(VOLUMES).unwrap_or_default()
}

fn list() -> Result<()> {
    print!("{}", volumes());
    Ok(())
}

/// Serves one volume with OpenSSH's sftp-server on stdin and stdout, which
/// losos-vm-call carries to the host's sshfs. It starts in the volume; the
/// rest of this machine is nothing but BusyBox and holds nothing of anyone's.
fn sftp(name: &str) -> Result<()> {
    if !valid_name(name) || !names(&volumes()).iter().any(|n| n == name) {
        bail!("no volume {name}");
    }
    let config = read_config(CONFIG)?;
    let server = config
        .sftp_server
        .ok_or_else(|| miette!("no sftp-server in {CONFIG}"))?;
    let path: PathBuf = Path::new(MEDIA).join(name);
    let error = Command::new(server)
        .arg("-d")
        .arg(&path)
        .current_dir(&path)
        .exec();
    Err(error)
        .into_diagnostic()
        .wrap_err("cannot start sftp-server")
}

/// Unmounts a volume, writing out what it holds, and tells the kernel its
/// device is gone, so it can be pulled out; it stays unmounted until it is
/// plugged in again.
fn eject(name: &str) -> Result<()> {
    let text = volumes();
    let device = device_of(&text, name).ok_or_else(|| miette!("no volume {name}"))?;
    std::fs::write(Path::new(EJECTED).join(&device), "").into_diagnostic()?;
    let path = Path::new(MEDIA).join(name);
    // The host's connections to it end first: each sftp-server started in
    // the volume, whose host side then lets go of its mount.
    for _ in 0..20 {
        let serving: Vec<i32> = std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
            .filter(|pid| {
                std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd == path)
            })
            .collect();
        if serving.is_empty() {
            break;
        }
        for pid in serving {
            // SAFETY: a plain syscall on integers.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !run("umount", &[&path.to_string_lossy()]) {
        let _ = std::fs::remove_file(Path::new(EJECTED).join(&device));
        bail!("{name} is in use");
    }
    let _ = std::fs::remove_dir(&path);
    // A share is let go of until the machine next starts.
    if device.starts_with("share:") {
        return Ok(());
    }
    // The whole disk a partition is on: its parent in sysfs.
    let sys = std::fs::canonicalize(format!("/sys/class/block/{device}")).into_diagnostic()?;
    let disk = if sys.join("partition").exists() {
        sys.parent().map(Path::to_path_buf).unwrap_or(sys)
    } else {
        sys
    };
    let delete = disk.join("device/delete");
    if delete.exists() {
        std::fs::write(&delete, "1").into_diagnostic()?;
    }
    Ok(())
}
