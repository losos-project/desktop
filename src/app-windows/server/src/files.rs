//! Files a host hands over, such as a disk image or an installer package
//! downloaded there: written into the person's Downloads folder as the
//! pieces come, and opened once whole, as a double click in the Finder
//! would open them.
//!
//! Plain file handling, so it is tested on any system; only `open` is
//! macOS's.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use app_windows_protocol::{FileData, FileSaved, FileStart};

/// What one connection is sending. Files it never finished are removed
/// when it goes, rather than left half written for someone to open.
pub struct Incoming {
    folder: PathBuf,
    files: HashMap<u32, Receiving>,
}

struct Receiving {
    out: BufWriter<File>,
    path: PathBuf,
    size: u64,
    written: u64,
    open: bool,
}

impl Incoming {
    pub fn new(folder: PathBuf) -> Self {
        Incoming {
            folder,
            files: HashMap::new(),
        }
    }

    pub fn start(&mut self, start: FileStart) -> Result<(), String> {
        if self.files.contains_key(&start.file) {
            return Err(format!("file {} is already being sent", start.file));
        }
        fs::create_dir_all(&self.folder).map_err(|e| format!("{}: {e}", self.folder.display()))?;
        let name = file_name(&start.name)?;
        // A new name rather than over a file already there, as browsers do.
        let (path, file) = (1..1000)
            .find_map(|n| {
                let path = self.folder.join(numbered(&name, n));
                match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(file) => Some(Ok((path, file))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(format!("{}: {e}", path.display()))),
                }
            })
            .unwrap_or_else(|| Err(format!("{name}: too many files of that name")))?;
        self.files.insert(
            start.file,
            Receiving {
                out: BufWriter::with_capacity(1 << 20, file),
                path,
                size: start.size,
                written: 0,
                open: start.open,
            },
        );
        Ok(())
    }

    pub fn data(&mut self, data: FileData) -> Result<(), String> {
        let receiving = self
            .files
            .get_mut(&data.file)
            .ok_or_else(|| format!("no file {} is being sent", data.file))?;
        receiving.written += data.data.len() as u64;
        let result = if receiving.written > receiving.size {
            Err(format!(
                "{}: more than the {} bytes it was said to have",
                receiving.path.display(),
                receiving.size
            ))
        } else {
            receiving
                .out
                .write_all(&data.data)
                .map_err(|e| format!("{}: {e}", receiving.path.display()))
        };
        if result.is_err() {
            self.forget(data.file);
        }
        result
    }

    /// The file is whole: closes it, and opens it if the host asked to.
    /// An error that comes from opening still leaves the file saved.
    pub fn end(&mut self, file: u32) -> Result<(FileSaved, Option<String>), String> {
        let mut receiving = self
            .files
            .remove(&file)
            .ok_or_else(|| format!("no file {file} is being sent"))?;
        let path = receiving.path.clone();
        if receiving.written != receiving.size {
            drop(receiving);
            let _ = fs::remove_file(&path);
            return Err(format!("{}: cut short", path.display()));
        }
        if let Err(e) = receiving.out.flush() {
            drop(receiving);
            let _ = fs::remove_file(&path);
            return Err(format!("{}: {e}", path.display()));
        }
        let opening = if receiving.open {
            Some(open(&path))
        } else {
            None
        };
        let saved = FileSaved {
            file,
            path: path.to_string_lossy().into_owned(),
            opened: matches!(opening, Some(Ok(()))),
        };
        Ok((saved, opening.and_then(Result::err)))
    }

    fn forget(&mut self, file: u32) {
        if let Some(receiving) = self.files.remove(&file) {
            let path = receiving.path.clone();
            drop(receiving);
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        let unfinished: Vec<u32> = self.files.keys().copied().collect();
        for file in unfinished {
            self.forget(file);
        }
    }
}

/// The name a host gave, as a name in one folder: no folders of its own,
/// not hidden, not empty, and short enough for any file system.
fn file_name(given: &str) -> Result<String, String> {
    let name = Path::new(given)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .trim_start_matches('.')
        .trim();
    if name.is_empty() {
        return Err(format!("{given:?} is not a file name"));
    }
    // 255 bytes is the limit almost everywhere; leave room for " (999)".
    let mut end = name.len().min(240);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    Ok(name[..end].to_owned())
}

/// `Name.dmg`, then `Name (2).dmg`, `Name (3).dmg` and on.
fn numbered(name: &str, n: u32) -> String {
    if n == 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({n}).{extension}"),
        _ => format!("{name} ({n})"),
    }
}

/// Opens a file as the Finder would: a disk image is mounted and its
/// window shown, an installer package starts Installer.
fn open(path: &Path) -> Result<(), String> {
    let status = Command::new("/usr/bin/open")
        .arg(path)
        .status()
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("macOS could not open {}", path.display()))
    }
}

/// The person's Downloads folder, where browsers put what they fetch.
pub fn downloads() -> PathBuf {
    Path::new(&std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into())).join("Downloads")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("app-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn start(incoming: &mut Incoming, file: u32, name: &str, size: u64) {
        incoming
            .start(FileStart {
                file,
                name: name.into(),
                size,
                open: false,
            })
            .unwrap();
    }

    #[test]
    fn saves_pieces_under_a_free_name() {
        let dir = folder("pieces");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Tool.dmg"), b"older").unwrap();
        let mut incoming = Incoming::new(dir.clone());
        start(&mut incoming, 1, "../../Tool.dmg", 6);
        for piece in [&b"abc"[..], b"def"] {
            incoming
                .data(FileData {
                    file: 1,
                    data: piece.to_vec(),
                })
                .unwrap();
        }
        let (saved, failed) = incoming.end(1).unwrap();
        assert_eq!(saved.path, dir.join("Tool (2).dmg").to_string_lossy());
        assert!(!saved.opened && failed.is_none());
        assert_eq!(fs::read(dir.join("Tool (2).dmg")).unwrap(), b"abcdef");
        assert_eq!(fs::read(dir.join("Tool.dmg")).unwrap(), b"older");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removes_what_never_finished() {
        let dir = folder("unfinished");
        let mut incoming = Incoming::new(dir.clone());
        start(&mut incoming, 1, "short.pkg", 10);
        start(&mut incoming, 2, "long.pkg", 1);
        incoming
            .data(FileData {
                file: 1,
                data: b"abc".to_vec(),
            })
            .unwrap();
        assert!(incoming.end(1).is_err());
        assert!(!dir.join("short.pkg").exists());
        assert!(incoming
            .data(FileData {
                file: 2,
                data: b"too much".to_vec(),
            })
            .is_err());
        assert!(!dir.join("long.pkg").exists());
        start(&mut incoming, 3, "gone.dmg", 5);
        drop(incoming);
        assert!(!dir.join("gone.dmg").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_stay_in_the_folder() {
        assert_eq!(file_name("a/b/.hidden.dmg").unwrap(), "hidden.dmg");
        assert!(file_name("..").is_err());
        assert!(file_name("/").is_err());
        assert_eq!(numbered("archive", 3), "archive (3)");
        assert_eq!(numbered(".dmg", 2), ".dmg (2)");
    }
}
