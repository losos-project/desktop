//! What a person's backups are set to, and where it is kept.
//!
//! All of it is in their home, which systemd-homed keeps encrypted while
//! they are signed out: `$XDG_CONFIG_HOME/losos-backup` holds the settings,
//! the box's pinned certificate and the two secrets the runs sign in with,
//! and `$XDG_STATE_HOME/losos-backup` holds what was uploaded, the names
//! repositories got and the last run's report. Neither directory is backed
//! up (walk.rs).
//!
//! The secrets are a LosOS cloud app password and a LosOS Git access token,
//! each made for this computer when the person signs in on the Backups page
//! and revocable on the box on its own. They are in a file readable by the
//! person alone, not in the Secret Service like `losos`'s keys: the timer
//! runs with nobody at the screen, and a locked keyring would ask for its
//! password in a window then.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// One of the box's two accounts, as signed in to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The name signed in with, which every request authenticates as.
    pub login: String,
    /// The account's own name, which its paths use: in Nextcloud the user
    /// ID, which can differ from the login (an e-mail address, say), in
    /// Forgejo the user name with its case.
    pub user: String,
}

/// How many hours apart backups run unless the person picks otherwise.
pub const EVERY_HOURS: u32 = 24;

/// The longest a person can set between backups: a week.
pub const MAX_HOURS: u32 = 7 * 24;

/// The settings, `config.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The box, by the name it announces (`mattbox.local`); empty until one
    /// is chosen.
    #[serde(rename = "box")]
    pub box_name: String,
    /// The box certificate's SHA-256 fingerprint, as the box's wizard shows
    /// it, kept to show beside the box.
    pub fingerprint: String,
    /// Whether the timer backs up. Off until the person turns it on.
    pub enabled: bool,
    /// The LosOS cloud account files go to.
    pub cloud: Option<Account>,
    /// The LosOS Git account repositories go to.
    pub git: Option<Account>,
    /// Hours from one backup that went through to the next, from 1 to
    /// [`MAX_HOURS`]. The timer asks every hour, so an hour is the finest.
    pub every_hours: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            box_name: String::new(),
            fingerprint: String::new(),
            enabled: false,
            cloud: None,
            git: None,
            every_hours: EVERY_HOURS,
        }
    }
}

impl Config {
    /// Whether a run has somewhere to go: a box, and at least one of its
    /// accounts signed in to.
    pub fn ready(&self) -> bool {
        !self.box_name.is_empty() && (self.cloud.is_some() || self.git.is_some())
    }
}

/// The secrets, `secrets.json`, readable by the person alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Secrets {
    /// The LosOS cloud app password.
    pub cloud: Option<String>,
    /// The LosOS Git access token.
    pub git: Option<String>,
}

/// Where everything is.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The home being backed up.
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME/losos-backup`.
    pub config: PathBuf,
    /// `$XDG_STATE_HOME/losos-backup`.
    pub state: PathBuf,
}

impl Paths {
    /// The paths from `$HOME` and the XDG variables, or `None` without a
    /// home.
    pub fn from_env() -> Option<Self> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let xdg = |var: &str, default: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
                .join("losos-backup")
        };
        Some(Self {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            state: xdg("XDG_STATE_HOME", ".local/state"),
            home,
        })
    }

    /// The settings.
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.json")
    }

    /// The secrets.
    pub fn secrets_file(&self) -> PathBuf {
        self.config.join("secrets.json")
    }

    /// The box's certificate, PEM, as fetched when the box was chosen. git
    /// reads it as its CA file too.
    pub fn cert_file(&self) -> PathBuf {
        self.config.join("box.pem")
    }

    /// More paths to leave out, one per line, relative to the home.
    pub fn exclude_file(&self) -> PathBuf {
        self.config.join("exclude")
    }

    /// What was uploaded to LosOS cloud.
    pub fn index_file(&self) -> PathBuf {
        self.state.join("files.json")
    }

    /// The names repositories got in LosOS Git.
    pub fn repos_file(&self) -> PathBuf {
        self.state.join("repos.json")
    }

    /// The last run's report.
    pub fn report_file(&self) -> PathBuf {
        self.state.join("last.json")
    }

    /// Held while a run is going.
    pub fn lock_file(&self) -> PathBuf {
        self.state.join("lock")
    }

    /// Reads the settings, or the defaults when there are none yet.
    pub fn load_config(&self) -> io::Result<Config> {
        load(&self.config_file())
    }

    /// Writes the settings.
    pub fn save_config(&self, config: &Config) -> io::Result<()> {
        save(&self.config_file(), config, false)
    }

    /// Reads the secrets.
    pub fn load_secrets(&self) -> io::Result<Secrets> {
        load(&self.secrets_file())
    }

    /// Writes the secrets, readable by the person alone.
    pub fn save_secrets(&self, secrets: &Secrets) -> io::Result<()> {
        save(&self.secrets_file(), secrets, true)
    }
}

/// Reads JSON from `path`, or `T::default()` when there is no such file.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e),
    }
}

/// Writes `value` to `path` as JSON, whole or not at all: through a file
/// beside it, renamed over it. `private` makes both readable by the owner
/// alone, from the moment the file exists.
pub fn save<T: Serialize>(path: &Path, value: &T, private: bool) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let partial = dir.join(format!(
        ".{}.partial",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(if private { 0o600 } else { 0o644 })
        .open(&partial)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
    file.sync_all()?;
    fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn keeps_the_secrets_to_their_owner() {
        let dir = std::env::temp_dir().join(format!("losos-backup-test-{}", std::process::id()));
        let paths = Paths {
            home: dir.clone(),
            config: dir.join("config"),
            state: dir.join("state"),
        };
        assert_eq!(paths.load_config().unwrap(), Config::default());
        let secrets = Secrets {
            cloud: Some("app-password".into()),
            git: None,
        };
        paths.save_secrets(&secrets).unwrap();
        assert_eq!(paths.load_secrets().unwrap(), secrets);
        let mode = fs::metadata(paths.secrets_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn is_ready_with_a_box_and_an_account() {
        let mut config = Config::default();
        assert!(!config.ready());
        config.box_name = "mattbox.local".into();
        assert!(!config.ready());
        config.git = Some(Account::default());
        assert!(config.ready());
    }
}
