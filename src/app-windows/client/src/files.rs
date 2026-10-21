//! Files for the guest. `send` hands some over to be opened there, and
//! `watch` hands over each of the guest's packages that lands in a folder
//! here, such as the person's Downloads, so that a disk image a browser
//! fetched on this desktop opens in macOS as it would on a Mac.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use app_windows_protocol::{self as protocol, FileData, FileRef, FileStart, Message};

use crate::link::{self, Link, Target};

/// What `watch` hands over when it is not told: macOS's disk images and
/// installer packages.
pub const MAC_PACKAGES: &[&str] = &["dmg", "pkg", "mpkg"];

/// How often `watch` looks at its folder. A download counts once its size
/// and time have stayed the same from one look to the next.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// Pieces waiting to be written before `send` reads more of the file.
const PIECES_AHEAD: usize = 8;

/// Hands files to a guest connected to, each opened there once whole when
/// `open` says so, and returns where the guest put them.
pub fn send(
    link: &Link,
    reader: &mut link::Reader,
    paths: &[PathBuf],
    open: bool,
) -> Result<Vec<String>, String> {
    let mut saved = Vec::new();
    for (n, path) in paths.iter().enumerate() {
        let file = n as u32 + 1;
        send_one(link, file, path, open)?;
        loop {
            match protocol::read(reader).map_err(|e| format!("the guest hung up: {e}"))? {
                Message::FileSaved(s) if s.file == file => {
                    saved.push(s.path);
                    break;
                }
                Message::Error(e) => return Err(format!("{}: {}", path.display(), e.message)),
                _ => {}
            }
        }
    }
    Ok(saved)
}

pub fn send_one(link: &Link, file: u32, path: &Path, open: bool) -> Result<(), String> {
    let mut input = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = input
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{} is not a file", path.display()))?;
    link.send(Message::FileStart(FileStart {
        file,
        name,
        size,
        open,
    }));
    let mut sent = 0;
    while sent < size {
        while link.backlog() > PIECES_AHEAD {
            thread::sleep(Duration::from_millis(5));
        }
        let mut piece = vec![0; protocol::FILE_PIECE.min((size - sent) as usize)];
        input
            .read_exact(&mut piece)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        sent += piece.len() as u64;
        link.send(Message::FileData(FileData { file, data: piece }));
    }
    link.send(Message::FileEnd(FileRef { file }));
    Ok(())
}

/// Hands over every file with one of `extensions` that appears in `folder`
/// from now on, until killed. A guest that cannot be reached is tried
/// again at the next look, so a download made while it is off opens once
/// it is up.
pub fn watch(target: &Target, folder: &Path, extensions: &[String]) -> Result<(), String> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let mut downloads = Downloads::new(look(folder, extensions));
    eprintln!(
        "app-windows: handing {} from {} to the guest",
        extensions.join(", "),
        folder.display()
    );
    loop {
        thread::sleep(LOOK_EVERY);
        let ready = downloads.settled(look(folder, extensions));
        if ready.is_empty() {
            continue;
        }
        let sent = link::connect(target)
            .and_then(|(_, link, mut reader)| send(&link, &mut reader, &ready, true));
        match sent {
            Ok(saved) => {
                for (path, there) in ready.iter().zip(&saved) {
                    eprintln!("app-windows: {} opened as {there}", path.display());
                }
                downloads.sent(&ready);
            }
            Err(e) => eprintln!("app-windows: {e}; trying again"),
        }
    }
}

/// A file's size and time of change, which a download in progress keeps
/// changing.
type Stamp = (u64, SystemTime);

fn look(folder: &Path, extensions: &[String]) -> HashMap<PathBuf, Stamp> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            // Hidden files are browsers' and Files' own business.
            !name.starts_with('.')
                && name.rsplit_once('.').is_some_and(|(_, extension)| {
                    extensions.iter().any(|x| x.eq_ignore_ascii_case(extension))
                })
        })
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            Some((e.path(), (meta.len(), meta.modified().ok()?)))
        })
        .collect()
}

/// What `watch` has seen of its folder.
struct Downloads {
    /// Each file as the last look found it.
    last: HashMap<PathBuf, Stamp>,
    /// Files already there or already handed over, as they were then.
    done: HashMap<PathBuf, Stamp>,
}

impl Downloads {
    /// Files there before watching began are the person's, not new
    /// downloads, and stay where they are.
    fn new(now: HashMap<PathBuf, Stamp>) -> Self {
        Downloads {
            last: now.clone(),
            done: now,
        }
    }

    /// The new or changed files that look the same as at the last look.
    fn settled(&mut self, now: HashMap<PathBuf, Stamp>) -> Vec<PathBuf> {
        let mut ready: Vec<PathBuf> = now
            .iter()
            .filter(|(path, stamp)| {
                self.last.get(*path) == Some(stamp) && self.done.get(*path) != Some(stamp)
            })
            .map(|(path, _)| path.clone())
            .collect();
        ready.sort();
        // A file removed and downloaded again under its name is new.
        self.done.retain(|path, _| now.contains_key(path));
        self.last = now;
        ready
    }

    fn sent(&mut self, paths: &[PathBuf]) {
        for path in paths {
            if let Some(stamp) = self.last.get(path) {
                self.done.insert(path.clone(), *stamp);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(size: u64, secs: u64) -> Stamp {
        (size, SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    fn files(list: &[(&str, Stamp)]) -> HashMap<PathBuf, Stamp> {
        list.iter().map(|(p, s)| (PathBuf::from(p), *s)).collect()
    }

    #[test]
    fn hands_over_new_downloads_once_settled() {
        let mut d = Downloads::new(files(&[("old.dmg", at(5, 1))]));
        // Still growing.
        assert!(d
            .settled(files(&[("old.dmg", at(5, 1)), ("new.dmg", at(1, 2))]))
            .is_empty());
        assert!(d
            .settled(files(&[("old.dmg", at(5, 1)), ("new.dmg", at(9, 3))]))
            .is_empty());
        let ready = d.settled(files(&[("old.dmg", at(5, 1)), ("new.dmg", at(9, 3))]));
        assert_eq!(ready, [PathBuf::from("new.dmg")]);
        d.sent(&ready);
        assert!(d
            .settled(files(&[("old.dmg", at(5, 1)), ("new.dmg", at(9, 3))]))
            .is_empty());
        // Downloaded again under the same name.
        d.settled(files(&[("old.dmg", at(5, 1))]));
        d.settled(files(&[("old.dmg", at(5, 1)), ("new.dmg", at(9, 8))]));
        let again = d.settled(files(&[("old.dmg", at(5, 1)), ("new.dmg", at(9, 8))]));
        assert_eq!(again, [PathBuf::from("new.dmg")]);
    }

    #[test]
    fn a_guest_that_cannot_be_reached_is_tried_again() {
        let mut d = Downloads::new(HashMap::new());
        d.settled(files(&[("a.pkg", at(3, 1))]));
        assert_eq!(d.settled(files(&[("a.pkg", at(3, 1))])).len(), 1);
        // Not sent: the next look offers it again.
        assert_eq!(d.settled(files(&[("a.pkg", at(3, 1))])).len(), 1);
    }

    #[test]
    fn looks_only_at_packages() {
        let dir = std::env::temp_dir().join(format!("app-windows-look-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "Tool.DMG",
            "Setup.pkg",
            "notes.txt",
            ".hidden.dmg",
            "Tool.dmg.part",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let extensions: Vec<String> = MAC_PACKAGES.iter().map(|x| x.to_string()).collect();
        let mut found: Vec<String> = look(&dir, &extensions)
            .into_keys()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["Setup.pkg", "Tool.DMG"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
