//! The sandbox the whole browser runs in.
//!
//! What Danube can see: the Nix store and the system's configuration,
//! read-only; the GPU, when it can draw ([`gpu_draws`]); its own profile,
//! cache and settings directories, and Downloads, writable; the fonts,
//! themes and icons a user installs, read-only; the Wayland, PipeWire and
//! PulseAudio sockets and derisk's; the compiled filter lists; the files the
//! person chose to upload, as the document portal shows them to Danube. The
//! session bus is only available through xdg-dbus-proxy, filtered to the
//! portals, notifications and accessibility. Nothing else in the home
//! directory exists in there, so a compromised browser cannot read SSH keys
//! or other programs' data.
//!
//! The portals tell a sandboxed app by the `/.flatpak-info` file at the
//! root of whoever is on the other end of its bus connection, which is
//! the proxy ([`bus_proxy`]): with one, the file chooser hands Danube its
//! chosen files through the document portal, the one way into this
//! sandbox from outside it, instead of as paths it cannot open.
//! The key socket carries security key requests to the launcher, without
//! exposing device nodes to the browser.
//!
//! A seccomp filter refuses the system calls a browser never needs.
//! Network access is shared: it is a browser. There is no Landlock ruleset
//! here: Landlock forbids a confined process to mount or to write its
//! user namespace's uid_map, and WebKit builds each page's sandbox inside
//! this one with both. The page sandboxes carry the Landlock rules instead
//! ([`Plan::from_bwrap_args`]), around the web content itself.

use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use tracing::info;

use crate::{Access, Op, Plan, Unshare};

/// System calls a browser has no use for, refused with EPERM. Flatpak's
/// list (flatpak-run.c's syscall_blocklist), less what WebKit's own sandbox
/// needs to build the per-page one inside this one: clone and unshare with
/// new namespaces, mount, pivot_root.
pub fn syscall_denylist() -> Vec<&'static str> {
    vec![
        "syslog",
        "uselib",
        "acct",
        "quotactl",
        "add_key",
        "keyctl",
        "request_key",
        "move_pages",
        "mbind",
        "get_mempolicy",
        "set_mempolicy",
        "migrate_pages",
        "kexec_load",
        "kexec_file_load",
        "init_module",
        "finit_module",
        "delete_module",
        "perf_event_open",
        "bpf",
        "userfaultfd",
        "open_by_handle_at",
        "name_to_handle_at",
        "lookup_dcookie",
        "fanotify_init",
        "iopl",
        "ioperm",
        "swapon",
        "swapoff",
        "reboot",
        "settimeofday",
        "clock_settime",
        "clock_adjtime",
        "adjtimex",
        "nfsservctl",
        "vhangup",
        "process_vm_writev",
        "kcmp",
    ]
}

/// What the browser's sandbox needs from the session it starts in.
#[derive(Clone, Debug)]
pub struct Session {
    pub home: PathBuf,
    pub runtime_dir: PathBuf,
    /// The Wayland socket's name in `runtime_dir`.
    pub wayland_display: String,
    /// The filtered session bus socket xdg-dbus-proxy listens on, bound
    /// into the sandbox as the session bus.
    pub bus_proxy: Option<PathBuf>,
    /// The socket the launcher, outside the sandbox, asks security keys
    /// through for the browser (danube's webauthn.rs), bound in as
    /// `danube-keys`. The keys' device nodes come and go as they are
    /// plugged in, which a bind made at start cannot follow, and the whole
    /// of /dev would hand the browser every device the person may open.
    pub keys: Option<PathBuf>,
    /// Where losos-adblock compiles its lists.
    pub adblock_state: PathBuf,
    /// The document portal's view for Danube
    /// (`$XDG_RUNTIME_DIR/doc/by-app/<app id>`), shown in the sandbox as
    /// `$XDG_RUNTIME_DIR/doc`, where the file chooser's files are.
    pub documents: Option<PathBuf>,
    /// More to show read-only: the folder of a page shown as the desktop
    /// background.
    pub read_only: Vec<PathBuf>,
    /// The program (and its arguments) to run inside.
    pub program: Vec<String>,
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The browser's sandbox for `session`. Danube's own directories are
/// created first, so they can be bound.
pub fn plan(session: &Session) -> Plan {
    plan_with_gpu(session, gpu_draws(Path::new("/dev/dri")))
}

fn plan_with_gpu(session: &Session, gpu_can_draw: bool) -> Plan {
    let home = &session.home;
    let run = &session.runtime_dir;
    let mut plan = Plan {
        // No network namespace: the network process needs the host's.
        unshare: vec![Unshare::Pid, Unshare::Ipc, Unshare::Uts],
        deny_syscalls: syscall_denylist(),
        no_inherited_fds: true,
        program: session.program.clone(),
        chdir: Some(s(home)),
        ..Plan::default()
    };
    fn bind(plan: &mut Plan, src: &Path, access: Access, optional: bool) {
        plan.ops.push(Op::Bind {
            src: s(src),
            dest: s(src),
            ro: access == Access::Read,
            dev: access == Access::Device,
            optional,
        });
    }

    // The system, read-only: every program and library, the configuration
    // they read (fonts, certificates, resolv.conf, time zone, passwd), the
    // GPU driver NixOS links at /run/opengl-driver, and sysfs, which Mesa
    // and libdrm read to find the GPU.
    for path in [
        "/nix/store",
        "/run/current-system",
        "/run/opengl-driver",
        "/run/opengl-driver-32",
        "/etc",
        "/sys",
        // glibc's NSS asks resolved over its Varlink socket; every name
        // still goes through the adblock forwarder.
        "/run/systemd/resolve",
        // Danube reads the compiled filter lists.
        &s(&session.adblock_state),
    ] {
        bind(&mut plan, Path::new(path), Access::Read, true);
    }
    if gpu_can_draw {
        bind(&mut plan, Path::new("/dev/dri"), Access::Device, true);
    } else {
        info!("the GPU cannot draw; pages render in software");
    }

    plan.ops.push(Op::Dev("/dev".into()));
    plan.ops.push(Op::Proc("/proc".into()));
    plan.ops.push(Op::Tmpfs("/tmp".into()));

    // An empty runtime directory with only the sockets it needs.
    plan.ops.push(Op::Tmpfs(s(run)));
    for name in [
        session.wayland_display.as_str(),
        "pipewire-0",
        "pulse/native",
        // derisk's agent socket and theme: menus in the palette, the theme.
        "derisk",
        // WebKit puts its own sockets for the web processes' D-Bus proxy
        // here; the runtime tmpfs already allows that.
    ] {
        let path = run.join(name);
        plan.ops.push(Op::Bind {
            src: s(&path),
            dest: s(&path),
            ro: false,
            dev: false,
            optional: true,
        });
    }
    if let Some(proxy) = &session.bus_proxy {
        plan.ops.push(Op::Bind {
            src: s(proxy),
            dest: s(&run.join("bus")),
            ro: false,
            dev: false,
            optional: false,
        });
        plan.env.push((
            "DBUS_SESSION_BUS_ADDRESS".into(),
            Some(format!("unix:path={}", s(&run.join("bus")))),
        ));
    } else {
        plan.env.push(("DBUS_SESSION_BUS_ADDRESS".into(), None));
    }
    if let Some(documents) = &session.documents {
        plan.ops.push(Op::Bind {
            src: s(documents),
            dest: s(&run.join("doc")),
            ro: false,
            dev: false,
            optional: true,
        });
    }

    // An empty home with Danube's directories and the user's own fonts,
    // themes and icons in it.
    plan.ops.push(Op::Tmpfs(s(home)));
    for dir in [
        ".local/share/danube",
        ".cache/danube",
        ".config/danube",
        "Downloads",
    ] {
        let path = home.join(dir);
        let _ = std::fs::create_dir_all(&path);
        bind(&mut plan, &path, Access::Write, false);
    }
    for dir in [
        ".local/share/fonts",
        ".fonts",
        ".config/fontconfig",
        ".local/share/derisk",
        ".local/share/icons",
        ".icons",
        // Theme and settings files derisk writes for other toolkits.
        ".config/derisk",
    ] {
        bind(&mut plan, &home.join(dir), Access::Read, true);
    }
    // After the home's tmpfs, so that a folder in it shows through.
    for path in &session.read_only {
        bind(&mut plan, path, Access::Read, true);
    }

    if let Some(keys) = &session.keys {
        let inside = run.join("danube-keys");
        plan.ops.push(Op::Bind {
            src: s(keys),
            dest: s(&inside),
            ro: false,
            dev: false,
            optional: false,
        });
        plan.env.push(("DANUBE_KEYS".into(), Some(s(&inside))));
    }

    // Marks the inside, so the browser does not try to sandbox itself again.
    plan.env.push(("DANUBE_SANDBOXED".into(), Some("1".into())));
    plan
}

/// The sandbox xdg-dbus-proxy runs in: the Nix store to run from, the
/// runtime directory (the session bus and the proxy's own socket) and
/// `bus_dir` when the bus is elsewhere, and `/.flatpak-info` saying
/// `app_info`, which the portals read through the proxy's process to know
/// which app is asking, and that it is sandboxed.
pub fn bus_proxy(
    runtime_dir: &Path,
    bus_dir: Option<&Path>,
    app_info: &str,
    program: Vec<String>,
) -> Plan {
    let mut plan = Plan {
        // A PID namespace of its own too: Hakoniwa mounts a /proc in
        // every container, which a shared one refuses.
        unshare: vec![Unshare::Pid, Unshare::Ipc, Unshare::Uts],
        deny_syscalls: syscall_denylist(),
        no_inherited_fds: true,
        program,
        ..Plan::default()
    };
    for path in ["/nix/store", "/etc"] {
        plan.ops.push(Op::Bind {
            src: path.into(),
            dest: path.into(),
            ro: true,
            dev: false,
            optional: true,
        });
    }
    plan.ops.push(Op::Dev("/dev".into()));
    for dir in std::iter::once(runtime_dir).chain(bus_dir.filter(|d| !d.starts_with(runtime_dir))) {
        plan.ops.push(Op::Bind {
            src: s(dir),
            dest: s(dir),
            ro: false,
            dev: false,
            optional: false,
        });
    }
    plan.ops.push(Op::File {
        dest: "/.flatpak-info".into(),
        contents: app_info.to_owned(),
    });
    plan
}

/// Whether Mesa can draw with this machine's GPU, so the browser gets
/// /dev/dri. A virtual machine's virtio-gpu without 3D (QEMU's `-vga
/// virtio`, virt-manager with 3D acceleration off) still has a render
/// node. Mesa has no driver for it and falls back to drawing in software on
/// that node, where the kernel refuses the dumb buffers software drawing
/// needs, so WebKit's GBM buffers all fail and every page stays blank.
/// Without /dev/dri, Mesa draws in software on its own and WebKit hands
/// pages over in shared memory, which works anywhere. A render node of any
/// other kind, or a machine with only primary nodes, keeps the GPU.
fn gpu_draws(dri: &Path) -> bool {
    gpu_draws_with(dri, virtio_3d_features)
}

fn gpu_draws_with(dri: &Path, mut probe: impl FnMut(&Path) -> Option<libc::c_int>) -> bool {
    let Ok(entries) = std::fs::read_dir(dri) else {
        return true;
    };
    let mut nodes = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .peekable();
    nodes.peek().is_none() || nodes.any(|e| !matches!(probe(&e.path()), Some(0)))
}

/// The kernel's `struct drm_version`.
#[repr(C)]
struct DrmVersion {
    major: libc::c_int,
    minor: libc::c_int,
    patchlevel: libc::c_int,
    name_len: libc::size_t,
    name: *mut libc::c_char,
    date_len: libc::size_t,
    date: *mut libc::c_char,
    desc_len: libc::size_t,
    desc: *mut libc::c_char,
}

/// The kernel's `struct drm_virtgpu_getparam`: `value` points at an int
/// the answer is written to.
#[repr(C)]
struct VirtgpuGetparam {
    param: u64,
    value: u64,
}

/// `_IOWR('d', nr, size)`, the same encoding on x86_64 and aarch64.
const fn drm_iowr(nr: libc::c_ulong, size: usize) -> libc::c_ulong {
    (3 << 30) | ((size as libc::c_ulong) << 16) | ((b'd' as libc::c_ulong) << 8) | nr
}

/// Virtio-gpu's 3D features, or `None` if the node is not a working virtio-gpu node.
fn virtio_3d_features(node: &Path) -> Option<libc::c_int> {
    const DRM_IOCTL_VERSION: libc::c_ulong = drm_iowr(0x00, size_of::<DrmVersion>());
    // DRM_COMMAND_BASE + DRM_VIRTGPU_GETPARAM.
    const DRM_IOCTL_VIRTGPU_GETPARAM: libc::c_ulong =
        drm_iowr(0x40 + 0x03, size_of::<VirtgpuGetparam>());
    const VIRTGPU_PARAM_3D_FEATURES: u64 = 1;

    let Ok(file) = OpenOptions::new().read(true).write(true).open(node) else {
        return None;
    };
    let mut name = [0u8; 16];
    let mut version = DrmVersion {
        major: 0,
        minor: 0,
        patchlevel: 0,
        name_len: name.len(),
        name: name.as_mut_ptr().cast(),
        date_len: 0,
        date: std::ptr::null_mut(),
        desc_len: 0,
        desc: std::ptr::null_mut(),
    };
    // SAFETY: the kernel writes at most name_len bytes into `name` and
    // nothing into the empty date and description.
    if unsafe { libc::ioctl(file.as_raw_fd(), DRM_IOCTL_VERSION, &mut version) } != 0 {
        return None;
    }
    if name.get(..version.name_len) != Some(b"virtio_gpu".as_slice()) {
        return None;
    }
    let mut features: libc::c_int = 0;
    let mut param = VirtgpuGetparam {
        param: VIRTGPU_PARAM_3D_FEATURES,
        value: &mut features as *mut libc::c_int as u64,
    };
    // SAFETY: the kernel writes one int to `features`.
    (unsafe { libc::ioctl(file.as_raw_fd(), DRM_IOCTL_VIRTGPU_GETPARAM, &mut param) } == 0)
        .then_some(features)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(root: &Path) -> Session {
        Session {
            home: root.join("home"),
            runtime_dir: root.join("run"),
            wayland_display: "wayland-0".into(),
            bus_proxy: None,
            keys: None,
            adblock_state: "/var/lib/losos-adblock".into(),
            documents: None,
            read_only: vec![],
            program: vec!["/bin/danube".into()],
        }
    }

    fn has_dri_bind(plan: &Plan) -> bool {
        plan.ops.iter().any(|op| {
            matches!(
                op,
                Op::Bind {
                    src,
                    dest,
                    dev: true,
                    ..
                } if src == "/dev/dri" && dest == "/dev/dri"
            )
        })
    }

    #[test]
    fn virtio_gpu_without_3d_uses_software_and_omits_dri_bind() {
        let root = std::env::temp_dir().join(format!("danube-no-3d-{}", std::process::id()));
        let dri = root.join("dri");
        std::fs::create_dir_all(&dri).unwrap();
        std::fs::write(dri.join("renderD128"), "").unwrap();

        let gpu_can_draw = gpu_draws_with(&dri, |_| Some(0));
        assert!(!gpu_can_draw);
        assert!(!has_dri_bind(&plan_with_gpu(&session(&root), gpu_can_draw)));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn virtio_gpu_with_3d_is_kept() {
        let dir = std::env::temp_dir().join(format!("danube-3d-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("renderD128"), "").unwrap();

        assert!(gpu_draws_with(&dir, |_| Some(1)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_drawable_render_node_keeps_a_mixed_gpu_setup() {
        let dir = std::env::temp_dir().join(format!("danube-mixed-gpu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("renderD128"), "").unwrap();
        std::fs::write(dir.join("renderD129"), "").unwrap();

        assert!(gpu_draws_with(&dir, |node| {
            match node.file_name().unwrap().to_str().unwrap() {
                "renderD128" => Some(0),
                "renderD129" => None,
                _ => unreachable!(),
            }
        }));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_gpu_is_kept_when_no_render_node_is_known_not_to_draw() {
        let dir = std::env::temp_dir().join(format!("danube-dri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // No render node: primary nodes or none, Mesa manages either way.
        assert!(gpu_draws_with(&dir, |_| Some(0)));
        // A render node that answers no DRM ioctl is not virtio-gpu.
        std::fs::write(dir.join("renderD128"), "").unwrap();
        assert!(gpu_draws_with(&dir, |_| None));
        assert!(gpu_draws_with(&dir.join("missing"), |_| Some(0)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn home_holds_only_danubes_directories() {
        let root = std::env::temp_dir().join(format!("danube-plan-{}", std::process::id()));
        let session = Session {
            home: root.join("home"),
            runtime_dir: root.join("run"),
            wayland_display: "wayland-0".into(),
            bus_proxy: None,
            keys: None,
            adblock_state: "/var/lib/losos-adblock".into(),
            documents: Some(root.join("run/doc/by-app/org.losos.Danube")),
            read_only: vec![root.join("home/Pictures/page")],
            program: vec!["/bin/danube".into()],
        };
        let plan = plan(&session);
        let writable: Vec<&str> = plan
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Bind {
                    dest, ro: false, ..
                } => Some(dest.as_str()),
                _ => None,
            })
            .collect();
        for path in &writable {
            let path = Path::new(path);
            assert!(
                !path.starts_with(&session.home)
                    || [
                        ".local/share/danube",
                        ".cache/danube",
                        ".config/danube",
                        "Downloads"
                    ]
                    .iter()
                    .any(|d| path == session.home.join(d)),
                "{path:?} is writable"
            );
        }
        assert!(plan.ops.contains(&Op::Tmpfs(s(&session.home))));
        assert!(plan.no_inherited_fds);
        // WebKit's page sandboxes could not be built under Landlock.
        assert!(plan.landlock.is_empty());
        // A background page's folder shows, read-only, after the home's
        // tmpfs that would hide it.
        let page = s(&session.read_only[0]);
        let tmpfs = plan
            .ops
            .iter()
            .position(|op| *op == Op::Tmpfs(s(&session.home)));
        let bound = plan
            .ops
            .iter()
            .position(|op| matches!(op, Op::Bind { src, ro: true, .. } if *src == page));
        assert!(
            bound > tmpfs,
            "{page} is not bound read-only after the home"
        );
        // Danube's documents are where an app's own are in a Flatpak.
        assert!(plan.ops.iter().any(|op| matches!(op,
            Op::Bind { src, dest, .. } if src.ends_with("by-app/org.losos.Danube") && *dest == s(&root.join("run/doc")))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_bus_proxy_says_which_app_it_is() {
        let plan = bus_proxy(
            Path::new("/run/user/1000"),
            Some(Path::new("/run/user/1000")),
            "[Application]\nname=org.losos.Danube\n",
            vec!["/nix/store/x/bin/xdg-dbus-proxy".into()],
        );
        assert!(plan.ops.contains(&Op::File {
            dest: "/.flatpak-info".into(),
            contents: "[Application]\nname=org.losos.Danube\n".into(),
        }));
        // The runtime directory once, writable: the proxy makes its socket
        // there.
        let runtime: Vec<_> = plan
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Bind { dest, ro: false, .. } if dest == "/run/user/1000"))
            .collect();
        assert_eq!(runtime.len(), 1);
        // The home is not there at all, nor the launcher's descriptors,
        // its connection to the whole session bus among them.
        assert!(!plan
            .ops
            .iter()
            .any(|op| matches!(op, Op::Bind { src, .. } if src.starts_with("/home"))));
        assert!(plan.no_inherited_fds);
        // A PID namespace, without which Hakoniwa cannot mount /proc.
        assert!(plan.unshare.contains(&Unshare::Pid));
    }
}
