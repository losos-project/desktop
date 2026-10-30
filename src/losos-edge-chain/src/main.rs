//! Carries the schizo gateway's connections out through the person's LosOS
//! edge, over Tor.
//!
//!     losos-edge-chain --listen <addr:port> --tor <addr:port> [--config <file>]
//!
//! The gateway's firewall sends a connection here only while the host has
//! chosen an edge. The edge file is read again for every connection, so
//! choosing another edge or none takes effect on the next one without a
//! restart; a connection that finds no file, or a file that is wrong, is
//! closed, never sent out another way (docs/schizo-mode.md, "Through an
//! edge").

use std::io::{self, IsTerminal};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use losos_edge_chain::{may_leave, open, original_dst, splice, Edge, EDGE_FILE};

struct Args {
    listen: SocketAddr,
    tor: SocketAddr,
    config: PathBuf,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .without_time()
        .init();

    match args().and_then(|a| serve(&a)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

fn args() -> Result<Args> {
    let mut listen = None;
    let mut tor = None;
    let mut config = PathBuf::from(EDGE_FILE);
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| miette!("{flag} takes a value"))?;
        match flag.as_str() {
            "--listen" => listen = Some(value.parse().into_diagnostic().wrap_err("--listen")?),
            "--tor" => tor = Some(value.parse().into_diagnostic().wrap_err("--tor")?),
            "--config" => config = PathBuf::from(value),
            _ => bail!(
                "usage: losos-edge-chain --listen <addr:port> --tor <addr:port> [--config <file>]"
            ),
        }
    }
    Ok(Args {
        listen: listen.ok_or_else(|| miette!("--listen is required"))?,
        tor: tor.ok_or_else(|| miette!("--tor is required"))?,
        config,
    })
}

fn serve(args: &Args) -> Result<()> {
    let listener = TcpListener::bind(args.listen)
        .into_diagnostic()
        .wrap_err_with(|| format!("listening on {}", args.listen))?;
    info!(listen = %args.listen, tor = %args.tor, config = %args.config.display(), "carrying connections through the edge");
    for client in listener.incoming() {
        let client = match client {
            Ok(c) => c,
            Err(e) => {
                warn!("accept: {e}");
                continue;
            }
        };
        let (listen, tor, config) = (args.listen, args.tor, args.config.clone());
        // One thread per connection: each spends most of its life blocked
        // in a copy, and the gateway carries one person's apps.
        let spawned = std::thread::Builder::new()
            .name("connection".into())
            .spawn(move || carry(client, listen, tor, &config));
        if let Err(e) = spawned {
            warn!("no thread for a connection: {e}");
        }
    }
    Ok(())
}

fn carry(client: TcpStream, listen: SocketAddr, tor: SocketAddr, config: &Path) {
    let Ok(peer) = client.peer_addr() else { return };
    let dst = match original_dst(&client) {
        Ok(d) if may_leave(d, listen) => d,
        Ok(d) => {
            warn!(%peer, dst = %d, "not a redirected connection, closed");
            return;
        }
        Err(e) => {
            warn!(%peer, "where it was going: {e}");
            return;
        }
    };
    let edge = match Edge::read(config) {
        Ok(edge) => edge,
        Err(e) => {
            // The firewall should not have sent it here without a file; the
            // host is switching the edge off or on. Closed either way.
            warn!(%peer, %dst, "no edge to carry it ({}): {e}", config.display());
            return;
        }
    };
    let out = match open(tor, &edge, peer.ip(), dst) {
        Ok(s) => s,
        Err(e) => {
            warn!(%peer, %dst, "through the edge: {e}");
            return;
        }
    };
    debug!(%peer, %dst, "through the edge");
    let _ = client.set_nodelay(true);
    splice(&client, &out);
}
