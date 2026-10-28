//! Files on their way between the host and a machine.
//!
//! On the host, the relay runs this program as the person to read, write
//! or capture anything ([`read`], [`write`], [`screenshot`]), so the kernel
//! checks the person's own access, never root's. In a machine, `forward`
//! keeps the copies it was handed in [`Staging`] and writes each back to
//! the host whenever an app saves it.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::{
    read_json, receive_stream, safe_name, send_stream, write_json, Reply, Request,
};

/// `losos-portal read PATH`: `file` or `dir` on a line, then the file's
/// bytes or a tar stream of the folder.
pub fn read(path: &Path) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    let mut out = io::stdout().lock();
    if meta.is_dir() {
        writeln!(out, "dir")?;
        out.flush()?;
        let status = Command::new("tar")
            .arg("-C")
            .arg(path)
            .args(["-cf", "-", "."])
            .stdin(Stdio::null())
            .status()?;
        return if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("tar failed"))
        };
    }
    let mut file = File::open(path)?;
    writeln!(out, "file")?;
    io::copy(&mut file, &mut out)?;
    out.flush()
}

/// `losos-portal write PATH`: standard input into `PATH`, whole or not at
/// all: into a file beside it, then renamed over it, keeping its mode.
pub fn write(path: &Path) -> io::Result<()> {
    write_from(path, &mut io::stdin().lock())
}

fn write_from(path: &Path, input: &mut impl Read) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("no folder to write in"))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".losos-portal-{}", std::process::id()));
    let temp = parent.join(temp_name);
    let mode = fs::metadata(path).map(|m| m.permissions().mode()).unwrap_or(0o644);
    let result = (|| {
        let mut file = File::create(&temp)?;
        io::copy(input, &mut file)?;
        file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// `losos-portal screenshot`: the screen from the person's derisk
/// session, the way derisk's own portal asks for it, as for [`read`].
pub fn screenshot() -> io::Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR is not set"))?;
    let socket = Path::new(&runtime).join("derisk/agent.sock");
    let mut stream = std::os::unix::net::UnixStream::connect(&socket)?;
    stream.write_all(b"{\"method\":\"screenshot\"}\n")?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let reply: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if reply["ok"] != serde_json::Value::Bool(true) {
        return Err(io::Error::other(
            reply["error"].as_str().unwrap_or("the session refused").to_owned(),
        ));
    }
    let path = reply["result"]["path"]
        .as_str()
        .ok_or_else(|| io::Error::other("the session returned no screenshot"))?;
    read(Path::new(path))
}

/// Where a machine keeps the files it was handed, and writes them back
/// from: `$XDG_DATA_HOME/losos-portal/<n>/<name>`, one folder each, which
/// the frontend hands the app (through the document portal, for a
/// Flatpak). An index there remembers which host file each one is, so
/// saving keeps working after `forward` starts again.
pub struct Staging {
    root: PathBuf,
    inotify: OwnedFd,
    watches: Mutex<HashMap<i32, Copy>>,
    next: AtomicU64,
}

/// One file handed to the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Copy {
    local: PathBuf,
    host: PathBuf,
}

impl Staging {
    pub fn open() -> io::Result<Staging> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| io::Error::other("HOME is not set"))?;
        let root = data.join("losos-portal");
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&root)?;
        // SAFETY: a plain syscall; the fd is owned at once.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let staging = Staging {
            root,
            // SAFETY: fd is fresh and nothing else owns it.
            inotify: unsafe { OwnedFd::from_raw_fd(fd) },
            watches: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        };
        for line in fs::read_to_string(staging.index()).unwrap_or_default().lines() {
            if let Some((local, host)) = line.split_once('\t') {
                let copy = Copy {
                    local: local.into(),
                    host: host.into(),
                };
                if copy.local.parent().is_some_and(Path::exists) {
                    let _ = staging.watch(copy);
                }
            }
        }
        Ok(staging)
    }

    fn index(&self) -> PathBuf {
        self.root.join("index")
    }

    /// A new folder for one file.
    fn folder(&self) -> io::Result<PathBuf> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let dir = self
            .root
            .join(format!("{now}-{}", self.next.fetch_add(1, Ordering::Relaxed)));
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(dir)
    }

    /// The local copy of `host`, fetched through `connect` when `fetch`
    /// (a file opened, not one about to be saved), and written back
    /// whenever it changes unless it is a folder.
    pub fn place<S: Read + Write>(
        &self,
        host: &Path,
        fetch: bool,
        connect: impl Fn() -> io::Result<S>,
    ) -> io::Result<PathBuf> {
        let name = safe_name(&host.file_name().unwrap_or_default().to_string_lossy());
        let local = self.folder()?.join(name);
        let mut folder = false;
        if fetch {
            let mut relay = connect()?;
            write_json(
                &mut relay,
                &Request::Fetch {
                    path: host.to_string_lossy().into_owned(),
                },
            )?;
            let reply: Reply = read_json(&mut relay)?;
            if !reply.ok {
                return Err(io::Error::other(reply.message.unwrap_or_default()));
            }
            if reply.kind.as_deref() == Some("dir") {
                folder = true;
                fs::create_dir(&local)?;
                let mut tar = Command::new("tar")
                    .arg("-C")
                    .arg(&local)
                    .args(["--no-same-owner", "-xf", "-"])
                    .stdin(Stdio::piped())
                    .spawn()?;
                receive_stream(&mut relay, tar.stdin.as_mut().unwrap())?;
                drop(tar.stdin.take());
                if !tar.wait()?.success() {
                    return Err(io::Error::other("cannot unpack the folder"));
                }
            } else {
                let mut file = File::create(&local)?;
                receive_stream(&mut relay, &mut file)?;
                file.sync_all()?;
            }
        }
        // A folder is a copy to read: changes in it stay here.
        if !folder {
            let copy = Copy {
                local: local.clone(),
                host: host.to_owned(),
            };
            self.watch(copy.clone())?;
            let mut index = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.index())?;
            writeln!(index, "{}\t{}", copy.local.display(), copy.host.display())?;
        }
        Ok(local)
    }

    fn watch(&self, copy: Copy) -> io::Result<()> {
        let dir = copy.local.parent().unwrap_or(Path::new("/"));
        let dir = CString::new(dir.as_os_str().as_bytes())?;
        // SAFETY: dir is NUL-ended; the fd is ours.
        let wd = unsafe {
            libc::inotify_add_watch(
                self.inotify.as_raw_fd(),
                dir.as_ptr(),
                libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO,
            )
        };
        if wd < 0 {
            return Err(io::Error::last_os_error());
        }
        self.watches.lock().unwrap().insert(wd, copy);
        Ok(())
    }

    /// Writes each copy back to the host after the app saved it, for as
    /// long as the program runs.
    pub fn write_back<S: Read + Write>(&self, connect: impl Fn() -> io::Result<S>) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut changed: Vec<Copy> = Vec::new();
            // The first event, then whatever comes within a moment of it:
            // an app saving often writes, renames and writes again.
            let mut wait = -1;
            loop {
                let mut poll = libc::pollfd {
                    fd: self.inotify.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                let ready = unsafe { libc::poll(&mut poll, 1, wait) };
                if ready <= 0 {
                    break;
                }
                // SAFETY: buf is valid for writes of its length.
                let n = unsafe {
                    libc::read(self.inotify.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n <= 0 {
                    break;
                }
                for (wd, name) in events(&buf[..n as usize]) {
                    if let Some(copy) = self.watches.lock().unwrap().get(&wd) {
                        if copy.local.file_name().map(|n| n.as_bytes()) == Some(name.as_slice())
                            && !changed.contains(copy)
                        {
                            changed.push(copy.clone());
                        }
                    }
                }
                wait = 300;
            }
            for copy in changed {
                match store(&copy, &connect) {
                    Ok(()) => info!(host = %copy.host.display(), "written back"),
                    Err(e) => warn!(host = %copy.host.display(), error = %e, "not written back"),
                }
            }
            if wait < 0 {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn store<S: Read + Write>(copy: &Copy, connect: impl Fn() -> io::Result<S>) -> io::Result<()> {
    let mut file = File::open(&copy.local)?;
    let mut relay = connect()?;
    write_json(
        &mut relay,
        &Request::Store {
            path: copy.host.to_string_lossy().into_owned(),
        },
    )?;
    send_stream(&mut relay, &mut file)?;
    let reply: Reply = read_json(&mut relay)?;
    if reply.ok {
        Ok(())
    } else {
        Err(io::Error::other(reply.message.unwrap_or_default()))
    }
}

/// The watch and file name of each inotify event in `buf`.
fn events(buf: &[u8]) -> Vec<(i32, Vec<u8>)> {
    let head = std::mem::size_of::<libc::inotify_event>();
    let mut out = Vec::new();
    let mut at = 0;
    while at + head <= buf.len() {
        // SAFETY: at least `head` bytes remain; read_unaligned copes with
        // the buffer's alignment.
        let event: libc::inotify_event =
            unsafe { std::ptr::read_unaligned(buf[at..].as_ptr().cast()) };
        let name_end = (at + head + event.len as usize).min(buf.len());
        let name = &buf[at + head..name_end];
        let name = name.split(|b| *b == 0).next().unwrap_or(&[]).to_vec();
        out.push((event.wd, name));
        at = name_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_whole_files_keeping_the_mode() {
        let dir = std::env::temp_dir().join(format!("losos-portal-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write_from(&path, &mut &b"new"[..]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // Nothing left beside it.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // A new file in a new folder, as an upload is.
        let upload = dir.join("uploads/work/p.png");
        write_from(&upload, &mut &b"png"[..]).unwrap();
        assert_eq!(fs::read(&upload).unwrap(), b"png");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_inotify_events() {
        let head = std::mem::size_of::<libc::inotify_event>();
        let mut buf = vec![0u8; head + 16];
        buf[..4].copy_from_slice(&7i32.to_ne_bytes());
        buf[12..16].copy_from_slice(&16u32.to_ne_bytes());
        buf[head..head + 5].copy_from_slice(b"a.txt");
        assert_eq!(events(&buf), [(7, b"a.txt".to_vec())]);
    }
}
