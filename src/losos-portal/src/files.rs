//! Files on their way between the host and a machine.
//!
//! On the host, the relay runs this program as the person to read, write
//! or capture anything ([`read`], [`write`], [`screenshot`]), so the kernel
//! checks the person's own access, never root's ([`read`], [`write`],
//! [`remove`], [`screenshot`]). In a machine, `forward` keeps the copies it
//! was handed in [`Staging`] and writes each back to the host whenever an
//! app saves it; in a folder's copy, new files go back too, and removed
//! ones are removed.

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

use crate::{read_json, receive_stream, safe_name, send_stream, write_json, Reply, Request};

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
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".losos-portal-{}", std::process::id()));
    let temp = parent.join(temp_name);
    let mode = fs::metadata(path)
        .map(|m| m.permissions().mode())
        .unwrap_or(0o644);
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

/// `losos-portal remove PATH`: a file, or a folder with everything in it,
/// that an app removed from its copy.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        // Gone already: what was asked for is so.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
            reply["error"]
                .as_str()
                .unwrap_or("the session refused")
                .to_owned(),
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
/// Flatpak). An index there remembers which host file or folder each one
/// is, so saving keeps working after `forward` starts again.
pub struct Staging {
    root: PathBuf,
    inotify: OwnedFd,
    watches: Mutex<HashMap<i32, Watch>>,
    next: AtomicU64,
}

/// One file or folder handed to the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Copy {
    local: PathBuf,
    host: PathBuf,
}

impl Copy {
    fn child(&self, name: &std::ffi::OsStr) -> Copy {
        Copy {
            local: self.local.join(name),
            host: self.host.join(name),
        }
    }
}

/// What one inotify watch is for.
#[derive(Clone, Debug)]
enum Watch {
    /// A file picked alone, watched through its folder.
    File(Copy),
    /// One folder of a folder's copy, all of whose files go back.
    Tree(Copy),
}

/// What goes back to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Change {
    Store(Copy),
    Remove(PathBuf),
}

/// What a tree's folders are watched for: a file written or moved in, a
/// folder made, anything removed or moved out.
const TREE_EVENTS: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_TO
    | libc::IN_MOVED_FROM
    | libc::IN_CREATE
    | libc::IN_DELETE;

impl Staging {
    pub fn open() -> io::Result<Staging> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| io::Error::other("HOME is not set"))?;
        let staging = Staging::at(data.join("losos-portal"))?;
        for line in fs::read_to_string(staging.index())
            .unwrap_or_default()
            .lines()
        {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                [local, host] => {
                    let copy = Copy {
                        local: local.into(),
                        host: host.into(),
                    };
                    if copy.local.parent().is_some_and(Path::exists) {
                        let _ = staging.watch(copy);
                    }
                }
                ["tree", local, host] => {
                    let copy = Copy {
                        local: local.into(),
                        host: host.into(),
                    };
                    if copy.local.is_dir() {
                        let _ = staging.watch_tree(&copy);
                    }
                }
                _ => {}
            }
        }
        Ok(staging)
    }

    fn at(root: PathBuf) -> io::Result<Staging> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        // SAFETY: a plain syscall; the fd is owned at once.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Staging {
            root,
            // SAFETY: fd is fresh and nothing else owns it.
            inotify: unsafe { OwnedFd::from_raw_fd(fd) },
            watches: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        })
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
        let dir = self.root.join(format!(
            "{now}-{}",
            self.next.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(dir)
    }

    /// The local copy of `host`, fetched through `connect` when `fetch`
    /// (a file opened, not one about to be saved), and written back
    /// whenever it changes: a file whenever it is saved, a folder's files
    /// whenever one is saved, added or removed.
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
        let copy = Copy {
            local: local.clone(),
            host: host.to_owned(),
        };
        let mut index = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index())?;
        if folder {
            self.watch_tree(&copy)?;
            writeln!(
                index,
                "tree\t{}\t{}",
                copy.local.display(),
                copy.host.display()
            )?;
        } else {
            self.watch(copy.clone())?;
            writeln!(index, "{}\t{}", copy.local.display(), copy.host.display())?;
        }
        Ok(local)
    }

    fn add_watch(&self, dir: &Path, mask: u32) -> io::Result<i32> {
        let dir = CString::new(dir.as_os_str().as_bytes())?;
        // SAFETY: dir is NUL-ended; the fd is ours.
        let wd = unsafe { libc::inotify_add_watch(self.inotify.as_raw_fd(), dir.as_ptr(), mask) };
        if wd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(wd)
        }
    }

    /// Watches the folder `dir` and every folder in it, and returns the
    /// files already there, for a folder that came into the copy with
    /// files in it.
    fn watch_tree(&self, dir: &Copy) -> io::Result<Vec<Copy>> {
        let mut files = Vec::new();
        let mut pending = vec![dir.clone()];
        while let Some(dir) = pending.pop() {
            let wd = self.add_watch(&dir.local, TREE_EVENTS)?;
            self.watches
                .lock()
                .unwrap()
                .insert(wd, Watch::Tree(dir.clone()));
            for entry in fs::read_dir(&dir.local)?.flatten() {
                let child = dir.child(&entry.file_name());
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => pending.push(child),
                    Ok(kind) if kind.is_file() => files.push(child),
                    // Links and the rest stay in the machine.
                    _ => {}
                }
            }
        }
        Ok(files)
    }

    /// What one event asks to go back to the host.
    fn changes(&self, wd: i32, mask: u32, name: &[u8]) -> Vec<Change> {
        if mask & libc::IN_IGNORED != 0 {
            self.watches.lock().unwrap().remove(&wd);
            return Vec::new();
        }
        let Some(watch) = self.watches.lock().unwrap().get(&wd).cloned() else {
            return Vec::new();
        };
        let name = std::ffi::OsStr::from_bytes(name);
        match watch {
            Watch::File(copy) => {
                let written = mask & (libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO) != 0;
                if written && copy.local.file_name() == Some(name) {
                    vec![Change::Store(copy)]
                } else {
                    Vec::new()
                }
            }
            Watch::Tree(dir) => {
                if name.is_empty() {
                    return Vec::new();
                }
                let child = dir.child(name);
                let removed = mask & (libc::IN_DELETE | libc::IN_MOVED_FROM) != 0;
                if removed {
                    return vec![Change::Remove(child.host)];
                }
                if mask & libc::IN_ISDIR != 0 {
                    if mask & (libc::IN_CREATE | libc::IN_MOVED_TO) == 0 {
                        return Vec::new();
                    }
                    // A folder made or moved in: watch it, and send what
                    // is already in it.
                    match self.watch_tree(&child) {
                        Ok(files) => files.into_iter().map(Change::Store).collect(),
                        Err(e) => {
                            warn!(folder = %child.local.display(), error = %e, "not watched");
                            Vec::new()
                        }
                    }
                } else if mask & (libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO) != 0 {
                    vec![Change::Store(child)]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn watch(&self, copy: Copy) -> io::Result<()> {
        let dir = copy.local.parent().unwrap_or(Path::new("/"));
        let wd = self.add_watch(dir, libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO)?;
        self.watches.lock().unwrap().insert(wd, Watch::File(copy));
        Ok(())
    }

    /// Writes each copy back to the host after the app saved it, and
    /// removes what it removed from a folder's copy, for as long as the
    /// program runs.
    pub fn write_back<S: Read + Write>(&self, connect: impl Fn() -> io::Result<S>) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            // One change for each host path, the last one asked for, in
            // the order the paths came: a file saved through a temporary
            // one is the file stored, and the temporary one removed, or
            // nothing when it never reached the host.
            let mut changed: Vec<Change> = Vec::new();
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
                for (wd, mask, name) in events(&buf[..n as usize]) {
                    for change in self.changes(wd, mask, &name) {
                        let host = match &change {
                            Change::Store(copy) => copy.host.clone(),
                            Change::Remove(host) => host.clone(),
                        };
                        let same = |c: &Change| match c {
                            Change::Store(copy) => copy.host == host,
                            Change::Remove(h) => *h == host,
                        };
                        match changed.iter_mut().find(|c| same(c)) {
                            Some(earlier) => *earlier = change,
                            None => changed.push(change),
                        }
                    }
                }
                wait = 300;
            }
            for change in changed {
                match change {
                    // Gone again before it could go back; its removal, if
                    // it had reached the host, is a change of its own.
                    Change::Store(copy) if !copy.local.is_file() => {}
                    Change::Store(copy) => match store(&copy, &connect) {
                        Ok(()) => info!(host = %copy.host.display(), "written back"),
                        Err(e) => {
                            warn!(host = %copy.host.display(), error = %e, "not written back")
                        }
                    },
                    Change::Remove(host) => match remove_on_host(&host, &connect) {
                        Ok(()) => info!(host = %host.display(), "removed"),
                        Err(e) => warn!(host = %host.display(), error = %e, "not removed"),
                    },
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

fn remove_on_host<S: Read + Write>(
    host: &Path,
    connect: impl Fn() -> io::Result<S>,
) -> io::Result<()> {
    let mut relay = connect()?;
    write_json(
        &mut relay,
        &Request::Remove {
            path: host.to_string_lossy().into_owned(),
        },
    )?;
    let reply: Reply = read_json(&mut relay)?;
    if reply.ok {
        Ok(())
    } else {
        Err(io::Error::other(reply.message.unwrap_or_default()))
    }
}

/// The watch, mask and file name of each inotify event in `buf`.
fn events(buf: &[u8]) -> Vec<(i32, u32, Vec<u8>)> {
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
        out.push((event.wd, event.mask, name));
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
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Nothing left beside it.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // A new file in a new folder, as an upload is.
        let upload = dir.join("uploads/work/p.png");
        write_from(&upload, &mut &b"png"[..]).unwrap();
        assert_eq!(fs::read(&upload).unwrap(), b"png");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Every change the events so far ask for.
    fn drain(staging: &Staging) -> Vec<Change> {
        let mut buf = vec![0u8; 64 * 1024];
        let mut out = Vec::new();
        loop {
            let mut poll = libc::pollfd {
                fd: staging.inotify.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(&mut poll, 1, 200) } <= 0 {
                return out;
            }
            // SAFETY: buf is valid for writes of its length.
            let n = unsafe {
                libc::read(
                    staging.inotify.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                )
            };
            for (wd, mask, name) in events(&buf[..n.max(0) as usize]) {
                out.extend(staging.changes(wd, mask, &name));
            }
        }
    }

    #[test]
    fn a_folders_copy_goes_back_whole() {
        let dir = std::env::temp_dir().join(format!("losos-portal-tree-{}", std::process::id()));
        let staging = Staging::at(dir.join("staging")).unwrap();
        let tree = Copy {
            local: dir.join("copy/project"),
            host: "/home/a/project".into(),
        };
        fs::create_dir_all(tree.local.join("src")).unwrap();
        fs::write(tree.local.join("src/main.rs"), "fn main() {}").unwrap();
        assert_eq!(
            staging.watch_tree(&tree).unwrap(),
            [tree.child("src".as_ref()).child("main.rs".as_ref())]
        );

        // A file saved in a folder of it.
        fs::write(tree.local.join("src/main.rs"), "fn main() { }").unwrap();
        // A new folder with a file in it.
        fs::create_dir(tree.local.join("docs")).unwrap();
        fs::write(tree.local.join("docs/README"), "hi").unwrap();
        let changes = drain(&staging);
        let host = Path::new("/home/a/project");
        assert!(changes.contains(&Change::Store(Copy {
            local: tree.local.join("src/main.rs"),
            host: host.join("src/main.rs"),
        })));
        assert!(changes.contains(&Change::Store(Copy {
            local: tree.local.join("docs/README"),
            host: host.join("docs/README"),
        })));

        // A file removed.
        fs::remove_file(tree.local.join("src/main.rs")).unwrap();
        assert_eq!(drain(&staging), [Change::Remove(host.join("src/main.rs"))]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removes_files_and_folders() {
        let dir = std::env::temp_dir().join(format!("losos-portal-remove-{}", std::process::id()));
        fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        fs::write(dir.join("sub/deeper/x"), "").unwrap();
        fs::write(dir.join("f"), "").unwrap();
        remove(&dir.join("f")).unwrap();
        remove(&dir.join("sub")).unwrap();
        // Gone already is done.
        remove(&dir.join("f")).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_inotify_events() {
        let head = std::mem::size_of::<libc::inotify_event>();
        let mut buf = vec![0u8; head + 16];
        buf[..4].copy_from_slice(&7i32.to_ne_bytes());
        buf[4..8].copy_from_slice(&libc::IN_CLOSE_WRITE.to_ne_bytes());
        buf[12..16].copy_from_slice(&16u32.to_ne_bytes());
        buf[head..head + 5].copy_from_slice(b"a.txt");
        assert_eq!(events(&buf), [(7, libc::IN_CLOSE_WRITE, b"a.txt".to_vec())]);
    }
}
