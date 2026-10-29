//! Hakoniwa containers for Danube (docs/danube.md, "Sandbox and
//! hardening").
//!
//! Two sandboxes are built here. [`browser`] is the one the whole browser
//! runs in: its own user, mount, PID, IPC and UTS namespaces, a filesystem
//! of the store, its profile and Downloads, and a seccomp filter. Inside
//! it, WebKit puts each web process in a second, narrower sandbox; WebKit
//! only knows how to ask bwrap for that, so the `danube-sandbox` program
//! takes the arguments WebKit gives bwrap ([`Plan::from_bwrap_args`]) and
//! builds the same sandbox with Hakoniwa, and Landlock over it.
//! WebKit runs it in bwrap's place because Danube sets
//! `WEBKIT_SANDBOX_LAUNCHER` (nixos/pkgs/patches/wpewebkit).
//!
//! Both end in [`Plan::run`], which also carries the open file descriptors
//! a sandboxed program needs (WebKit's IPC socket among them) through
//! Hakoniwa, which closes every descriptor but the standard three.

pub mod browser;
mod launch;

use std::fs::File;
use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};

pub use launch::run;

/// Why a sandbox could not be built or entered.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error("{0} needs {1} argument(s)")]
    #[diagnostic(code(danube_sandbox::usage))]
    MissingArgument(String, usize),
    #[error("unknown sandbox option {0}")]
    #[diagnostic(
        code(danube_sandbox::unknown),
        help("WebKit passed a bwrap option this launcher does not implement; it needs adding to Plan::from_bwrap_args")
    )]
    Unknown(String),
    #[error("{0} is not a file descriptor")]
    #[diagnostic(code(danube_sandbox::fd))]
    BadFd(String),
    #[error("no program to run in the sandbox")]
    #[diagnostic(code(danube_sandbox::program))]
    NoProgram,
    #[error("cannot read {0}")]
    #[diagnostic(code(danube_sandbox::read))]
    Read(String, #[source] std::io::Error),
    #[error("cannot start the sandbox")]
    #[diagnostic(
        code(danube_sandbox::spawn),
        help("the kernel must allow unprivileged user namespaces (the OS sets kernel.unprivileged_userns_clone)")
    )]
    Spawn(#[source] hakoniwa::Error),
    #[error("cannot pass file descriptors into the sandbox")]
    #[diagnostic(code(danube_sandbox::pass))]
    Pass(#[source] std::io::Error),
}

/// One step of building the sandbox's filesystem, in bwrap's terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Bind `src` at `dest`; `ro` read-only, `dev` keeping device nodes
    /// usable, `optional` skipping a `src` that does not exist.
    Bind {
        src: String,
        dest: String,
        ro: bool,
        dev: bool,
        optional: bool,
    },
    /// A symbolic link at `link` pointing to `target`.
    Symlink { target: String, link: String },
    /// An empty tmpfs.
    Tmpfs(String),
    /// A procfs for the new PID namespace.
    Proc(String),
    /// A minimal /dev: null, zero, full, random, urandom, tty, pts, shm.
    Dev(String),
    /// An empty directory.
    Dir(String),
    /// A file with these contents.
    File { dest: String, contents: String },
}

/// Namespaces beyond the user and mount ones every sandbox gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unshare {
    Pid,
    Ipc,
    Net,
    Uts,
    Cgroup,
}

/// What Landlock lets the sandboxed program do with a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read and execute.
    Read,
    /// Read, execute, write, create and remove.
    Write,
    /// As `Read`, and use device nodes.
    Device,
}

/// A sandbox, and what to run in it.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub unshare: Vec<Unshare>,
    pub ops: Vec<Op>,
    /// Environment changes on top of the launcher's own: `Some` sets, `None`
    /// removes.
    pub env: Vec<(String, Option<String>)>,
    /// A compiled seccomp program (`struct sock_filter` array), loaded just
    /// before the program starts: WebKit's own filter for web processes.
    pub seccomp_bpf: Option<Vec<u8>>,
    /// Hakoniwa's own seccomp filter, from [`browser::syscall_denylist`].
    pub deny_syscalls: Vec<&'static str>,
    /// Landlock rules; empty means no Landlock ruleset.
    pub landlock: Vec<(String, Access)>,
    /// Where to write `{"child-pid": N}`, as bwrap's `--info-fd` does.
    pub info_fd: Option<RawFd>,
    /// Descriptors read here and not to be passed on.
    pub consumed_fds: Vec<RawFd>,
    /// Pass the program none of the launcher's descriptors but the standard
    /// three, and leave them open here. WebKit hands its web processes
    /// their IPC sockets as open descriptors, so its launches pass every
    /// one; Danube's own sandboxes (the browser, its bus proxy) are started
    /// from a process that holds a connection to the whole session bus,
    /// which must not reach inside.
    pub no_inherited_fds: bool,
    pub chdir: Option<String>,
    pub program: Vec<String>,
}

fn take_fd(arg: &str) -> Result<RawFd, Error> {
    arg.parse::<RawFd>()
        .ok()
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| Error::BadFd(arg.to_owned()))
}

/// Reads all of an inherited descriptor and closes it.
fn read_fd(fd: RawFd) -> Result<Vec<u8>, Error> {
    // SAFETY: WebKit passed us this descriptor to read, and nothing else
    // here uses it.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let mut out = Vec::new();
    file.read_to_end(&mut out)
        .map_err(|e| Error::Read(format!("descriptor {fd}"), e))?;
    Ok(out)
}

impl Plan {
    /// Reads the arguments WebKit's BubblewrapLauncher gives bwrap: options,
    /// `--args FD` for more options NUL-separated in a sealed memfd, then
    /// `--` and the program. Only what WebKit uses is implemented; anything
    /// else is an error rather than silently a weaker sandbox.
    pub fn from_bwrap_args(args: impl IntoIterator<Item = String>) -> Result<Self, Error> {
        let mut plan = Plan::default();
        let mut queue: std::collections::VecDeque<String> = args.into_iter().collect();
        while let Some(arg) = queue.pop_front() {
            let mut take = |n: usize| -> Result<Vec<String>, Error> {
                if queue.len() < n {
                    return Err(Error::MissingArgument(arg.clone(), n));
                }
                Ok(queue.drain(..n).collect())
            };
            match arg.as_str() {
                "--" => {
                    plan.program = queue.drain(..).collect();
                    break;
                }
                "--args" => {
                    let fd = take_fd(&take(1)?[0])?;
                    let bytes = read_fd(fd)?;
                    let more: Vec<String> = bytes
                        .split(|b| *b == 0)
                        .filter(|a| !a.is_empty())
                        .map(|a| String::from_utf8_lossy(a).into_owned())
                        .collect();
                    // In front of what is left, as bwrap reads them.
                    for (i, a) in more.into_iter().enumerate() {
                        queue.insert(i, a);
                    }
                }
                // Hakoniwa's container always dies with the launcher, and
                // the launcher with WebKit (launch.rs).
                "--die-with-parent" | "--new-session" | "--unshare-user" | "--unshare-user-try" => {
                }
                "--unshare-pid" => plan.unshare.push(Unshare::Pid),
                "--unshare-ipc" => plan.unshare.push(Unshare::Ipc),
                "--unshare-net" => plan.unshare.push(Unshare::Net),
                "--unshare-uts" => plan.unshare.push(Unshare::Uts),
                "--unshare-cgroup" | "--unshare-cgroup-try" => plan.unshare.push(Unshare::Cgroup),
                "--unshare-all" => plan.unshare.extend([
                    Unshare::Pid,
                    Unshare::Ipc,
                    Unshare::Net,
                    Unshare::Uts,
                    Unshare::Cgroup,
                ]),
                "--bind" | "--bind-try" | "--ro-bind" | "--ro-bind-try" | "--dev-bind"
                | "--dev-bind-try" => {
                    let v = take(2)?;
                    plan.ops.push(Op::Bind {
                        src: v[0].clone(),
                        dest: v[1].clone(),
                        ro: arg.starts_with("--ro-"),
                        dev: arg.starts_with("--dev-"),
                        optional: arg.ends_with("-try"),
                    });
                }
                "--symlink" => {
                    let v = take(2)?;
                    plan.ops.push(Op::Symlink {
                        target: v[0].clone(),
                        link: v[1].clone(),
                    });
                }
                "--tmpfs" => plan.ops.push(Op::Tmpfs(take(1)?.remove(0))),
                "--proc" => plan.ops.push(Op::Proc(take(1)?.remove(0))),
                "--dev" => plan.ops.push(Op::Dev(take(1)?.remove(0))),
                "--dir" => plan.ops.push(Op::Dir(take(1)?.remove(0))),
                "--ro-bind-data" | "--bind-data" | "--file" => {
                    let v = take(2)?;
                    let fd = take_fd(&v[0])?;
                    let contents = String::from_utf8_lossy(&read_fd(fd)?).into_owned();
                    plan.ops.push(Op::File {
                        dest: v[1].clone(),
                        contents,
                    });
                }
                "--setenv" => {
                    let v = take(2)?;
                    plan.env.push((v[0].clone(), Some(v[1].clone())));
                }
                "--unsetenv" => plan.env.push((take(1)?.remove(0), None)),
                "--chdir" => plan.chdir = Some(take(1)?.remove(0)),
                "--seccomp" => {
                    let fd = take_fd(&take(1)?[0])?;
                    plan.seccomp_bpf = Some(read_fd(fd)?);
                }
                "--info-fd" => {
                    let fd = take_fd(&take(1)?[0])?;
                    plan.info_fd = Some(fd);
                    plan.consumed_fds.push(fd);
                }
                _ => return Err(Error::Unknown(arg)),
            }
        }
        if plan.program.is_empty() {
            return Err(Error::NoProgram);
        }
        // Landlock over what WebKit set up, which bwrap has no equivalent
        // of: the program may read and run whatever is in its sandbox, and
        // write only where WebKit made it writable. Here rather than over
        // the whole browser (browser.rs), which would forbid this sandbox.
        plan.landlock.push(("/".into(), Access::Read));
        let writable: Vec<(String, Access)> = plan
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Bind {
                    dest,
                    ro: false,
                    dev,
                    ..
                } => Some((
                    dest.clone(),
                    if *dev { Access::Device } else { Access::Write },
                )),
                Op::Tmpfs(dest) => Some((dest.clone(), Access::Write)),
                Op::Dev(dest) => Some((dest.clone(), Access::Device)),
                _ => None,
            })
            .collect();
        plan.landlock.extend(writable);
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_webkits_options() {
        let plan = Plan::from_bwrap_args(args(&[
            "--die-with-parent",
            "--unshare-pid",
            "--unshare-net",
            "--ro-bind",
            "/nix/store",
            "/nix/store",
            "--dev-bind-try",
            "/dev/dri",
            "/dev/dri",
            "--symlink",
            "usr/lib",
            "/lib",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--setenv",
            "A",
            "1",
            "--unsetenv",
            "B",
            "--",
            "/bin/WebKitWebProcess",
            "7",
        ]))
        .unwrap();
        assert_eq!(plan.unshare, vec![Unshare::Pid, Unshare::Net]);
        assert_eq!(plan.program, args(&["/bin/WebKitWebProcess", "7"]));
        assert_eq!(
            plan.ops[1],
            Op::Bind {
                src: "/dev/dri".into(),
                dest: "/dev/dri".into(),
                ro: false,
                dev: true,
                optional: true
            }
        );
        assert_eq!(
            plan.env,
            vec![("A".into(), Some("1".into())), ("B".into(), None)]
        );
        // Read and run anything in there; write only to the GPU and /dev.
        assert_eq!(
            plan.landlock,
            vec![
                ("/".into(), Access::Read),
                ("/dev/dri".into(), Access::Device),
                ("/dev".into(), Access::Device),
            ]
        );
    }

    #[test]
    fn refuses_what_it_does_not_implement() {
        assert!(matches!(
            Plan::from_bwrap_args(args(&["--cap-add", "ALL", "--", "x"])),
            Err(Error::Unknown(_))
        ));
        assert!(matches!(
            Plan::from_bwrap_args(args(&["--unshare-pid"])),
            Err(Error::NoProgram)
        ));
    }
}
