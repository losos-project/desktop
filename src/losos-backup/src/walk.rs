//! What in the home gets backed up, and how.
//!
//! The walk goes through the whole home without following symbolic links,
//! which on this OS mostly point into the Nix store. Nextcloud cannot hold
//! a link, so each is listed with its target ([`Tree::links`]), and the
//! list goes to LosOS cloud as one file a restore can make them again
//! from; a config that is a link goes to LosOS Git as one, which git holds. A directory holding a `.git` directory, or a bare repository,
//! is a repository: it goes to LosOS Git and the walk does not go into it.
//! The configs ([`is_config`]) go to LosOS Git as well, when there is an
//! account there. Every other regular file goes to LosOS cloud, apart from what can be
//! fetched or rebuilt again ([`EXCLUDED`], any `node_modules`, and any
//! directory tagged with a `CACHEDIR.TAG`, which cargo's `target`, ccache
//! and most other caches write), the backups' own settings and secrets,
//! and what the person lists in `~/.config/losos-backup/exclude`.

use std::{
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// Paths under the home that are never backed up, relative to it: caches,
/// the trash, what Flatpak, containers and machines download or build, and
/// systemd-homed's copy of the account record with its password hashes.
pub const EXCLUDED: &[&str] = &[
    ".cache",
    ".config/losos-backup",
    ".local/state/losos-backup",
    ".local/share/Trash",
    ".local/share/flatpak",
    ".local/share/containers",
    ".local/share/libvirt",
    ".local/share/gnome-boxes",
    ".cargo/registry",
    ".cargo/git",
    ".rustup",
    ".npm",
    ".gradle/caches",
    ".m2/repository",
    ".nix-defexpr",
    ".identity",
];

/// The first bytes of a valid `CACHEDIR.TAG` (bford.info/cachedir).
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// A file for LosOS cloud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its path relative to the home, `/`-separated.
    pub rel: String,
    /// Its size in bytes.
    pub size: u64,
    /// When it last changed, in nanoseconds since the epoch.
    pub mtime: i64,
}

/// A symbolic link, kept as the list LosOS cloud gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Its path relative to the home, `/`-separated.
    pub rel: String,
    /// What it points to, as written in it.
    pub target: String,
}

/// A repository for LosOS Git.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    /// Where it is.
    pub path: PathBuf,
    /// Its path relative to the home.
    pub rel: String,
    /// Whether it has no working tree.
    pub bare: bool,
}

/// What a walk found.
#[derive(Debug, Default)]
pub struct Tree {
    /// Files for LosOS cloud, in walk order.
    pub files: Vec<File>,
    /// Directories holding them, relative to the home, parents first.
    pub dirs: Vec<String>,
    /// Repositories for LosOS Git.
    pub repos: Vec<Repo>,
    /// Symbolic links, in walk order.
    pub links: Vec<Link>,
    /// Paths that were left out because they could not be read, or their
    /// names are not UTF-8, which WebDAV paths must be.
    pub unreadable: Vec<String>,
}

/// How to walk.
#[derive(Debug, Default)]
pub struct Options {
    /// More paths to leave out, relative to the home.
    pub excluded: Vec<String>,
    /// Whether repositories are walked into as files instead: when no LosOS
    /// Git account is signed in to, so that they are still backed up.
    pub repos_as_files: bool,
}

/// The extra exclusions in `file`: one path per line, relative to the
/// home, `#` starting a comment.
pub fn read_excluded(file: &Path) -> Vec<String> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .map(|l| l.trim_start_matches("~/").trim_matches('/'))
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `rel` is left out by the list or by its own name.
fn excluded(rel: &str, options: &Options) -> bool {
    let under = |prefix: &str| rel == prefix || rel.starts_with(&format!("{prefix}/"));
    if EXCLUDED.iter().any(|p| under(p)) || options.excluded.iter().any(|p| under(p)) {
        return true;
    }
    let parts: Vec<&str> = rel.split('/').collect();
    // Each Flatpak app's own cache.
    if matches!(parts.as_slice(), [".var", "app", _, "cache", ..]) {
        return true;
    }
    parts.last() == Some(&"node_modules")
}

/// Whether `rel` is a config, which goes to its own repository in LosOS
/// Git when there is an account there: anything under `~/.config`, and
/// the home's own dotfiles (`.bashrc`, `.gitconfig`), files whose names
/// start with a dot directly in the home. A dot directory such as `~/.ssh`
/// or `~/.local` is not one, and stays with the rest.
pub fn is_config(rel: &str) -> bool {
    in_config(rel) || (rel.starts_with('.') && !rel.contains('/'))
}

/// Whether `rel` is `~/.config` or under it.
fn in_config(rel: &str) -> bool {
    rel == ".config" || rel.starts_with(".config/")
}

impl Tree {
    /// Takes the configs ([`is_config`]) out of the files and links for
    /// LosOS cloud, with the folders under `~/.config`, and returns how
    /// many there were.
    pub fn take_configs(&mut self) -> usize {
        let before = self.files.len() + self.links.len();
        self.files.retain(|f| !is_config(&f.rel));
        self.links.retain(|l| !is_config(&l.rel));
        self.dirs.retain(|d| !in_config(d));
        before - self.files.len() - self.links.len()
    }

    /// The links as LosOS cloud keeps them: a line each, the path and its
    /// target split by a tab, after a header saying how to make them again.
    pub fn links_list(&self) -> String {
        let mut list = String::from(
            "# The symbolic links in this home, which LosOS cloud cannot hold:\n\
             # each line is a path, a tab and the link's target. From the home,\n\
             #   grep -v '^#' .losos-backup-links.tsv | while IFS=\"$(printf '\\t')\" read -r p t; do ln -sfn \"$t\" \"$p\"; done\n\
             # makes them again.\n",
        );
        for link in &self.links {
            list.push_str(&link.rel);
            list.push('\t');
            list.push_str(&link.target);
            list.push('\n');
        }
        list
    }
}

/// Whether `dir` is tagged as a cache.
fn is_cache(dir: &Path) -> bool {
    let mut head = [0; CACHEDIR_SIGNATURE.len()];
    fs::File::open(dir.join("CACHEDIR.TAG"))
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| head == CACHEDIR_SIGNATURE)
}

/// Whether `dir` is a bare repository: HEAD, objects and refs, and no work
/// tree.
fn is_bare(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

/// Walks `home`.
pub fn walk(home: &Path, options: &Options) -> Tree {
    let mut tree = Tree::default();
    let mut stack = vec![(home.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                tree.unreadable.push(rel);
                continue;
            }
        };
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for entry in names {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                tree.unreadable
                    .push(format!("{rel}/{}", entry.file_name().to_string_lossy()));
                continue;
            };
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if excluded(&child, options) {
                continue;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                tree.unreadable.push(child);
                continue;
            };
            if meta.is_file() {
                tree.files.push(File {
                    rel: child,
                    size: meta.len(),
                    mtime: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
                });
            } else if meta.is_dir() {
                subdirs.push((entry.path(), child));
            } else if meta.file_type().is_symlink() {
                // A target that is not UTF-8, or would break the list's
                // lines, cannot be listed.
                match fs::read_link(entry.path()).ok().and_then(|t| {
                    t.to_str()
                        .filter(|t| !t.contains(['\t', '\n']) && !child.contains(['\t', '\n']))
                        .map(str::to_owned)
                }) {
                    Some(target) => tree.links.push(Link { rel: child, target }),
                    None => tree.unreadable.push(child),
                }
            }
            // Sockets, FIFOs and devices have nothing to upload.
        }
        // Reversed, so the stack pops them in name order.
        for (path, child) in subdirs.into_iter().rev() {
            if is_cache(&path) {
                continue;
            }
            if !options.repos_as_files {
                let git = path.join(".git");
                if git.is_dir() || is_bare(&path) {
                    tree.repos.push(Repo {
                        bare: !git.is_dir(),
                        path,
                        rel: child,
                    });
                    continue;
                }
                // A `.git` file is a worktree or a submodule's checkout,
                // whose commits are in the repository it points to.
                if git.is_file() {
                    continue;
                }
            }
            tree.dirs.push(child.clone());
            stack.push((path, child));
        }
    }
    tree.dirs.sort();
    tree.repos.sort_by(|a, b| a.rel.cmp(&b.rel));
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        let home = std::env::temp_dir().join(format!("losos-backup-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        for dir in [
            "Documents/Taxes",
            ".cache/fontconfig",
            ".config/losos-backup",
            "src/project/.git/objects",
            "src/project/src",
            "src/worktree",
            "src/bare.git/objects",
            "src/bare.git/refs",
            "src/web/node_modules/left-pad",
            "src/rust/target/debug",
            ".var/app/org.example.App/cache",
            ".var/app/org.example.App/config",
            "Videos",
        ] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        for (file, text) in [
            ("Documents/Taxes/2026.pdf", "%PDF"),
            ("Documents/notes.txt", "hello"),
            (".cache/fontconfig/x", "cache"),
            (".config/losos-backup/secrets.json", "{}"),
            ("src/project/README.md", "repo file"),
            ("src/worktree/.git", "gitdir: ../project/.git/worktrees/w"),
            ("src/bare.git/HEAD", "ref: refs/heads/main"),
            ("src/web/node_modules/left-pad/index.js", ""),
            (
                "src/rust/target/CACHEDIR.TAG",
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            ),
            ("src/rust/target/debug/app", "elf"),
            ("src/rust/Cargo.toml", "[package]"),
            (".var/app/org.example.App/cache/blob", ""),
            (".var/app/org.example.App/config/app.ini", ""),
            ("Videos/big.mkv", "skip me"),
        ] {
            fs::write(home.join(file), text).unwrap();
        }
        std::os::unix::fs::symlink("/nix/store", home.join("store-link")).unwrap();
        home
    }

    #[test]
    fn walks_the_home_as_files_and_repositories() {
        let home = home();
        let options = Options {
            excluded: vec!["Videos".into()],
            repos_as_files: false,
        };
        let tree = walk(&home, &options);
        let files: Vec<&str> = tree.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            files,
            [
                ".var/app/org.example.App/config/app.ini",
                "Documents/notes.txt",
                "Documents/Taxes/2026.pdf",
                "src/rust/Cargo.toml",
            ]
        );
        assert_eq!(
            tree.dirs,
            [
                ".config",
                ".var",
                ".var/app",
                ".var/app/org.example.App",
                ".var/app/org.example.App/config",
                "Documents",
                "Documents/Taxes",
                "src",
                "src/rust",
                "src/web",
            ]
        );
        let repos: Vec<(&str, bool)> = tree
            .repos
            .iter()
            .map(|r| (r.rel.as_str(), r.bare))
            .collect();
        assert_eq!(repos, [("src/bare.git", true), ("src/project", false)]);
        assert_eq!(tree.files[1].size, 5);
        assert_eq!(
            tree.links,
            [Link {
                rel: "store-link".into(),
                target: "/nix/store".into(),
            }]
        );
        assert!(tree.links_list().ends_with("\nstore-link\t/nix/store\n"));

        let tree = walk(
            &home,
            &Options {
                excluded: Vec::new(),
                repos_as_files: true,
            },
        );
        assert!(tree.repos.is_empty());
        assert!(tree.files.iter().any(|f| f.rel == "src/project/README.md"));
        assert!(tree.files.iter().any(|f| f.rel == "src/worktree/.git"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn takes_out_the_configs() {
        let file = |rel: &str| File {
            rel: rel.into(),
            size: 0,
            mtime: 0,
        };
        let mut tree = Tree {
            files: vec![
                file(".bashrc"),
                file(".config/git/config"),
                file(".configure-notes"),
                file(".ssh/config"),
                file("Documents/.hidden"),
                file("Documents/notes.txt"),
            ],
            dirs: vec![
                ".config".into(),
                ".config/git".into(),
                ".ssh".into(),
                "Documents".into(),
            ],
            links: vec![
                Link {
                    rel: ".config/home-manager".into(),
                    target: "/nix/store/x-home-manager-files/.config/home-manager".into(),
                },
                Link {
                    rel: "Downloads".into(),
                    target: "/mnt/data/Downloads".into(),
                },
            ],
            ..Tree::default()
        };
        // Three files and a link.
        assert_eq!(tree.take_configs(), 4);
        assert_eq!(tree.links.len(), 1);
        assert_eq!(tree.links[0].rel, "Downloads");
        let files: Vec<&str> = tree.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            files,
            [".ssh/config", "Documents/.hidden", "Documents/notes.txt"]
        );
        assert_eq!(tree.dirs, [".ssh", "Documents"]);
    }

    #[test]
    fn reads_the_persons_own_exclusions() {
        let file =
            std::env::temp_dir().join(format!("losos-backup-exclude-{}", std::process::id()));
        fs::write(
            &file,
            "# big things\nVideos\n~/Downloads/\n\n  Games # Steam\n",
        )
        .unwrap();
        assert_eq!(read_excluded(&file), ["Videos", "Downloads", "Games"]);
        fs::remove_file(file).unwrap();
    }
}
