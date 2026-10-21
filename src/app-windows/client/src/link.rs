//! The connection to a guest's app server: where it is, the token that
//! lets this host in, and a writer thread so nothing that sends ever waits
//! on the network.

use std::io::{BufReader, BufWriter, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use app_windows_protocol::{self as protocol, Hello, Message, Welcome};
use crossbeam_channel::{unbounded, Sender};

/// Which server, and the secret it was set up with.
#[derive(Clone, Debug)]
pub struct Target {
    pub server: String,
    pub token_file: PathBuf,
}

/// The sending half of a connection. Dropping every clone closes it.
#[derive(Clone)]
pub struct Link {
    tx: Sender<Message>,
}

impl Link {
    /// A link to nowhere, for a recording played back: what is sent to it
    /// has no guest to go to.
    pub fn detached() -> Self {
        let (tx, rx) = unbounded::<Message>();
        thread::spawn(move || rx.into_iter().for_each(drop));
        Link { tx }
    }

    #[cfg(test)]
    pub fn for_test(tx: Sender<Message>) -> Self {
        Link { tx }
    }

    pub fn send(&self, message: Message) {
        // A closed connection is noticed by the reader, which reports it.
        let _ = self.tx.send(message);
    }
}

pub type Reader = BufReader<TcpStream>;

/// Connects and says hello, and returns what the guest said back.
pub fn connect(target: &Target) -> Result<(Welcome, Link, Reader), String> {
    let token = std::fs::read_to_string(&target.token_file).map_err(|e| {
        format!(
            "reading the token from {}: {e}",
            target.token_file.display()
        )
    })?;
    let address = target
        .server
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", target.server))?
        .next()
        .ok_or_else(|| format!("{} has no address", target.server))?;
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map_err(|e| format!("connecting to {}: {e}", target.server))?;
    let _ = stream.set_nodelay(true);
    let write_half = stream.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::with_capacity(1 << 20, stream);

    let (tx, rx) = unbounded::<Message>();
    thread::Builder::new()
        .name("send".into())
        .spawn(move || {
            let mut out = BufWriter::new(write_half);
            while let Ok(message) = rx.recv() {
                if protocol::write(&mut out, &message).is_err() {
                    return;
                }
                if rx.is_empty() && out.flush().is_err() {
                    return;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let link = Link { tx };
    link.send(Message::Hello(Hello {
        version: protocol::VERSION,
        token: token.trim().to_owned(),
    }));
    match protocol::read(&mut reader) {
        Ok(Message::Welcome(welcome)) => Ok((welcome, link, reader)),
        Ok(Message::Error(e)) => Err(format!("{} refused: {}", target.server, e.message)),
        Ok(other) => Err(format!("{} answered hello with {other:?}", target.server)),
        Err(e) => Err(format!("{} hung up: {e}", target.server)),
    }
}
