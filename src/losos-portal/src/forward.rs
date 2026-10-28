//! `losos-portal forward`: the portal backend xdg-desktop-portal finds as
//! `org.freedesktop.impl.portal.desktop.losos`, on the host and in every
//! machine, for the portals whose dialogs run in the portal machine
//! ([`crate::PORTALS`]). Each request goes to the host's relay as it came,
//! and its answer comes back to the frontend.
//!
//! In a machine, files cross too. A file the person picked is copied in
//! before the app hears of it, and the app is handed the copy, which is
//! written back to the host whenever the app saves it ([`Staging`]). A
//! picture for the wallpaper goes the other way. On the host none of that
//! is needed: the portal machine sees the person's home at the same paths.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::{bounded, Sender};
use miette::{Context, IntoDiagnostic, Result};
use tracing::{debug, info, warn};
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zvariant::{OwnedValue, Value};

use crate::files::Staging;
use crate::{
    backend, path_from_uri, read_json, response, response_body, result_uris, send_stream,
    uri_from_path, write_frame, write_json, Body, Reply, Request, BUS_NAME, CLOSE, FILE_CHOOSER,
    HOST_CID, PATH, PORTALS, RELAY_PORT, RELAY_SOCKET, REQUEST, SCREENSHOT, WALLPAPER,
};

/// Where `forward` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// On the host, reaching the relay over its socket.
    Host,
    /// In a machine, reaching it over vsock.
    Machine,
}

/// A connection to the relay.
pub enum Relay {
    Unix(UnixStream),
    Vsock(vsock::VsockStream),
}

impl Relay {
    pub fn connect(place: Place) -> io::Result<Relay> {
        Ok(match place {
            Place::Host => Relay::Unix(UnixStream::connect(RELAY_SOCKET)?),
            Place::Machine => {
                Relay::Vsock(vsock::VsockStream::connect_with_cid_port(HOST_CID, RELAY_PORT)?)
            }
        })
    }

    fn try_clone(&self) -> io::Result<Relay> {
        Ok(match self {
            Relay::Unix(s) => Relay::Unix(s.try_clone()?),
            Relay::Vsock(s) => Relay::Vsock(s.try_clone()?),
        })
    }
}

impl Read for Relay {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Relay::Unix(s) => s.read(buf),
            Relay::Vsock(s) => s.read(buf),
        }
    }
}

impl Write for Relay {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Relay::Unix(s) => s.write(buf),
            Relay::Vsock(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Relay::Unix(s) => s.flush(),
            Relay::Vsock(s) => s.flush(),
        }
    }
}

/// The requests running now, by their object path, each with the way to
/// withdraw it.
type Running = Arc<Mutex<HashMap<String, Sender<()>>>>;

pub fn run(place: Place) -> Result<()> {
    let bus = Connection::session()
        .into_diagnostic()
        .wrap_err("connecting to the session bus")?;
    // Listening before the name is ours, so no call is missed.
    let calls = MessageIterator::from(&bus);
    bus.request_name(BUS_NAME)
        .into_diagnostic()
        .wrap_err_with(|| format!("taking {BUS_NAME}"))?;
    let staging = match place {
        Place::Machine => Some(Arc::new(
            Staging::open()
                .into_diagnostic()
                .wrap_err("making the folder for files from the host")?,
        )),
        Place::Host => None,
    };
    if let Some(staging) = &staging {
        let staging = Arc::clone(staging);
        thread::spawn(move || staging.write_back(|| Relay::connect(place)));
    }
    info!(?place, "serving {BUS_NAME}");

    let running: Running = Arc::default();
    for message in calls {
        let message = match message {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "session bus");
                continue;
            }
        };
        if message.message_type() != Type::MethodCall {
            continue;
        }
        let header = message.header();
        let (Some(path), Some(member)) = (header.path(), header.member()) else {
            continue;
        };
        let interface = header.interface().map(|i| i.to_string()).unwrap_or_default();
        let (path, member) = (path.to_string(), member.to_string());
        let answered = match (interface.as_str(), member.as_str()) {
            ("org.freedesktop.DBus.Properties", "GetAll") => {
                let iface: String = message.body().deserialize().unwrap_or_default();
                bus.reply(&header, &properties(&iface))
            }
            ("org.freedesktop.DBus.Properties", "Get") => {
                let (iface, name): (String, String) =
                    message.body().deserialize().unwrap_or_default();
                match properties(&iface).remove(name.as_str()) {
                    Some(value) => bus.reply(&header, &value),
                    None => bus.reply_dbus_error(
                        &header,
                        zbus::fdo::Error::UnknownProperty(format!("{iface} has no {name}")),
                    ),
                }
            }
            ("org.freedesktop.DBus.Introspectable", "Introspect") => {
                bus.reply(&header, &introspection(&path))
            }
            ("org.freedesktop.DBus.Peer", "Ping") => bus.reply(&header, &()),
            ("org.freedesktop.DBus.Peer", "GetMachineId") => bus.reply(
                &header,
                &std::fs::read_to_string("/etc/machine-id")
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            ),
            (REQUEST, "Close") => {
                if let Some(close) = running.lock().unwrap().remove(&path) {
                    let _ = close.send(());
                }
                bus.reply(&header, &())
            }
            (interface, _) if path == PATH && backend(interface).is_some() => {
                let (bus, running, staging) = (bus.clone(), Arc::clone(&running), staging.clone());
                let message = message.clone();
                thread::spawn(move || {
                    request(&bus, &message, place, staging.as_deref(), &running);
                });
                Ok(())
            }
            _ => bus.reply_dbus_error(
                &header,
                zbus::fdo::Error::UnknownMethod(format!("{interface}.{member} is not served here")),
            ),
        };
        if let Err(e) = answered {
            debug!(error = %e, "reply");
        }
    }
    Ok(())
}

/// The properties of an interface, answered here so the frontend's start,
/// which reads them all, does not start the portal machine.
fn properties(interface: &str) -> HashMap<&'static str, OwnedValue> {
    let mut properties = HashMap::new();
    // derisk's Screenshot is version 2, with PickColor.
    if interface == SCREENSHOT {
        properties.insert("version", OwnedValue::from(2u32));
    }
    properties
}

fn introspection(path: &str) -> String {
    let mut xml = String::from(
        "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n<node>\n",
    );
    if path == PATH {
        for (interface, _) in PORTALS {
            xml.push_str(&format!("  <interface name=\"{interface}\"/>\n"));
        }
    }
    xml.push_str("</node>\n");
    xml
}

/// One request, from the frontend to the relay and back.
fn request(
    bus: &Connection,
    message: &zbus::Message,
    place: Place,
    staging: Option<&Staging>,
    running: &Running,
) {
    let header = message.header();
    let interface = header.interface().map(|i| i.to_string()).unwrap_or_default();
    let method = header.member().map(|m| m.to_string()).unwrap_or_default();
    let reply = (|| -> std::result::Result<Body, Box<Reply>> {
        let body = Body::of(message).map_err(fail)?;
        let mut fields: Vec<Value> = body
            .fields()
            .map_err(fail)?
            .into_iter()
            .map(Value::from)
            .collect();
        let handle = match fields.first() {
            Some(Value::ObjectPath(path)) => Some(path.to_string()),
            _ => None,
        };
        outgoing(&interface, &method, &mut fields, place)?;
        let body = Body::from_fields(fields).map_err(fail)?;

        let mut relay = Relay::connect(place)
            .map_err(|e| fail(format!("cannot reach the portal relay: {e}")))?;
        write_json(
            &mut relay,
            &Request::Call {
                interface: interface.clone(),
                method: method.clone(),
                signature: body.signature.clone(),
                caller: None,
            },
        )
        .and_then(|_| write_frame(&mut relay, &body.bytes))
        .map_err(fail)?;

        // Withdrawn by the frontend: say so to the relay.
        let (close, closed) = bounded::<()>(1);
        if let Some(handle) = &handle {
            running.lock().unwrap().insert(handle.clone(), close);
        }
        let mut closer = relay.try_clone().map_err(fail)?;
        thread::spawn(move || {
            if closed.recv().is_ok() {
                let _ = write_frame(&mut closer, CLOSE);
            }
        });
        let answer: std::result::Result<Reply, io::Error> = read_json(&mut relay);
        if let Some(handle) = &handle {
            running.lock().unwrap().remove(handle);
        }
        let answer = answer.map_err(fail)?;
        if !answer.ok {
            return Err(Box::new(answer));
        }
        let body = Body {
            signature: answer.signature.unwrap_or_default(),
            bytes: crate::expect_frame(&mut relay).map_err(fail)?,
        };
        match staging {
            Some(staging) => incoming(&interface, &method, body, staging, place),
            None => Ok(body),
        }
    })();
    let sent = match reply {
        Ok(body) if body.signature.is_empty() => bus.reply(&header, &()),
        Ok(body) => match body.structure() {
            Ok(structure) => bus.reply(&header, &structure),
            Err(e) => bus.reply_dbus_error(&header, zbus::fdo::Error::Failed(e.to_string())),
        },
        Err(reply) => {
            let reply = *reply;
            let name = reply
                .error
                .unwrap_or_else(|| "org.freedesktop.DBus.Error.Failed".to_owned());
            let text = reply.message.unwrap_or_default();
            warn!(%interface, %method, error = %name, message = %text, "request failed");
            match zbus::Message::error(&header, name.as_str()).and_then(|b| b.build(&(text,))) {
                Ok(error) => bus.send(&error),
                Err(e) => Err(e),
            }
        }
    };
    if let Err(e) = sent {
        debug!(error = %e, "reply");
    }
}

fn fail(error: impl std::fmt::Display) -> Box<Reply> {
    Box::new(Reply::failed(error))
}

/// What leaves a machine with a request: nothing that names a path here,
/// which the portal machine cannot see, and a wallpaper's picture itself.
fn outgoing(
    interface: &str,
    method: &str,
    fields: &mut [Value<'static>],
    place: Place,
) -> std::result::Result<(), Box<Reply>> {
    if interface == WALLPAPER && method == "SetWallpaperURI" {
        // (handle, app_id, parent_window, uri, options)
        if let Some(Value::Str(uri)) = fields.get(3) {
            let local = path_from_uri(uri).ok_or_else(|| fail("not a local file"))?;
            let uploaded = upload(&local, place).map_err(fail)?;
            fields[3] = Value::from(uri_from_path(&uploaded));
        }
        return Ok(());
    }
    if place == Place::Machine && interface == FILE_CHOOSER {
        // (handle, app_id, parent_window, title, options)
        if let Some(Value::Dict(options)) = fields.get(4) {
            let options: HashMap<String, OwnedValue> =
                HashMap::try_from(Value::Dict(options.try_clone().map_err(fail)?))
                    .map_err(fail)?;
            let kept: HashMap<String, Value> = options
                .into_iter()
                .filter(|(key, _)| !matches!(key.as_str(), "current_folder" | "current_file"))
                .map(|(k, v)| (k, Value::from(v)))
                .collect();
            fields[4] = Value::from(kept);
        }
    }
    Ok(())
}

/// The relay's copy of a picture for the wallpaper, where the portal
/// machine can read it.
fn upload(local: &Path, place: Place) -> io::Result<std::path::PathBuf> {
    let mut file = std::fs::File::open(local)?;
    let mut relay = Relay::connect(place)?;
    write_json(
        &mut relay,
        &Request::Upload {
            name: local
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        },
    )?;
    send_stream(&mut relay, &mut file)?;
    let reply: Reply = read_json(&mut relay)?;
    match reply.path {
        Some(path) if reply.ok => Ok(path.into()),
        _ => Err(io::Error::other(reply.message.unwrap_or_default())),
    }
}

/// What reaches the app in a machine: its copies of the files the person
/// picked for it, in place of the host's.
fn incoming(
    interface: &str,
    method: &str,
    body: Body,
    staging: &Staging,
    place: Place,
) -> std::result::Result<Body, Box<Reply>> {
    if interface != FILE_CHOOSER && interface != SCREENSHOT {
        return Ok(body);
    }
    let Some((0, mut results)) = response(&body) else {
        return Ok(body);
    };
    // A file about to be saved has nothing to fetch yet.
    let fetch = !(interface == FILE_CHOOSER && method != "OpenFile");
    let mut local = Vec::new();
    for uri in result_uris(&results) {
        let host = path_from_uri(&uri).ok_or_else(|| fail("not a local file"))?;
        let copy = staging
            .place(&host, fetch, || Relay::connect(place))
            .map_err(|e| fail(format!("cannot copy {} here: {e}", host.display())))?;
        local.push(uri_from_path(&copy));
    }
    if results.contains_key("uris") {
        let uris = OwnedValue::try_from(Value::from(local.clone())).map_err(fail)?;
        results.insert("uris".to_owned(), uris);
    } else if let Some(uri) = local.first() {
        results.insert("uri".to_owned(), OwnedValue::from(zvariant::Str::from(uri.clone())));
    }
    response_body(0, results).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_sends_no_paths_of_its_own() {
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("current_folder", Value::from(b"/home/user\0".to_vec()));
        options.insert("current_name", Value::from("notes.txt"));
        let mut fields = vec![
            Value::from(zvariant::ObjectPath::try_from("/r/1").unwrap()),
            Value::from("org.example.App"),
            Value::from(""),
            Value::from("Save"),
            Value::from(options),
        ];
        outgoing(FILE_CHOOSER, "SaveFile", &mut fields, Place::Machine).unwrap();
        let Value::Dict(options) = &fields[4] else {
            panic!()
        };
        let options: HashMap<String, OwnedValue> =
            HashMap::try_from(Value::Dict(options.try_clone().unwrap())).unwrap();
        assert!(options.contains_key("current_name"));
        assert!(!options.contains_key("current_folder"));
    }

    #[test]
    fn the_frontend_reads_properties_without_the_machine() {
        assert_eq!(properties(SCREENSHOT).len(), 1);
        assert!(properties(FILE_CHOOSER).is_empty());
        assert!(introspection(PATH).contains(FILE_CHOOSER));
    }
}
