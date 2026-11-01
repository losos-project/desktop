//! LosOS Git, the box's Git server at `/git/`: a private repository per
//! repository in the home, and the push. Its API has Forgejo's shapes for
//! what this uses, so a box still on Forgejo answered the same.
//!
//! Each repository's branches and tags are pushed as they are, forced, and
//! a branch deleted at home is deleted on the box, so the box's copy is a
//! mirror. What is not committed yet goes too, as one commit on the branch
//! `losos-backup/uncommitted`: the work tree, untracked files included but
//! not ignored ones, on top of `HEAD`. It is made with an index of its own,
//! so the repository's index, branches and stash stay as they were.
//!
//! The configs (`~/.config` and the home's dotfiles, [`crate::walk::is_config`])
//! go to a repository of their own, `<computer>-config`, as a history: a
//! commit each run that finds them changed, on `main`, which the run
//! continues from the box's copy so that the history survives this
//! computer losing its own.
//!
//! git does the pushing, with everything it needs in its environment
//! rather than its command line, which other accounts can read: the token,
//! the box's certificate as the only CA, and the box's name pinned to the
//! address the run goes through. The repository's own hooks do not run.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{json, Value};

use crate::{boxes::Route, cloud::basic, encode_segment, walk::Repo, Error};

/// The branch uncommitted work is pushed to.
pub const UNCOMMITTED: &str = "losos-backup/uncommitted";

/// LosOS Git's address on `box_name`.
pub fn base(box_name: &str) -> String {
    format!("https://{box_name}/git")
}

// There was a sign-in with a name and password here, which asked Forgejo
// for a token. LosOS Git, which replaced Forgejo on the box, takes no
// passwords: the token comes from LosOS Login's sign-in (`crate::login`),
// or is made in LosOS Git's own settings and checked by `check_token`.

/// The account `token` belongs to, which also says LosOS Git took it.
// ureq's error carries the whole response, so it is large wherever it goes.
#[allow(clippy::result_large_err)]
pub fn check_token(agent: &ureq::Agent, base: &str, token: &str) -> Result<String, Error> {
    let user: Value = agent
        .get(&format!("{base}/api/v1/user"))
        .set("Authorization", &format!("token {token}"))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(401 | 403, _) => Error::new("LosOS Git did not accept that token."),
            e => Error::from(e),
        })?
        .into_json()?;
    user.get("login")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::new("LosOS Git did not say whose token it is."))
}

/// A Forgejo repository name for `rel`, not among `taken`: the
/// repository's directory name, or its whole path when that name is taken,
/// in the characters Forgejo allows.
pub fn repo_name(rel: &str, taken: &BTreeSet<String>) -> String {
    let clean = |s: &str| {
        let s: String = s
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let s = s.trim_matches(|c| c == '.' || c == '-').to_owned();
        let s = s.strip_suffix(".git").unwrap_or(&s).to_owned();
        let s: String = s.chars().take(90).collect();
        if s.is_empty() {
            "home".to_owned()
        } else {
            s
        }
    };
    let last = rel.rsplit('/').next().unwrap_or(rel);
    let mut candidates = vec![clean(last), clean(&rel.replace('/', "-"))];
    candidates.dedup();
    for candidate in &candidates {
        if !taken.contains(candidate) {
            return candidate.clone();
        }
    }
    let base = candidates.last().cloned().unwrap_or_default();
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !taken.contains(c))
        .unwrap_or(base)
}

/// The names repositories got, kept so a repository keeps its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Names {
    /// The box and account they are for; another starts the list over.
    pub owner: String,
    /// Each repository's path relative to the home, and its name.
    pub names: BTreeMap<String, String>,
    /// The configs' repository's name, or empty until it has one.
    pub config: String,
}

impl Names {
    fn taken(&self) -> BTreeSet<String> {
        let mut taken: BTreeSet<String> = self.names.values().cloned().collect();
        if !self.config.is_empty() {
            taken.insert(self.config.clone());
        }
        taken
    }

    /// The name for `rel`, given one if it has none yet.
    pub fn name(&mut self, rel: &str) -> String {
        if let Some(name) = self.names.get(rel) {
            return name.clone();
        }
        let name = repo_name(rel, &self.taken());
        self.names.insert(rel.to_owned(), name.clone());
        name
    }

    /// The configs' repository's name, `<host>-config`, given it if it has
    /// none yet.
    pub fn config(&mut self, host: &str) -> String {
        if self.config.is_empty() {
            self.config = repo_name(&format!("{host}-config"), &self.taken());
        }
        self.config.clone()
    }
}

/// A person's account in LosOS Git.
pub struct Forge<'a> {
    agent: &'a ureq::Agent,
    route: &'a Route,
    base: String,
    user: String,
    token: String,
    /// The box's certificate, for git.
    cert_file: &'a Path,
}

impl<'a> Forge<'a> {
    /// `user`'s account, with `token`, reached through `route`.
    pub fn new(
        agent: &'a ureq::Agent,
        route: &'a Route,
        user: &str,
        token: &str,
        cert_file: &'a Path,
    ) -> Self {
        Self {
            agent,
            base: base(&route.name),
            route,
            user: user.to_owned(),
            token: token.to_owned(),
            cert_file,
        }
    }

    /// Makes the private repository `name` unless it is there.
    pub fn ensure(&self, name: &str, description: &str) -> Result<(), Error> {
        let auth = format!("token {}", self.token);
        let repo = format!(
            "{}/api/v1/repos/{}/{}",
            self.base,
            encode_segment(&self.user),
            encode_segment(name)
        );
        match self.agent.get(&repo).set("Authorization", &auth).call() {
            Ok(_) => return Ok(()),
            Err(ureq::Error::Status(404, _)) => {}
            Err(e) => return Err(e.into()),
        }
        self.agent
            .post(&format!("{}/api/v1/user/repos", self.base))
            .set("Authorization", &auth)
            .send_json(json!({
                "name": name,
                "private": true,
                "description": description,
            }))?;
        Ok(())
    }

    /// The repository's clone URL.
    pub fn url(&self, name: &str) -> String {
        format!(
            "{}/{}/{}.git",
            self.base,
            encode_segment(&self.user),
            encode_segment(name)
        )
    }

    /// The environment git pushes with.
    fn env(&self) -> Vec<(String, String)> {
        let address = match self.route.address {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        let config = [
            (
                "http.extraHeader",
                format!("Authorization: {}", basic(&self.user, &self.token)),
            ),
            ("http.sslCAInfo", self.cert_file.display().to_string()),
            (
                "http.curloptResolve",
                format!("{}:443:{address}", self.route.name),
            ),
            ("credential.helper", String::new()),
            ("core.hooksPath", "/dev/null".to_owned()),
        ];
        let mut env = vec![
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
            ("GIT_CONFIG_COUNT".to_owned(), config.len().to_string()),
        ];
        for (i, (key, value)) in config.into_iter().enumerate() {
            env.push((format!("GIT_CONFIG_KEY_{i}"), key.to_owned()));
            env.push((format!("GIT_CONFIG_VALUE_{i}"), value));
        }
        env
    }

    /// Pushes `repo` to `name`, made first if need be. Returns whether
    /// there was anything to push: a repository with no commit has not.
    pub fn push(&self, repo: &Repo, name: &str, host: &str, scratch: &Path) -> Result<bool, Error> {
        let local: BTreeSet<String> = git(
            &repo.path,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads",
                "refs/tags",
            ],
            &[],
        )?
        .lines()
        .map(str::to_owned)
        .collect();
        if local.is_empty() {
            return Ok(false);
        }
        self.ensure(
            name,
            &format!("~/{} on {host}, backed up by LosOS Desktop", repo.rel),
        )?;
        let url = self.url(name);
        let env = self.env();
        let remote: BTreeSet<String> = git(&repo.path, &["ls-remote", "--heads", &url], &env)?
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1))
            .map(str::to_owned)
            .collect();
        let mut refspecs = vec![
            "+refs/heads/*:refs/heads/*".to_owned(),
            "+refs/tags/*:refs/tags/*".to_owned(),
        ];
        let uncommitted = format!("refs/heads/{UNCOMMITTED}");
        match (!repo.bare)
            .then(|| snapshot(repo, host, scratch))
            .transpose()?
            .flatten()
        {
            Some(commit) => refspecs.push(format!("+{commit}:{uncommitted}")),
            None if remote.contains(&uncommitted) => refspecs.push(format!(":{uncommitted}")),
            None => {}
        }
        for gone in remote
            .iter()
            .filter(|r| !local.contains(*r) && **r != uncommitted)
        {
            refspecs.push(format!(":{gone}"));
        }
        let mut args = vec!["push", "--porcelain", "--quiet", url.as_str()];
        args.extend(refspecs.iter().map(String::as_str));
        git(&repo.path, &args, &env)?;
        Ok(true)
    }
}

impl Forge<'_> {
    /// Commits `files`, paths relative to `home`, as `main` of the
    /// repository `name`, made first if need be, on top of the box's `main`,
    /// and pushes it. `git_dir` is this computer's copy, a bare repository
    /// with an index of its own. Returns whether it pushed a commit: not
    /// when nothing changed since the box's.
    pub fn push_files(
        &self,
        home: &Path,
        files: &[&str],
        name: &str,
        host: &str,
        git_dir: &Path,
    ) -> Result<bool, Error> {
        if files.is_empty() {
            return Ok(false);
        }
        if !git_dir.join("HEAD").is_file() {
            fs::create_dir_all(git_dir)?;
            git(
                git_dir,
                &["init", "--bare", "--quiet", "--initial-branch=main"],
                &[],
            )?;
        }
        self.ensure(
            name,
            &format!("The configs on {host}, backed up by LosOS Desktop"),
        )?;
        let url = self.url(name);
        let mut env = self.env();
        env.push(("GIT_DIR".to_owned(), git_dir.display().to_string()));
        // The box's main, fetched every run: it is where the history goes
        // on from, so this computer's copy can be lost, and an edit made on
        // the box is a parent rather than a push refused.
        let theirs = "refs/remotes/box/main";
        let parent = if git(home, &["ls-remote", "--heads", &url, "main"], &env)?
            .trim()
            .is_empty()
        {
            None
        } else {
            git(
                home,
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    &url,
                    &format!("+main:{theirs}"),
                ],
                &env,
            )?;
            Some(git(home, &["rev-parse", theirs], &env)?.trim().to_owned())
        };
        // A new index each run holding exactly these files, read from the
        // home: update-index takes the paths as they are, so nothing is
        // left out by an ignore file or read as a pattern.
        let index = git_dir.join("backup-index");
        let _ = fs::remove_file(&index);
        let mut tree_env = env.clone();
        tree_env.push(("GIT_INDEX_FILE".to_owned(), index.display().to_string()));
        tree_env.push(("GIT_WORK_TREE".to_owned(), home.display().to_string()));
        let list = files.iter().fold(Vec::new(), |mut list, f| {
            list.extend_from_slice(f.as_bytes());
            list.push(0);
            list
        });
        git_with(
            home,
            &["update-index", "--add", "-z", "--stdin"],
            &tree_env,
            Some(&list),
        )?;
        let tree = git(home, &["write-tree"], &tree_env)?.trim().to_owned();
        let _ = fs::remove_file(&index);
        if let Some(parent) = &parent {
            let theirs = git(home, &["rev-parse", &format!("{parent}^{{tree}}")], &env)?;
            if theirs.trim() == tree {
                return Ok(false);
            }
        }
        let mut commit_env = env.clone();
        commit_env.extend(ident(host));
        let message = format!("The configs on {host}");
        let mut args = vec!["commit-tree", tree.as_str(), "-m", &message];
        if let Some(parent) = &parent {
            args.extend(["-p", parent.as_str()]);
        }
        let commit = git(home, &args, &commit_env)?.trim().to_owned();
        // Not forced: the commit is on top of the box's main, so anything
        // else there since is a refusal to report, not history to drop.
        git(
            home,
            &[
                "push",
                "--porcelain",
                "--quiet",
                &url,
                &format!("{commit}:refs/heads/main"),
            ],
            &env,
        )?;
        Ok(true)
    }
}

/// Who the backups' own commits are by.
fn ident(host: &str) -> [(String, String); 4] {
    [
        ("GIT_AUTHOR_NAME".to_owned(), "LosOS backup".to_owned()),
        ("GIT_AUTHOR_EMAIL".to_owned(), format!("backup@{host}")),
        ("GIT_COMMITTER_NAME".to_owned(), "LosOS backup".to_owned()),
        ("GIT_COMMITTER_EMAIL".to_owned(), format!("backup@{host}")),
    ]
}

/// A commit of the work tree as it is, on top of `HEAD`, when it differs
/// from `HEAD`; made with a copy of the index in `scratch`, so nothing of
/// the repository's own changes.
fn snapshot(repo: &Repo, host: &str, scratch: &Path) -> Result<Option<String>, Error> {
    let status = git(
        &repo.path,
        &["status", "--porcelain", "--untracked-files=normal"],
        &[],
    )?;
    if status.trim().is_empty()
        || git(
            &repo.path,
            &["rev-parse", "--verify", "--quiet", "HEAD"],
            &[],
        )
        .is_err()
    {
        return Ok(None);
    }
    fs::create_dir_all(scratch)?;
    let index = scratch.join("index");
    let own = git(&repo.path, &["rev-parse", "--git-path", "index"], &[])?;
    let own = repo.path.join(own.trim());
    if own.exists() {
        fs::copy(&own, &index)?;
    } else {
        let _ = fs::remove_file(&index);
    }
    let index_env = [("GIT_INDEX_FILE".to_owned(), index.display().to_string())];
    let result = (|| {
        git(&repo.path, &["add", "--all"], &index_env)?;
        let tree = git(&repo.path, &["write-tree"], &index_env)?;
        let message = format!("Uncommitted changes in ~/{} on {host}", repo.rel);
        git(
            &repo.path,
            &["commit-tree", tree.trim(), "-p", "HEAD", "-m", &message],
            &ident(host),
        )
    })();
    let _ = fs::remove_file(&index);
    Ok(Some(result?.trim().to_owned()))
}

/// Runs git in `dir` with `env` added, and returns what it printed.
fn git(dir: &Path, args: &[&str], env: &[(String, String)]) -> Result<String, Error> {
    git_with(dir, args, env, None)
}

/// [`git`], with `input` on its standard input.
fn git_with(
    dir: &Path,
    args: &[&str],
    env: &[(String, String)],
    input: Option<&[u8]>,
) -> Result<String, Error> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        // The box's certificate is the only CA, which these would replace.
        .env_remove("GIT_SSL_CAINFO")
        .env_remove("GIT_SSL_CAPATH")
        .env_remove("GIT_SSL_NO_VERIFY")
        // The box is reached at the address the run goes through, which
        // a proxy could not reach.
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("all_proxy")
        .env_remove("ALL_PROXY")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(format!("cannot run git: {e}")))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // Written whole before reading: update-index reads all of its
        // input before it prints anything.
        stdin.write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("no reason given");
        Err(Error::new(format!("git {} failed: {reason}", args[0])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_repositories_after_their_directory() {
        let mut names = Names::default();
        assert_eq!(names.name("src/losos-desktop"), "losos-desktop");
        assert_eq!(names.name("work/losos-desktop"), "work-losos-desktop");
        assert_eq!(names.name("src/losos-desktop"), "losos-desktop");
        assert_eq!(names.name("backups/dotfiles.git"), "dotfiles");
        assert_eq!(names.name("Projekty/čaj a káva"), "aj-a-k-va");
        assert_eq!(names.name(".config/nvim"), "nvim");
        assert_eq!(names.name(".local/nvim"), "local-nvim");
        assert_eq!(names.name("x/.local/nvim"), "x-.local-nvim");
    }

    #[test]
    fn takes_a_number_when_both_names_are_taken() {
        let taken: BTreeSet<String> = ["a".into(), "b-a".into()].into();
        assert_eq!(repo_name("b/a", &taken), "b-a-2");
    }

    /// A repository with a commit, a change to it, an untracked file and an
    /// ignored one: the snapshot holds the first two and leaves the
    /// repository's own index alone.
    #[test]
    fn snapshots_uncommitted_work_without_touching_the_index() {
        let Ok(_) = Command::new("git").arg("--version").output() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("losos-backup-git-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ident = [
            ("GIT_AUTHOR_NAME".to_owned(), "t".to_owned()),
            ("GIT_AUTHOR_EMAIL".to_owned(), "t@t".to_owned()),
            ("GIT_COMMITTER_NAME".to_owned(), "t".to_owned()),
            ("GIT_COMMITTER_EMAIL".to_owned(), "t@t".to_owned()),
        ];
        git(&dir, &["init", "-q", "-b", "main"], &[]).unwrap();
        fs::write(dir.join("a.txt"), "one").unwrap();
        fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        git(&dir, &["add", "."], &[]).unwrap();
        git(&dir, &["commit", "-q", "-m", "first"], &ident).unwrap();
        let repo = Repo {
            path: dir.clone(),
            rel: "src/t".into(),
            bare: false,
        };
        let scratch = dir.join("../losos-backup-git-scratch");
        assert_eq!(snapshot(&repo, "pc", &scratch).unwrap(), None);
        fs::write(dir.join("a.txt"), "two").unwrap();
        fs::write(dir.join("new.txt"), "new").unwrap();
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::write(dir.join("build/out"), "ignored").unwrap();
        let before = git(&dir, &["status", "--porcelain"], &[]).unwrap();
        let commit = snapshot(&repo, "pc", &scratch).unwrap().unwrap();
        assert_eq!(git(&dir, &["status", "--porcelain"], &[]).unwrap(), before);
        let files = git(&dir, &["ls-tree", "-r", "--name-only", &commit], &[]).unwrap();
        assert_eq!(
            files.lines().collect::<Vec<_>>(),
            [".gitignore", "a.txt", "new.txt"]
        );
        assert_eq!(
            git(&dir, &["show", &format!("{commit}:a.txt")], &[]).unwrap(),
            "two"
        );
        assert_eq!(
            git(&dir, &["log", "-1", "--format=%s", &commit], &[])
                .unwrap()
                .trim(),
            "Uncommitted changes in ~/src/t on pc"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
