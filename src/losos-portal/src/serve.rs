//! `losos-portal serve`, in the portal machine as the person: each request
//! the relay passes on is put to the backend that answers it on this
//! machine's session bus, GTK's or derisk's, whose dialog then shows on
//! the desktop through the machine's waypipe.
//!
//! Only the relay can ask: a request must come from the host, from a port
//! only root there can bind.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use miette::{Context, IntoDiagnostic, Result};
use tracing::{debug, info, warn};
use vsock::{VsockListener, VsockStream, VMADDR_CID_ANY};
use zvariant::Value;

use crate::{
    backend, read_json, write_frame, write_json, Body, Reply, Request, FILE_CHOOSER, HOST_CID,
    PATH, PRIVILEGED_PORTS, REQUEST, SERVE_PORT,
};

/// Each request's own object path on this bus, where its backend serves
/// the Request object the relay's `close` reaches.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

pub fn run() -> Result<()> {
    let bus = zbus::blocking::Connection::session()
        .into_diagnostic()
        .wrap_err("connecting to the session bus")?;
    let listener = VsockListener::bind_with_cid_port(VMADDR_CID_ANY, SERVE_PORT)
        .into_diagnostic()
        .wrap_err_with(|| format!("listening on vsock port {SERVE_PORT}"))?;
    info!(port = SERVE_PORT, "serving the portals");
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                warn!(error = %e, "accept");
                continue;
            }
        };
        match stream.peer_addr() {
            Ok(peer) if peer.cid() == HOST_CID && PRIVILEGED_PORTS.contains(&peer.port()) => {}
            Ok(peer) => {
                warn!(cid = peer.cid(), port = peer.port(), "refused: not the host's relay");
                continue;
            }
            Err(e) => {
                warn!(error = %e, "refused: no peer address");
                continue;
            }
        }
        let bus = bus.clone();
        thread::spawn(move || {
            if let Err(e) = one(&bus, stream) {
                debug!(error = %e, "request");
            }
        });
    }
    Ok(())
}

fn one(bus: &zbus::blocking::Connection, mut stream: VsockStream) -> io::Result<()> {
    let Request::Call {
        interface,
        method,
        signature,
        caller,
    } = read_json(&mut stream)?
    else {
        return write_json(&mut stream, &Reply::failed("only calls are served here"));
    };
    let body = Body {
        signature,
        bytes: crate::expect_frame(&mut stream)?,
    };
    let Some(destination) = backend(&interface) else {
        return write_json(
            &mut stream,
            &Reply::error(
                "org.freedesktop.DBus.Error.UnknownInterface",
                format!("{interface} is not served here"),
            ),
        );
    };
    let handle = format!(
        "{PATH}/request/losos_portal/r{}",
        REQUESTS.fetch_add(1, Ordering::Relaxed)
    );
    let body = match prepare(&interface, &method, body, &handle, caller.as_deref()) {
        Ok(body) => body,
        Err(e) => return write_json(&mut stream, &Reply::failed(e)),
    };
    info!(%interface, %method, caller = caller.as_deref().unwrap_or("?"), "request");

    // The relay closes the request by sending `close`, or by going away.
    let mut closer = stream.try_clone()?;
    let close_bus = bus.clone();
    let close_handle = handle.clone();
    let close_backend = destination.to_owned();
    let done = Arc::new(AtomicBool::new(false));
    let answered = Arc::clone(&done);
    thread::spawn(move || {
        // Anything but a frame that is not `close` withdraws the request:
        // `close` itself, or the relay gone.
        let frame = crate::read_frame(&mut closer);
        let withdrawn = !matches!(frame, Ok(Some(ref f)) if f != crate::CLOSE);
        if withdrawn && !answered.load(Ordering::SeqCst) {
            let _ = close_bus.call_method(
                Some(close_backend.as_str()),
                close_handle.as_str(),
                Some(REQUEST),
                "Close",
                &(),
            );
        }
    });

    let reply = call(bus, destination, &interface, &method, &body);
    match reply {
        Ok(body) => {
            write_json(
                &mut stream,
                &Reply {
                    signature: Some(body.signature.clone()),
                    ..Reply::ok()
                },
            )?;
            write_frame(&mut stream, &body.bytes)?;
        }
        Err(zbus::Error::MethodError(name, message, _)) => {
            write_json(
                &mut stream,
                &Reply::error(name.as_str(), message.unwrap_or_default()),
            )?;
        }
        Err(e) => write_json(&mut stream, &Reply::failed(e))?,
    }
    stream.flush()?;
    done.store(true, Ordering::SeqCst);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// The request as this machine's backend takes it: the request object at
/// this bus's own path, no parent window (the app's is on another
/// machine's display, and means nothing here), and a file chooser's title
/// that says which machine asks.
pub fn prepare(
    interface: &str,
    method: &str,
    body: Body,
    handle: &str,
    caller: Option<&str>,
) -> zbus::Result<Body> {
    let mut fields: Vec<Value> = body.fields()?.into_iter().map(Value::from).collect();
    if matches!(fields.first(), Some(Value::ObjectPath(_))) {
        fields[0] = Value::from(zvariant::ObjectPath::try_from(handle)?.into_owned());
    }
    if matches!(fields.get(2), Some(Value::Str(_))) {
        fields[2] = Value::from("");
    }
    let machine = caller.filter(|c| *c != "host");
    if interface == FILE_CHOOSER && matches!(method, "OpenFile" | "SaveFile" | "SaveFiles") {
        if let (Some(machine), Some(Value::Str(title))) = (machine, fields.get(3)) {
            fields[3] = Value::from(format!("{machine}: {title}"));
        }
    }
    Body::from_fields(fields)
}

fn call(
    bus: &zbus::blocking::Connection,
    destination: &str,
    interface: &str,
    method: &str,
    body: &Body,
) -> zbus::Result<Body> {
    // No timeout: a dialog stays up for as long as the person leaves it.
    let reply = if body.signature.is_empty() {
        bus.call_method(Some(destination), PATH, Some(interface), method, &())?
    } else {
        bus.call_method(
            Some(destination),
            PATH,
            Some(interface),
            method,
            &body.structure()?,
        )?
    };
    Body::of(&reply)
}
