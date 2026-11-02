//! History and bookmarks (docs/danube.md, "History and bookmarks"): every
//! page a tab finished loading, newest first, and the pages the person
//! bookmarked, in the order they were added. Both are JSON files in
//! Danube's data directory, written whole (to a new file renamed over the
//! old) whenever they change, and read once at start. The window shows
//! them as derisk's History and Bookmarks menus, which the command palette
//! searches like every other menu, so there is no page of Danube's own to
//! list them.
//!
//! A browser without a profile (the captive portal's, the desktop
//! background's) keeps neither.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tracing::warn;

/// How many pages the history keeps; the oldest go first.
const HISTORY: usize = 1000;

/// A page in the history or the bookmarks.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub uri: String,
    pub title: String,
    /// Seconds since 1970 when it was last visited, or bookmarked.
    pub time: u64,
}

impl Page {
    fn to_json(&self) -> Value {
        json!({"uri": self.uri, "title": self.title, "time": self.time})
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            uri: value["uri"].as_str()?.to_owned(),
            title: value["title"].as_str().unwrap_or_default().to_owned(),
            time: value["time"].as_u64().unwrap_or_default(),
        })
    }

    /// What a menu shows for it: its title, or its address without one.
    pub fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.uri
        } else {
            &self.title
        }
    }
}

pub struct Library {
    /// Where the files live; `None` keeps nothing.
    dir: Option<PathBuf>,
    /// Newest first, one entry per address.
    pub history: Vec<Page>,
    /// Oldest first, as they were added.
    pub bookmarks: Vec<Page>,
}

impl Library {
    pub fn open(dir: Option<&Path>) -> Self {
        let read = |name: &str| -> Vec<Page> {
            let Some(dir) = dir else { return Vec::new() };
            match std::fs::read(dir.join(name)) {
                Ok(bytes) => match serde_json::from_slice::<Vec<Value>>(&bytes) {
                    Ok(list) => list.iter().filter_map(Page::from_json).collect(),
                    Err(error) => {
                        warn!(%error, name, "unreadable; starting it again");
                        Vec::new()
                    }
                },
                Err(_) => Vec::new(),
            }
        };
        Self {
            history: read("history.json"),
            bookmarks: read("bookmarks.json"),
            dir: dir.map(Path::to_path_buf),
        }
    }

    /// A tab finished loading `uri`. Only pages from the web and files
    /// count: not a blank tab, an extension's page or Danube's own.
    /// `true` when the history changed.
    pub fn visit(&mut self, uri: &str, title: &str) -> bool {
        if self.dir.is_none() || !kept(uri) {
            return false;
        }
        self.history.retain(|p| p.uri != uri);
        self.history.insert(
            0,
            Page {
                uri: uri.to_owned(),
                title: title.to_owned(),
                time: now(),
            },
        );
        self.history.truncate(HISTORY);
        self.save("history.json", &self.history);
        true
    }

    /// The page at `uri` has a title now (titles often come after the
    /// load). `true` when that changed anything.
    pub fn retitle(&mut self, uri: &str, title: &str) -> bool {
        let mut changed = false;
        for page in self.history.iter_mut().chain(self.bookmarks.iter_mut()) {
            if page.uri == uri && page.title != title && !title.is_empty() {
                page.title = title.to_owned();
                changed = true;
            }
        }
        if changed {
            self.save("history.json", &self.history);
            self.save("bookmarks.json", &self.bookmarks);
        }
        changed
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.save("history.json", &self.history);
    }

    pub fn is_bookmarked(&self, uri: &str) -> bool {
        self.bookmarks.iter().any(|p| p.uri == uri)
    }

    /// Bookmarks `uri`, or takes its bookmark away when it has one.
    pub fn toggle_bookmark(&mut self, uri: &str, title: &str) -> bool {
        if self.dir.is_none() || !kept(uri) {
            return false;
        }
        if self.is_bookmarked(uri) {
            self.bookmarks.retain(|p| p.uri != uri);
        } else {
            self.bookmarks.push(Page {
                uri: uri.to_owned(),
                title: title.to_owned(),
                time: now(),
            });
        }
        self.save("bookmarks.json", &self.bookmarks);
        true
    }

    fn save(&self, name: &str, pages: &[Page]) {
        let Some(dir) = &self.dir else { return };
        let path = dir.join(name);
        let new = dir.join(format!(".{name}.new"));
        let list: Vec<Value> = pages.iter().map(Page::to_json).collect();
        let written = std::fs::write(&new, Value::Array(list).to_string())
            .and_then(|()| std::fs::rename(&new, &path));
        if let Err(error) = written {
            warn!(%error, path = %path.display(), "cannot save");
        }
    }
}

/// Whether a load of `uri` goes in the history and may be bookmarked.
fn kept(uri: &str) -> bool {
    ["https://", "http://", "file://"].iter().any(|s| uri.starts_with(s))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library of its own for each test, which run at once.
    fn library(test: &str) -> (Library, PathBuf) {
        let dir = std::env::temp_dir().join(format!("danube-library-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (Library::open(Some(&dir)), dir)
    }

    #[test]
    fn history_is_newest_first_once_each() {
        let (mut lib, dir) = library("history");
        assert!(lib.visit("https://a.example/", "A"));
        assert!(lib.visit("https://b.example/", "B"));
        assert!(lib.visit("https://a.example/", "A again"));
        assert!(!lib.visit("about:blank", ""));
        assert!(!lib.visit("danube-extension://x/popup.html", ""));
        let uris: Vec<_> = lib.history.iter().map(|p| p.uri.as_str()).collect();
        assert_eq!(uris, ["https://a.example/", "https://b.example/"]);
        assert_eq!(lib.history[0].title, "A again");
        // Read back as written.
        let again = Library::open(Some(&dir));
        assert_eq!(again.history, lib.history);
        lib.clear_history();
        assert!(Library::open(Some(&dir)).history.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bookmarks_toggle_and_take_late_titles() {
        let (mut lib, dir) = library("bookmarks");
        assert!(lib.toggle_bookmark("https://a.example/", ""));
        assert!(lib.is_bookmarked("https://a.example/"));
        assert_eq!(lib.bookmarks[0].label(), "https://a.example/");
        assert!(lib.retitle("https://a.example/", "A"));
        assert_eq!(Library::open(Some(&dir)).bookmarks[0].label(), "A");
        assert!(lib.toggle_bookmark("https://a.example/", "A"));
        assert!(!lib.is_bookmarked("https://a.example/"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_profile_keeps_nothing() {
        let mut lib = Library::open(None);
        assert!(!lib.visit("https://a.example/", "A"));
        assert!(!lib.toggle_bookmark("https://a.example/", "A"));
        assert!(lib.history.is_empty() && lib.bookmarks.is_empty());
    }
}
