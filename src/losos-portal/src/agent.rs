//! `losos-portal agent`, in the portal machine: the derisk session's agent
//! socket as derisk's portal there expects it, at
//! `$XDG_RUNTIME_DIR/derisk/agent.sock`, answering the one request it
//! makes, a screenshot. The relay takes the picture from the person's
//! session on the host, and it is kept here for the portal to frame and
//! save. Nothing else the session's socket can do is reachable from here.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use miette::{Context, IntoDiagnostic, Result};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::{read_json, receive_stream, write_json, Reply, Request, HOST_CID, RELAY_PORT};

/// The last few screenshots, as the session keeps them.
const KEEP: u64 = 4;

pub fn run() -> Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| miette::miette!("XDG_RUNTIME_DIR is not set"))?;
    let dir = runtime.join("derisk");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .into_diagnostic()?;
    let socket = dir.join("agent.sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .into_diagnostic()
        .wrap_err_with(|| format!("listening on {}", socket.display()))?;
    info!(socket = %socket.display(), "answering derisk's portal");
    let next = AtomicU64::new(0);
    thread::scope(|scope| {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (dir, next) = (&dir, &next);
            scope.spawn(move || {
                if let Err(e) = one(stream, dir, next) {
                    warn!(error = %e, "agent request");
                }
            });
        }
    });
    Ok(())
}

fn one(mut stream: UnixStream, dir: &Path, next: &AtomicU64) -> io::Result<()> {
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line).unwrap_or_default();
    let answer = if request["method"] == "screenshot" {
        let n = next.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("screenshot-{}.png", n % KEEP));
        match screenshot(&path) {
            Ok(()) => json!({"ok": true, "result": {"path": path}}),
            Err(e) => json!({"ok": false, "error": format!("no screenshot: {e}")}),
        }
    } else {
        json!({"ok": false, "error": "only screenshots are asked for here"})
    };
    let mut reply = answer.to_string();
    reply.push('\n');
    stream.write_all(reply.as_bytes())
}

fn screenshot(path: &Path) -> io::Result<()> {
    let mut relay = vsock::VsockStream::connect_with_cid_port(HOST_CID, RELAY_PORT)?;
    write_json(&mut relay, &Request::Screenshot)?;
    let reply: Reply = read_json(&mut relay)?;
    if !reply.ok {
        return Err(io::Error::other(reply.message.unwrap_or_default()));
    }
    let mut file = fs::File::create(path)?;
    receive_stream(&mut relay, &mut file)?;
    Ok(())
}
