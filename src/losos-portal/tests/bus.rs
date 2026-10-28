//! A request's body through a real session bus and back, as `forward`
//! receives it and `serve` sends it on: every value the portals use keeps
//! its type. Skipped where there is no dbus-daemon.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use losos_portal::Body;
use zbus::blocking::MessageIterator;
use zvariant::Value;

#[test]
fn bodies_cross_a_bus_unchanged() {
    // The session bus's own configuration, beside the daemon in its
    // package: a build sandbox has no /etc/dbus-1.
    let Some(config) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("../share/dbus-1/session.conf"))
            .find(|conf| conf.exists())
    }) else {
        eprintln!("no dbus-daemon; skipped");
        return;
    };
    let socket = std::env::temp_dir().join(format!("losos-portal-bus-{}", std::process::id()));
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--nofork", "--print-address"])
        .arg(format!("--config-file={}", config.display()))
        .arg(format!("--address=unix:path={}", socket.display()))
        .stdout(Stdio::piped())
        .spawn()
    else {
        eprintln!("no dbus-daemon; skipped");
        return;
    };
    let mut address = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(daemon.stdout.take().unwrap()),
        &mut address,
    )
    .unwrap();
    let address = address.trim().to_owned();
    // A bus with no `guid`, which zbus would otherwise check, as it may.
    let address = address.split(",guid=").next().unwrap().to_owned();

    let server = zbus::blocking::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .unwrap();
    let calls = MessageIterator::from(&server);
    server.request_name("org.example.Echo").unwrap();
    let echo = std::thread::spawn(move || {
        for message in calls {
            let message = message.unwrap();
            if message.message_type() != zbus::message::Type::MethodCall {
                continue;
            }
            // What forward does with a request, then what serve does.
            let body = Body::of(&message).unwrap();
            let sent = Body {
                signature: body.signature.clone(),
                bytes: body.bytes.clone(),
            };
            server
                .reply(&message.header(), &sent.structure().unwrap())
                .unwrap();
            return body.signature;
        }
        String::new()
    });

    let client = zbus::blocking::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .unwrap();
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("multiple", Value::from(true));
    options.insert("current_name", Value::from("notes.txt"));
    options.insert(
        "filters",
        Value::from(vec![("Text", vec![(0u32, "*.txt")])]),
    );
    let handle =
        zvariant::ObjectPath::try_from("/org/freedesktop/portal/desktop/request/1_2/t").unwrap();
    let reply = client
        .call_method(
            Some("org.example.Echo"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.FileChooser"),
            "OpenFile",
            &(
                handle.clone(),
                "org.example.App",
                "",
                "Open a file",
                options,
            ),
        )
        .unwrap();
    let (got_handle, app, parent, title, got): (
        zvariant::OwnedObjectPath,
        String,
        String,
        String,
        HashMap<String, zvariant::OwnedValue>,
    ) = reply.body().deserialize().unwrap();
    assert_eq!(got_handle.as_str(), handle.as_str());
    assert_eq!(
        (app.as_str(), parent.as_str(), title.as_str()),
        ("org.example.App", "", "Open a file")
    );
    assert!(bool::try_from(&got["multiple"]).unwrap());
    assert!(got.contains_key("filters"));
    assert_eq!(echo.join().unwrap(), "osssa{sv}");
    let _ = daemon.kill();
}
