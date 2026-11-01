//! Focus mode: while a game, or an editor for 3D, video or music, is open,
//! what the desktop runs in the background waits (docs/focus-mode.md).
//!
//!     losos-focus start [<unit>...] | stop | status   (the person, or GameMode)
//!     losos-focus system pause <unit>...              (root, losos-focus@.service)
//!     losos-focus system hold <user>                  (root, losos-focus@.service)
//!     losos-focus system resume                       (root, losos-focus@.service)
//!
//! GameMode runs `start` when the first program asks it for focus and
//! `stop` when the last one has gone (focus.nix). `start` freezes the
//! person's background.slice, stops the person's units it is given, and
//! starts losos-focus@<user>.service, which does the same for the system:
//! it freezes losos-background.slice and stops its units, Ollama's among
//! them, whose memory a game wants. `stop` and `resume` undo it.
//!
//! The units stopped are mostly the timers of the work in the slices:
//! systemd refuses to start a unit in a frozen slice, so a timer firing
//! mid-game would fail its run. Stopped, it fires again once started, at
//! once for one that counts from its last run or keeps a stamp.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use miette::{bail, miette, IntoDiagnostic, WrapErr};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// systemd's own slice for a person's background work; focus.nix moves
/// their indexer, backups and watchers into it.
const USER_SLICE: &str = "background.slice";
/// The system's equivalent, which focus.nix defines and fills.
const SYSTEM_SLICE: &str = "losos-background.slice";
/// The units `system pause` stopped, one a line, for `resume` to start
/// again. The person's are in `$XDG_RUNTIME_DIR/losos-focus/stopped`.
const STOPPED: &str = "/run/losos-focus/stopped";

fn main() -> miette::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["start", units @ ..] => start(units),
        ["stop"] => stop(),
        ["status"] => status(),
        ["system", "pause", units @ ..] => pause(&[], SYSTEM_SLICE, units, Path::new(STOPPED)),
        ["system", "hold", user] => hold(user),
        ["system", "resume"] => system_resume(),
        _ => bail!(
            "usage: losos-focus start [<unit>...] | stop | status\n       \
             losos-focus system pause <unit>... | hold <user> | resume"
        ),
    }
}

fn start(units: &[&str]) -> miette::Result<()> {
    // The person's half first: it needs nobody's permission, so even when
    // the system's half is refused the indexer and backups wait.
    if let Err(e) = pause(&["--user"], USER_SLICE, units, &user_record()?) {
        warn!("{e:?}");
    }
    // --no-block: GameMode waits for this script, and the program that
    // asked for focus waits for GameMode.
    systemctl(&["--no-block", "start", &unit()?])?;
    info!("focus is on");
    Ok(())
}

fn stop() -> miette::Result<()> {
    if let Err(e) = resume(&["--user"], USER_SLICE, &user_record()?) {
        warn!("{e:?}");
    }
    systemctl(&["--no-block", "stop", &unit()?])?;
    info!("focus is off");
    Ok(())
}

fn status() -> miette::Result<()> {
    let user = state(&["--user"], USER_SLICE, "FreezerState");
    let system = state(&[], &unit()?, "ActiveState");
    println!("{USER_SLICE}: {user}");
    println!("{}: {system}", unit()?);
    Ok(())
}

/// Freezes `slice` and stops those of `units` that are running, adding
/// them to `record` so a second person's focus does not forget the
/// first's.
fn pause(scope: &[&str], slice: &str, units: &[&str], record: &Path) -> miette::Result<()> {
    freeze(scope, slice)?;
    if units.is_empty() {
        return Ok(());
    }
    let mut show = scope.to_vec();
    show.extend(["show", "-p", "Id,ActiveState"]);
    show.extend_from_slice(units);
    let running = losos_focus::running(&systemctl(&show)?);
    if running.is_empty() {
        return Ok(());
    }
    // Recorded before stopping, so a stop cut short is still undone.
    let mut stopped = std::fs::read_to_string(record).unwrap_or_default();
    for unit in &running {
        stopped.push_str(unit);
        stopped.push('\n');
    }
    if let Some(dir) = record.parent() {
        std::fs::create_dir_all(dir).into_diagnostic()?;
    }
    std::fs::write(record, stopped).into_diagnostic()?;
    let mut stop = scope.to_vec();
    stop.push("stop");
    stop.extend(running.iter().map(String::as_str));
    systemctl(&stop)?;
    info!("stopped {}", running.join(" "));
    Ok(())
}

/// Thaws `slice` and starts again what `pause` recorded.
fn resume(scope: &[&str], slice: &str, record: &Path) -> miette::Result<()> {
    let mut thaw = scope.to_vec();
    thaw.extend(["thaw", slice]);
    if let Err(e) = systemctl(&thaw) {
        warn!("{e:?}");
    }
    let stopped = std::fs::read_to_string(record).unwrap_or_default();
    let units: Vec<&str> = stopped.lines().filter(|l| !l.is_empty()).collect();
    if !units.is_empty() {
        let mut start = scope.to_vec();
        start.extend(["--no-block", "start"]);
        start.extend_from_slice(&units);
        systemctl(&start)?;
        info!("started {} again", units.join(" "));
    }
    let _ = std::fs::remove_file(record);
    Ok(())
}

/// Waits until `user` has logged out. GameMode goes with their session,
/// perhaps without running `stop`, and the system's updates must not stay
/// frozen until the next boot because of it.
fn hold(user: &str) -> miette::Result<()> {
    let out = Command::new("id")
        .args(["-u", user])
        .output()
        .into_diagnostic()?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || uid.is_empty() {
        bail!("there is no account {user}");
    }
    // Removed when the last of their sessions closes. A stat every few
    // seconds is all this costs while a game runs.
    let runtime = format!("/run/user/{uid}");
    while Path::new(&runtime).exists() {
        std::thread::sleep(Duration::from_secs(5));
    }
    info!("{user} logged out");
    Ok(())
}

/// Undoes `system pause`, unless another person's focus still holds.
fn system_resume() -> miette::Result<()> {
    // This instance is stopping, so it is not among the active ones.
    let others = systemctl(&[
        "list-units",
        "--plain",
        "--no-legend",
        "--state=active,activating",
        "losos-focus@*.service",
    ])?;
    if let Some(other) = others.lines().find_map(|l| l.split_whitespace().next()) {
        info!("{other} still holds focus");
        return Ok(());
    }
    resume(&[], SYSTEM_SLICE, Path::new(STOPPED))
}

/// Starts `slice` if nothing has yet, since systemd freezes only an active
/// unit and a slice is inactive until something runs in it, then freezes
/// it, so what starts in it from now on waits too.
fn freeze(scope: &[&str], slice: &str) -> miette::Result<()> {
    let mut start = scope.to_vec();
    start.extend(["start", slice]);
    systemctl(&start)?;
    let mut freeze = scope.to_vec();
    freeze.extend(["freeze", slice]);
    systemctl(&freeze).map(drop)
}

/// Where `start` records the person's units it stopped: their runtime
/// directory, which goes when they log out, as their units do.
fn user_record() -> miette::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| miette!("XDG_RUNTIME_DIR is not set: not in a session"))?;
    Ok(PathBuf::from(runtime).join("losos-focus/stopped"))
}

/// The person's losos-focus@ unit, which polkit lets only them start.
fn unit() -> miette::Result<String> {
    let user = std::env::var("USER")
        .ok()
        .or_else(|| {
            let out = Command::new("id").arg("-un").output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        })
        .ok_or_else(|| miette!("cannot tell who is running this"))?;
    losos_focus::instance(&user).ok_or_else(|| miette!("{user:?} is not an account name"))
}

fn state(scope: &[&str], unit: &str, property: &str) -> String {
    let mut args = scope.to_vec();
    args.extend(["show", "--value", "-p", property, unit]);
    systemctl(&args)
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|e| format!("{e}"))
}

fn systemctl(args: &[&str]) -> miette::Result<String> {
    let out = Command::new("systemctl")
        .args(args)
        .output()
        .into_diagnostic()
        .wrap_err("cannot run systemctl")?;
    if !out.status.success() {
        bail!(
            "systemctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
