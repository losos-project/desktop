//! Turning a [`Plan`] into a running Hakoniwa container.
//!
//! Hakoniwa closes every descriptor but the standard three before the
//! program starts, and a sandboxed web process needs its IPC socket, which
//! WebKit names by number on its command line. So the container's stdin is
//! one end of a socket pair: once the container is up, the launcher sends
//! every descriptor it was given over it (SCM_RIGHTS), and the first thing
//! that runs inside puts each back at its own number, restores the real
//! stdin, loads WebKit's seccomp program if there is one, and execs.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::Path;
use std::process::ExitCode;

use hakoniwa::landlock::{CompatMode, FsAccess, Resource, Ruleset};
use hakoniwa::seccomp::{Action, Filter};
use hakoniwa::{Container, MountOptions, Namespace, Runctl, Stdio};
use nix::sys::socket::{
    recvmsg, sendmsg, socketpair, AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags,
    SockFlag, SockType,
};
use tracing::{debug, warn};

use crate::{Access, Error, Op, Plan, Unshare};

/// At most this many descriptors go in one message (the kernel's
/// SCM_MAX_FD is 253).
const MAX_FDS: usize = 250;

/// Every descriptor this process has open from 3 up, but `skip`.
fn open_fds(skip: &[RawFd]) -> Vec<RawFd> {
    let Ok(dir) = std::fs::read_dir("/proc/self/fd") else {
        return Vec::new();
    };
    let mut fds: BTreeSet<RawFd> = dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd > 2 && !skip.contains(fd))
        .collect();
    // The directory listing's own descriptor is closed by now; drop any
    // number that no longer refers to anything.
    // SAFETY: F_GETFD only inspects the descriptor table.
    fds.retain(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1);
    fds.into_iter().collect()
}

/// The plan's Landlock rules for paths the container will have: those that
/// exist here, where the container's mounts come from, and those the plan
/// mounts something at. Hakoniwa resolves every rule's path inside the
/// container and refuses to start ("landlock path must be exist") when one
/// is missing, and a plan can name a path a session lacks: a machine has no
/// /dev/dri, a system without 32-bit graphics no /run/opengl-driver-32, a
/// new user none of ~/.icons or derisk's folders. Such a path is not
/// bound either, so there is nothing behind it for a rule to allow.
fn landlock_rules(plan: &Plan) -> impl Iterator<Item = &(String, Access)> {
    plan.landlock.iter().filter(|(path, _)| {
        let mounted = plan.ops.iter().any(|op| match op {
            Op::Bind { src, dest, .. } => dest == path && Path::new(src).exists(),
            Op::Tmpfs(dest) | Op::Proc(dest) | Op::Dev(dest) | Op::Dir(dest) => dest == path,
            Op::File { dest, .. } => dest == path,
            Op::Symlink { .. } => false,
        });
        let kept = mounted || Path::new(path).exists();
        if !kept {
            debug!(%path, "missing; no Landlock rule");
        }
        kept
    })
}

/// Whether an earlier bind of the plan already shows `dest`: an ancestor
/// bound from the same path with the same flags. WebKit binds the
/// D-Bus proxy's own binary inside the /nix/store it also binds; bwrap
/// takes that, but Hakoniwa creates every file bind's target first and
/// fails on a file it cannot open for writing (exit 125), so the
/// redundant bind is left out instead.
fn shown_by_ancestor(plan: &Plan, src: &str, dest: &str, ro: bool, dev: bool) -> bool {
    src == dest
        && plan.ops.iter().any(|op| match op {
            Op::Bind {
                src: s,
                dest: d,
                ro: r,
                dev: v,
                ..
            } => {
                s == d
                    && d != dest
                    && Path::new(dest).starts_with(d)
                    && (*r, *v) == (ro, dev)
                    && Path::new(s).exists()
            }
            _ => false,
        })
}

/// The folder a symlink goes in, when nothing else in the plan makes it.
/// WebKit asks for `/var/run` with no `/var`; bwrap makes the folder, but
/// Hakoniwa's symlink fails without it (exit 125). The root is an empty
/// tmpfs, so making it there is all bwrap does too.
fn unmade_parent(plan: &Plan, link: &str) -> Option<String> {
    let parent = Path::new(link).parent()?;
    if parent == Path::new("/") {
        return None;
    }
    let made = plan.ops.iter().any(|op| {
        let dest = match op {
            Op::Bind { src, dest, .. } if Path::new(src).exists() => dest,
            Op::Tmpfs(dest) | Op::Proc(dest) | Op::Dev(dest) | Op::Dir(dest) => dest,
            _ => return false,
        };
        parent.starts_with(dest)
    });
    (!made).then(|| parent.to_string_lossy().into_owned())
}

fn container(plan: &Plan) -> Container {
    let mut c = Container::new();
    // `new` also makes a PID namespace and mounts /proc; a plan that wants
    // neither says so by not asking.
    if !plan.unshare.contains(&Unshare::Pid) {
        c.share(Namespace::Pid);
    }
    for ns in &plan.unshare {
        c.unshare(match ns {
            Unshare::Pid => Namespace::Pid,
            Unshare::Ipc => Namespace::Ipc,
            Unshare::Net => Namespace::Network,
            Unshare::Uts => Namespace::Uts,
            Unshare::Cgroup => Namespace::Cgroup,
        });
    }
    // Bind mounts in a user namespace keep the flags the host mount has
    // locked (nosuid, nodev, ...); MountFallback adds them rather than fail.
    c.runctl(Runctl::MountFallback);

    let mut proc_mounted = plan.unshare.contains(&Unshare::Pid);
    for op in &plan.ops {
        match op {
            Op::Bind {
                src,
                dest,
                ro,
                dev,
                optional,
            } => {
                if !Path::new(src).exists() {
                    if !optional {
                        warn!(%src, "missing; not bound");
                    }
                    continue;
                }
                if shown_by_ancestor(plan, src, dest, *ro, *dev) {
                    debug!(%dest, "already shown by an earlier bind");
                    continue;
                }
                let mut options = MountOptions::BIND | MountOptions::REC;
                if *ro {
                    options |= MountOptions::RDONLY;
                }
                if !*dev {
                    options |= MountOptions::NOSUID | MountOptions::NODEV;
                }
                c.mount(src, dest, "", options, None);
            }
            Op::Symlink { target, link } => {
                if let Some(parent) = unmade_parent(plan, link) {
                    c.dir(&parent, 0o755);
                }
                c.symlink(target, link);
            }
            Op::Tmpfs(dest) => {
                c.tmpfsmount(dest);
            }
            Op::Proc(dest) => {
                if dest != "/proc" || !proc_mounted {
                    c.procfsmount(dest);
                    proc_mounted = true;
                }
            }
            Op::Dev(dest) => {
                c.devfsmount(dest);
            }
            Op::Dir(dest) => {
                c.dir(dest, 0o755);
            }
            Op::File { dest, contents } => {
                c.file(dest, contents);
            }
        }
    }

    if !plan.landlock.is_empty() {
        let mut ruleset = Ruleset::default();
        // Relax: an older kernel gets as much of Landlock as it has,
        // instead of no browser.
        ruleset.restrict(Resource::FS, CompatMode::Relax);
        for (path, access) in landlock_rules(plan) {
            let mode = match access {
                Access::Read => FsAccess::R | FsAccess::X,
                Access::Device => FsAccess::R | FsAccess::W | FsAccess::X,
                Access::Write => FsAccess::R | FsAccess::W | FsAccess::X,
            };
            ruleset.allow_path(path, mode);
        }
        c.landlock_ruleset(ruleset);
    }

    if !plan.deny_syscalls.is_empty() {
        let mut filter = Filter::new(Action::Allow);
        for syscall in &plan.deny_syscalls {
            filter.add_rule(Action::Errno(libc::EPERM), syscall);
        }
        c.seccomp_filter(filter);
    }
    c
}

/// Inside the container: takes back the descriptors, then becomes the
/// program. Only returns on failure, with the exit status to use.
fn enter(targets_hint: usize, seccomp: Option<&[u8]>, program: &[CString]) -> i32 {
    // The message: the target numbers as little-endian u32s, the last one
    // being the real stdin (0), and the descriptors in the same order.
    let mut numbers = vec![0u8; 4 * (targets_hint + 1)];
    let mut cmsg = nix::cmsg_space!([RawFd; MAX_FDS + 1]);
    let (count, fds) = {
        let mut iov = [IoSliceMut::new(&mut numbers)];
        let Ok(msg) = recvmsg::<()>(0, &mut iov, Some(&mut cmsg), MsgFlags::MSG_CMSG_CLOEXEC)
        else {
            return 126;
        };
        let mut fds = Vec::new();
        if let Ok(cmsgs) = msg.cmsgs() {
            for c in cmsgs {
                if let ControlMessageOwned::ScmRights(r) = c {
                    fds.extend(r);
                }
            }
        }
        (msg.bytes / 4, fds)
    };
    if fds.len() != count {
        return 126;
    }
    let targets: Vec<RawFd> = numbers[..count * 4]
        .chunks(4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as RawFd)
        .collect();
    // SAFETY (all below): plain descriptor-table calls on descriptors this
    // process now owns. First every received descriptor moves out of the
    // way, above any target, so placing one cannot close another. Just
    // above the highest target, not at a fixed number: the open-file soft
    // limit a systemd unit starts with is 1024, and a number past it fails.
    let above = targets.iter().max().map_or(0, |t| t + 1);
    let mut high = Vec::with_capacity(fds.len());
    for fd in &fds {
        let moved = unsafe { libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, above) };
        unsafe { libc::close(*fd) };
        if moved < 0 {
            return 126;
        }
        high.push(moved);
    }
    // The socket the descriptors came on is done with; its number may be
    // a target too (0 is: the real stdin).
    for (target, fd) in targets.iter().zip(&high) {
        // dup2 clears close-on-exec on the copy, so it survives exec.
        if unsafe { libc::dup2(*fd, *target) } < 0 {
            return 126;
        }
        unsafe { libc::close(*fd) };
    }

    if let Some(bpf) = seccomp {
        // A `struct sock_fprog` over WebKit's program, as bwrap loads it.
        let prog = libc::sock_fprog {
            len: (bpf.len() / std::mem::size_of::<libc::sock_filter>()) as u16,
            filter: bpf.as_ptr() as *mut libc::sock_filter,
        };
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &prog as *const libc::sock_fprog,
                ) != 0
            {
                return 126;
            }
        }
    }

    let mut argv: Vec<*const libc::c_char> = program.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // execv keeps the environment Hakoniwa set up for this process.
    unsafe { libc::execv(argv[0], argv.as_ptr()) };
    127
}

/// Runs `plan`'s program in its sandbox and waits for it, exiting as it
/// does.
pub fn run(plan: Plan) -> Result<ExitCode, Error> {
    // The launcher dies with whoever started it (WebKit, or the desktop
    // for the whole browser), and Hakoniwa makes the container die with
    // the launcher, so nothing outlives its parent.
    // SAFETY: prctl on this process.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };

    let carried = if plan.no_inherited_fds {
        Vec::new()
    } else {
        open_fds(&plan.consumed_fds)
    };
    if carried.len() > MAX_FDS {
        return Err(Error::Pass(std::io::Error::other(
            "too many open descriptors",
        )));
    }
    let (ours, theirs) = socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::SOCK_CLOEXEC,
    )
    .map_err(|e| Error::Pass(e.into()))?;

    let program: Vec<CString> = plan
        .program
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap_or_default())
        .collect();
    let seccomp = plan.seccomp_bpf.clone();
    let count = carried.len();

    let c = container(&plan);
    // SAFETY: the closure runs in the forked child, and only makes the
    // async-signal-safe calls `enter` lists before exec.
    let mut command =
        unsafe { c.command_from_closure(move || enter(count, seccomp.as_deref(), &program)) };
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    for (key, value) in &plan.env {
        match value {
            Some(v) => env.insert(key.clone(), v.clone()),
            None => env.remove(key),
        };
    }
    // Not WebKit's business inside: the launcher's own switch.
    env.remove("WEBKIT_SANDBOX_LAUNCHER_INNER");
    command.envs(env);
    command.current_dir(plan.chdir.as_deref().unwrap_or("/"));
    command.stdin(Stdio::from(theirs));
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    let mut child = command.spawn().map_err(Error::Spawn)?;
    debug!(pid = child.id(), program = ?plan.program, "sandbox started");

    // The descriptors, then our stdin as number 0.
    let mut numbers: Vec<u8> = Vec::with_capacity(4 * (count + 1));
    let mut fds: Vec<RawFd> = Vec::with_capacity(count + 1);
    for fd in &carried {
        numbers.extend_from_slice(&(*fd as u32).to_le_bytes());
        fds.push(*fd);
    }
    numbers.extend_from_slice(&0u32.to_le_bytes());
    fds.push(0);
    let iov = [IoSlice::new(&numbers)];
    let rights = [ControlMessage::ScmRights(&fds)];
    sendmsg::<()>(ours.as_raw_fd(), &iov, &rights, MsgFlags::empty(), None)
        .map_err(|e| Error::Pass(e.into()))?;
    drop(ours);
    // The sandbox has its copies; ours would keep WebKit's IPC socket open
    // after the web process died.
    for fd in &carried {
        // SAFETY: closing descriptors this process owns and no longer uses.
        unsafe { libc::close(*fd) };
    }

    if let Some(fd) = plan.info_fd {
        // xdg-desktop-portal reads this to map the sandbox's PIDs.
        // SAFETY: WebKit gave us this descriptor to write.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let _ = std::io::Write::write_all(
            &mut file,
            format!("{{\"child-pid\": {}}}\n", child.id()).as_bytes(),
        );
    }

    let status = child.wait().map_err(Error::Spawn)?;
    if !status.success() {
        debug!(code = status.code, reason = %status.reason, "sandboxed program ended");
    }
    Ok(ExitCode::from(status.code.clamp(0, 255) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landlock_rules_leave_out_missing_paths() {
        let here = std::env::temp_dir().to_string_lossy().into_owned();
        let gone = format!("{here}/danube-landlock-missing-{}", std::process::id());
        let plan = Plan {
            landlock: vec![(here.clone(), Access::Read), (gone, Access::Read)],
            ..Plan::default()
        };
        let kept: Vec<&str> = landlock_rules(&plan).map(|(p, _)| p.as_str()).collect();
        assert_eq!(kept, [here.as_str()]);
    }

    #[test]
    fn a_bind_inside_the_same_bind_is_left_out() {
        let bind = |path: &str, ro| Op::Bind {
            src: path.into(),
            dest: path.into(),
            ro,
            dev: false,
            optional: false,
        };
        let plan = Plan {
            ops: vec![bind("/", true), bind("/nix/store/x/bin/proxy", true)],
            ..Plan::default()
        };
        assert!(shown_by_ancestor(
            &plan,
            "/nix/store/x/bin/proxy",
            "/nix/store/x/bin/proxy",
            true,
            false
        ));
        // Not the bind itself, nor one with other flags.
        assert!(!shown_by_ancestor(&plan, "/", "/", true, false));
        assert!(!shown_by_ancestor(
            &plan,
            "/nix/store/x/bin/proxy",
            "/nix/store/x/bin/proxy",
            false,
            false
        ));
    }

    #[test]
    fn a_symlink_gets_its_folder_unless_the_plan_makes_it() {
        let link = |link: &str| Op::Symlink {
            target: "../run".into(),
            link: link.into(),
        };
        let plan = Plan {
            ops: vec![link("/var/run"), Op::Tmpfs("/tmp".into()), link("/tmp/x/y")],
            ..Plan::default()
        };
        assert_eq!(unmade_parent(&plan, "/var/run").as_deref(), Some("/var"));
        assert_eq!(unmade_parent(&plan, "/tmp/x/y"), None);
        assert_eq!(unmade_parent(&plan, "/lib64"), None);
    }
}
