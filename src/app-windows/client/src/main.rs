//! The host's half of app windows (docs/app-windows.md): a guest's apps in
//! this desktop's app grid and palette, and each of its windows as a window
//! here, from a guest that runs an app server such as `macos-app-server`.
//!
//!     app-windows [OPTIONS] show             show the guest's windows, and keep its apps listed
//!     app-windows [OPTIONS] apps             list the guest's apps here, and print them
//!     app-windows [OPTIONS] open ID          start an app, and show its windows if nothing does
//!     app-windows [OPTIONS] record SCRIPT OUT
//!                                            run a test script with no screen (record.rs)
//!     app-windows [OPTIONS] replay FILE [SPEED]
//!                                            show a recording's windows, timed as they came
//!     app-windows [OPTIONS] send FILE...     hand files to the guest and open them there
//!     app-windows [OPTIONS] watch DIR [EXTENSION...]
//!                                            hand over each such file that lands in DIR
//!                                            (dmg, pkg and mpkg when none are named)
//!
//!     --server HOST:PORT   the guest's app server (127.0.0.1:7180)
//!     --token-file PATH    its secret ($XDG_CONFIG_HOME/app-windows/PREFIX/token)
//!     --prefix NAME        what app ids here start with (macos)
//!     --name NAME          what the guest is called in app descriptions (macOS)
//!     --exec COMMAND       what an app's entry runs, before `open ID`
//!                          (this program with these options)
//!     --wait SECONDS       how long `show` waits for the guest to answer (120)
//!     --software           decode on the processor even where the GPU could

// Used by the Wayland windows; tested on any system.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod cursor;
mod decode;
#[cfg(unix)]
mod derisk;
mod entries;
mod files;
mod link;
mod record;
mod stream;
#[cfg(target_os = "linux")]
mod wayland;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use app_windows_protocol::{self as protocol, AppInfo, Launch, Message};

use crate::link::Target;

#[derive(Clone)]
struct Options {
    target: Target,
    prefix: String,
    name: String,
    exec: Vec<String>,
    wait: Duration,
    hardware: bool,
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: app-windows [--server HOST:PORT] [--token-file PATH] [--prefix NAME] [--name NAME]"
    );
    eprintln!("                   [--exec COMMAND] [--wait SECONDS] [--software]");
    eprintln!("                   show | apps | open ID | record SCRIPT OUT | replay FILE [SPEED]");
    eprintln!("                   | send FILE... | watch DIR [EXTENSION...]");
    ExitCode::from(2)
}

fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => d.into(),
        _ => Path::new(&std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    }
}

fn parse(args: &mut Vec<String>) -> Result<Options, String> {
    let mut server = format!("127.0.0.1:{}", protocol::DEFAULT_PORT);
    let mut token_file = None;
    let mut prefix = "macos".to_owned();
    let mut name = "macOS".to_owned();
    let mut exec = None;
    let mut wait = Duration::from_secs(120);
    let mut hardware = true;
    let mut rest = Vec::new();
    let mut it = std::mem::take(args).into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--server" => server = value()?,
            "--token-file" => token_file = Some(PathBuf::from(value()?)),
            "--prefix" => prefix = value()?,
            "--name" => name = value()?,
            "--exec" => exec = Some(value()?.split_whitespace().map(str::to_owned).collect()),
            "--wait" => {
                wait = Duration::from_secs(value()?.parse().map_err(|e| format!("--wait: {e}"))?)
            }
            "--software" => hardware = false,
            _ => rest.push(arg),
        }
    }
    *args = rest;
    // The prefix becomes part of file names and app ids.
    if prefix.is_empty()
        || !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(format!(
            "--prefix {prefix:?}: letters, digits and dashes only"
        ));
    }
    let token_file = token_file.unwrap_or_else(|| {
        config_home()
            .join("app-windows")
            .join(&prefix)
            .join("token")
    });
    let exec = match exec {
        Some(e) => e,
        None => {
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            vec![
                me.to_string_lossy().into_owned(),
                "--server".into(),
                server.clone(),
                "--token-file".into(),
                token_file.to_string_lossy().into_owned(),
                "--prefix".into(),
                prefix.clone(),
                "--name".into(),
                name.clone(),
            ]
        }
    };
    Ok(Options {
        target: Target { server, token_file },
        prefix,
        name,
        exec,
        wait,
        hardware,
    })
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse(&mut args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("app-windows: {e}");
            return usage();
        }
    };
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["show"] => show(&options),
        ["apps"] => apps(&options),
        ["open", id] => open(&options, id),
        ["record", script, out] => match record::main(
            &options.target,
            Path::new(script),
            Path::new(out),
            options.hardware,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err("the script failed; see the report".into()),
            Err(e) => Err(e),
        },
        ["send", paths @ ..] if !paths.is_empty() => {
            let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
            // What "Open in macOS" runs, so it waits for a guest that is
            // still starting.
            connect_waiting(&options)
                .and_then(|(_, link, mut reader)| files::send(&link, &mut reader, &paths, true))
                .map(|saved| {
                    for there in saved {
                        println!("{there}");
                    }
                })
        }
        ["watch", folder, extensions @ ..] => {
            let extensions: Vec<String> = if extensions.is_empty() {
                files::MAC_PACKAGES.iter().map(|x| x.to_string()).collect()
            } else {
                extensions
                    .iter()
                    .map(|x| x.trim_start_matches('.').to_owned())
                    .collect()
            };
            files::watch(&options.target, Path::new(folder), &extensions)
        }
        ["replay", file] => replay(&options, Path::new(file), 1.0),
        ["replay", file, speed] => match speed.parse() {
            Ok(speed) if speed > 0.0 => replay(&options, Path::new(file), speed),
            _ => Err(format!("{speed} is not a speed")),
        },
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("app-windows: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Writes the guest's apps as entries here.
fn sync(options: &Options, apps: &[AppInfo]) {
    let places = match entries::Places::of_this_user() {
        Ok(p) => p,
        Err(e) => return eprintln!("app-windows: {e}"),
    };
    match entries::sync(&places, &options.prefix, &options.name, &options.exec, apps) {
        Ok(n) => eprintln!("app-windows: {n} {} apps listed", options.name),
        Err(e) => eprintln!("app-windows: listing {} apps: {e}", options.name),
    }
}

fn apps(options: &Options) -> Result<(), String> {
    let (welcome, link, mut reader) = link::connect(&options.target)?;
    eprintln!("{} runs {}", welcome.name, welcome.system);
    link.send(Message::ListApps);
    loop {
        match protocol::read(&mut reader).map_err(|e| e.to_string())? {
            Message::Apps(apps) => {
                sync(options, &apps);
                for app in &apps {
                    println!("{}\t{}", app.id, app.name);
                }
                return Ok(());
            }
            Message::Error(e) => return Err(e.message),
            _ => {}
        }
    }
}

/// Where `show` notes that it runs, so that `open` knows whether to.
fn lock_path(options: &Options) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    dir.join(format!("app-windows-{}.lock", options.prefix))
}

fn open(options: &Options, id: &str) -> Result<(), String> {
    let (_, link, mut reader) = connect_waiting(options)?;
    link.send(Message::Launch(Launch { id: id.to_owned() }));
    // The guest says nothing when the app starts, and an error at once
    // when it cannot; a moment is enough to tell.
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)));
    if let Ok(Message::Error(e)) = protocol::read(&mut reader) {
        return Err(e.message);
    }
    drop(link);
    let lock = std::fs::File::create(lock_path(options)).map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        // Something already shows the guest's windows; the app's will come.
        return Ok(());
    }
    drop(lock);
    show(options)
}

/// Connects, trying again while the guest is still starting.
fn connect_waiting(
    options: &Options,
) -> Result<(protocol::Welcome, link::Link, link::Reader), String> {
    let started = Instant::now();
    loop {
        match link::connect(&options.target) {
            Ok(connected) => return Ok(connected),
            Err(e) if started.elapsed() < options.wait && !e.contains("refused:") => {
                eprintln!("app-windows: {e}; trying again");
                thread::sleep(Duration::from_secs(2));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(target_os = "linux")]
fn show(options: &Options) -> Result<(), String> {
    let lock = std::fs::File::create(lock_path(options)).map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        return Err(format!("{}'s windows are already shown here", options.name));
    }
    let (welcome, link, reader) = connect_waiting(options)?;
    eprintln!(
        "app-windows: showing {}'s windows ({}), video encoded in {}",
        welcome.name, welcome.system, welcome.encoder
    );
    if !welcome.missing.is_empty() {
        eprintln!(
            "app-windows: {} still has to allow this in its System Settings: {}",
            welcome.name,
            welcome.missing.join(", ")
        );
    }
    let hardware = options.hardware;
    let reading = link.clone();
    let sync_options = options.clone();
    let shown = wayland::Options {
        prefix: options.prefix.clone(),
        exit_when_lost: true,
        on_apps: Box::new(move |apps| sync(&sync_options, apps)),
    };
    let result = wayland::show(link, shown, move |frames, sink| {
        thread::Builder::new()
            .name("read".into())
            .spawn(move || {
                let options = stream::Options {
                    hardware,
                    recording: None,
                    every_picture: false,
                };
                stream::run(reader, reading, options, frames, Arc::default(), sink);
            })
            .expect("spawning the reader");
    });
    drop(lock);
    result
}

#[cfg(target_os = "linux")]
fn replay(options: &Options, file: &Path, speed: f64) -> Result<(), String> {
    let source =
        stream::Replay::open(file, speed).map_err(|e| format!("{}: {e}", file.display()))?;
    let link = link::Link::detached();
    let reading = link.clone();
    let hardware = options.hardware;
    // The recorded guest's apps go in the app grid, as `show` puts a live
    // guest's there.
    let sync_options = options.clone();
    let shown = wayland::Options {
        prefix: options.prefix.clone(),
        exit_when_lost: false,
        on_apps: Box::new(move |apps| sync(&sync_options, apps)),
    };
    wayland::show(link, shown, move |frames, sink| {
        thread::spawn(move || {
            let options = stream::Options {
                hardware,
                recording: None,
                every_picture: true,
            };
            stream::run(source, reading, options, frames, Arc::default(), sink);
        });
    })
}

#[cfg(not(target_os = "linux"))]
fn show(_: &Options) -> Result<(), String> {
    Err("showing windows needs a Wayland desktop; `record` works anywhere".into())
}

#[cfg(not(target_os = "linux"))]
fn replay(options: &Options, _: &Path, _: f64) -> Result<(), String> {
    show(options)
}
