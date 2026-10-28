//! The part of nmcli's language the host's programs speak, read and printed
//! the way NetworkManager's own nmcli does it, so they cannot tell the
//! difference: derisk's Network pages and first-boot setup
//! (derisk-install's network.rs and wifi.rs), Danube's captive portal
//! watch, and a person at a terminal asking the usual questions. Anything
//! else is refused with a sentence saying where it is not available, rather
//! than half done.

/// A request, as read from nmcli's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `nmcli general [status]`.
    General,
    /// `nmcli device [status]`.
    Devices,
    /// `nmcli device wifi [list] [ifname X] [--rescan yes|no|auto]`.
    WifiList {
        ifname: Option<String>,
        rescan: bool,
    },
    /// `nmcli device wifi rescan [ifname X]`.
    WifiRescan { ifname: Option<String> },
    /// `nmcli connection [show]`.
    Connections,
    /// `nmcli connection show [uuid|id] X`.
    Connection { key: Key },
    /// `nmcli connection add type wifi ...`.
    Add(Add),
    /// `nmcli connection up [uuid|id] X [ifname Y] [passwd-file F]`.
    Up {
        key: Key,
        passwd_file: Option<String>,
    },
    /// `nmcli connection down [uuid|id] X`.
    Down { key: Key },
    /// `nmcli connection delete [uuid|id] X`.
    Delete { key: Key },
    /// `nmcli device disconnect X` / `nmcli device connect X`.
    DeviceConnect { device: String, connect: bool },
    /// `nmcli radio wifi [on|off]`.
    Radio { on: Option<bool> },
    /// `nmcli monitor`.
    Monitor,
}

/// How a connection is named on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Uuid(String),
    Id(String),
}

/// `nmcli connection add type wifi`'s settings that this machine keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Add {
    pub ifname: Option<String>,
    pub name: Option<String>,
    pub ssid: String,
    pub save: bool,
    /// `wifi-sec.key-mgmt`: none, wpa-psk, sae or owe.
    pub key_mgmt: Option<String>,
    /// `wifi-sec.pmf`: required, optional or disable.
    pub pmf: Option<String>,
    /// `wifi-sec.psk`, when given on the command line rather than through a
    /// passwd-file.
    pub psk: Option<String>,
}

/// How the answer is printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// `-t`: values only, separated by colons.
    pub terse: bool,
    /// `-f`/`-g`: which fields, in order.
    pub fields: Option<Vec<String>>,
    /// `-g`: values only, no header, even without `-t`.
    pub values_only: bool,
}

/// The whole command line: how to print and what to do. `wait` is
/// `--wait`'s seconds, which `connection up` waits for at most.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub output: Output,
    pub wait: Option<u32>,
    pub request: Request,
}

/// Reads nmcli's arguments. The error is nmcli's sentence for them.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut output = Output {
        terse: false,
        fields: None,
        values_only: false,
    };
    let mut wait = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut it = args.iter().map(String::as_str);
    while let Some(arg) = it.next() {
        match arg {
            "-t" | "--terse" => output.terse = true,
            "-f" | "--fields" => {
                output.fields = Some(split_fields(
                    it.next().ok_or("option '-f' requires an argument")?,
                ))
            }
            "-g" | "--get-values" => {
                output.fields = Some(split_fields(
                    it.next().ok_or("option '-g' requires an argument")?,
                ));
                output.values_only = true;
                output.terse = true;
            }
            "-w" | "--wait" => {
                wait = Some(
                    it.next()
                        .and_then(|w| w.parse().ok())
                        .ok_or("option '--wait' requires a number of seconds")?,
                )
            }
            // Accepted and without effect here: colours, pretty tables, and
            // the default of asking nothing.
            "-c" | "--colors" | "-m" | "--mode" => {
                it.next();
            }
            "-p" | "--pretty" | "-e" | "--escape" => {}
            _ => {
                rest.push(arg);
                // Everything after the object is the command's own.
                rest.extend(it.by_ref());
            }
        }
    }
    let request = parse_request(&rest)?;
    Ok(Command {
        output,
        wait,
        request,
    })
}

fn split_fields(text: &str) -> Vec<String> {
    text.split(',').map(|f| f.trim().to_owned()).collect()
}

/// Whether `word` is an abbreviation of `full` that nmcli takes: any
/// leading part of it.
fn is(word: &str, full: &str) -> bool {
    !word.is_empty() && full.starts_with(word)
}

fn parse_key(words: &[&str]) -> Result<Key, String> {
    match words {
        [kind, value, ..] if *kind == "uuid" => Ok(Key::Uuid((*value).to_owned())),
        [kind, value, ..] if *kind == "id" => Ok(Key::Id((*value).to_owned())),
        [value, ..] => Ok(Key::Id((*value).to_owned())),
        [] => Err("no connection given".into()),
    }
}

/// The value after `name` among `words`, which go in name-value pairs.
fn after<'a>(words: &[&'a str], name: &str) -> Option<&'a str> {
    words
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1])
}

fn parse_request(words: &[&str]) -> Result<Request, String> {
    let unsupported = || {
        Err(format!(
            "'{}' is not available here: the network machine answers the commands Settings uses",
            words.join(" ")
        ))
    };
    match words {
        [] | ["g" | "ge" | "gen" | "general"] => Ok(Request::General),
        [g, s] if is(g, "general") && is(s, "status") => Ok(Request::General),
        [m] if is(m, "monitor") => Ok(Request::Monitor),
        [r, w, rest @ ..] if is(r, "radio") && is(w, "wifi") => match rest {
            [] => Ok(Request::Radio { on: None }),
            ["on"] => Ok(Request::Radio { on: Some(true) }),
            ["off"] => Ok(Request::Radio { on: Some(false) }),
            _ => unsupported(),
        },
        [d] if is(d, "device") => Ok(Request::Devices),
        [d, s] if is(d, "device") && is(s, "status") => Ok(Request::Devices),
        [d, c, device] if is(d, "device") && (*c == "connect" || is(c, "disconnect")) => {
            Ok(Request::DeviceConnect {
                device: (*device).to_owned(),
                connect: *c == "connect",
            })
        }
        [d, w, rest @ ..] if is(d, "device") && is(w, "wifi") => {
            let (verb, rest) = match rest {
                [v, rest @ ..] if is(v, "list") || is(v, "rescan") => (*v, rest),
                _ => ("list", rest),
            };
            let ifname = after(rest, "ifname").map(str::to_owned);
            if is(verb, "rescan") {
                Ok(Request::WifiRescan { ifname })
            } else {
                Ok(Request::WifiList {
                    ifname,
                    rescan: after(rest, "--rescan") == Some("yes"),
                })
            }
        }
        [c] if is(c, "connection") => Ok(Request::Connections),
        [c, s] if is(c, "connection") && is(s, "show") => Ok(Request::Connections),
        [c, s, rest @ ..] if is(c, "connection") && is(s, "show") => Ok(Request::Connection {
            key: parse_key(rest)?,
        }),
        [c, u, rest @ ..] if is(c, "connection") && *u == "up" => {
            let key = parse_key(rest)?;
            Ok(Request::Up {
                key,
                passwd_file: after(rest, "passwd-file").map(str::to_owned),
            })
        }
        [c, d, rest @ ..] if is(c, "connection") && is(d, "down") => Ok(Request::Down {
            key: parse_key(rest)?,
        }),
        [c, d, rest @ ..] if is(c, "connection") && is(d, "delete") => Ok(Request::Delete {
            key: parse_key(rest)?,
        }),
        [c, a, rest @ ..] if is(c, "connection") && is(a, "add") => {
            match after(rest, "type") {
                Some("wifi" | "802-11-wireless") => {}
                _ => return Err("only Wi-Fi connections can be added here".into()),
            }
            let ssid = after(rest, "ssid").ok_or("a Wi-Fi connection needs an ssid")?;
            Ok(Request::Add(Add {
                ifname: after(rest, "ifname")
                    .filter(|i| *i != "*")
                    .map(str::to_owned),
                name: after(rest, "con-name").map(str::to_owned),
                ssid: ssid.to_owned(),
                save: after(rest, "save") != Some("no"),
                key_mgmt: after(rest, "wifi-sec.key-mgmt")
                    .or_else(|| after(rest, "802-11-wireless-security.key-mgmt"))
                    .map(str::to_owned),
                pmf: after(rest, "wifi-sec.pmf")
                    .or_else(|| after(rest, "802-11-wireless-security.pmf"))
                    .map(str::to_owned),
                psk: after(rest, "wifi-sec.psk")
                    .or_else(|| after(rest, "802-11-wireless-security.psk"))
                    .map(str::to_owned),
            }))
        }
        _ => unsupported(),
    }
}

/// The secret in a passwd-file's text: the value of its `...psk:` line.
pub fn passwd_file_psk(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .ends_with(".psk")
            .then(|| value.to_owned())
            .or_else(|| (key.trim() == "psk").then(|| value.to_owned()))
    })
}

/// A row of a listing: each field by nmcli's name for it.
pub type Row = Vec<(&'static str, String)>;

/// Escapes a value for terse output: colons and backslashes get a backslash.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c == ':' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Prints `rows` as nmcli would: the fields asked for (or `default`), terse
/// as colon-separated values, otherwise as a table with a header.
pub fn format(rows: &[Row], output: &Output, default: &[&str]) -> Result<String, String> {
    let fields: Vec<String> = match &output.fields {
        Some(fields) if fields.iter().any(|f| f == "all") => rows
            .first()
            .map(|row| row.iter().map(|(k, _)| (*k).to_owned()).collect())
            .unwrap_or_default(),
        Some(fields) => fields.clone(),
        None => default.iter().map(|f| (*f).to_owned()).collect(),
    };
    if let Some(row) = rows.first() {
        if let Some(bad) = fields.iter().find(|f| !row.iter().any(|(k, _)| k == f)) {
            return Err(format!("invalid field '{bad}'"));
        }
    }
    let value = |row: &Row, field: &str| {
        row.iter()
            .find(|(k, _)| *k == field)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let mut out = String::new();
    if output.terse {
        for row in rows {
            let values: Vec<String> = fields.iter().map(|f| escape(&value(row, f))).collect();
            out.push_str(&values.join(":"));
            out.push('\n');
        }
        return Ok(out);
    }
    let widths: Vec<usize> = fields
        .iter()
        .map(|f| {
            rows.iter()
                .map(|row| value(row, f).chars().count())
                .chain([f.len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<String>| {
        let mut text = cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        text.truncate(text.trim_end().len());
        text + "\n"
    };
    out.push_str(&line(fields.clone()));
    for row in rows {
        out.push_str(&line(fields.iter().map(|f| value(row, f)).collect()));
    }
    Ok(out)
}

/// nmcli's BARS for a signal from 0 to 100.
pub fn bars(signal: u8) -> &'static str {
    match signal {
        76.. => "▂▄▆█",
        51..=75 => "▂▄▆_",
        26..=50 => "▂▄__",
        1..=25 => "▂___",
        0 => "____",
    }
}

/// An SSID's bytes as nmcli's SSID-HEX prints them.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn reads_what_derisk_asks() {
        let c = parse(&args("-t -f STATE general")).unwrap();
        assert_eq!(c.request, Request::General);
        assert!(c.output.terse);
        assert_eq!(c.output.fields, Some(vec!["STATE".into()]));

        let c = parse(&args("-t -g STATE general")).unwrap();
        assert!(c.output.values_only);

        assert_eq!(
            parse(&args("-t -f DEVICE,TYPE,STATE device"))
                .unwrap()
                .request,
            Request::Devices
        );
        assert_eq!(
            parse(&args(
                "-t -f IN-USE,SSID-HEX,SIGNAL,SECURITY device wifi list ifname wlan0 --rescan no"
            ))
            .unwrap()
            .request,
            Request::WifiList {
                ifname: Some("wlan0".into()),
                rescan: false
            }
        );
        assert_eq!(
            parse(&args("-t device wifi rescan ifname wlan0"))
                .unwrap()
                .request,
            Request::WifiRescan {
                ifname: Some("wlan0".into())
            }
        );
        assert_eq!(
            parse(&args("-t -f UUID,TYPE,ACTIVE connection show"))
                .unwrap()
                .request,
            Request::Connections
        );
        assert_eq!(
            parse(&args("-t -g 802-11-wireless.ssid connection show uuid abc"))
                .unwrap()
                .request,
            Request::Connection {
                key: Key::Uuid("abc".into())
            }
        );
        let c = parse(&args(
            "--wait 30 connection up uuid abc passwd-file /dev/stdin",
        ))
        .unwrap();
        assert_eq!(c.wait, Some(30));
        assert_eq!(
            c.request,
            Request::Up {
                key: Key::Uuid("abc".into()),
                passwd_file: Some("/dev/stdin".into())
            }
        );
        assert_eq!(
            parse(&args("-t connection delete uuid abc"))
                .unwrap()
                .request,
            Request::Delete {
                key: Key::Uuid("abc".into())
            }
        );
        assert_eq!(parse(&args("monitor")).unwrap().request, Request::Monitor);
    }

    #[test]
    fn reads_an_add() {
        let line: Vec<String> = [
            "-t",
            "connection",
            "add",
            "type",
            "wifi",
            "ifname",
            "wlan0",
            "con-name",
            "Home: 2",
            "ssid",
            "Home: 2",
            "save",
            "yes",
            "wifi-sec.key-mgmt",
            "sae",
            "wifi-sec.pmf",
            "required",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(
            parse(&line).unwrap().request,
            Request::Add(Add {
                ifname: Some("wlan0".into()),
                name: Some("Home: 2".into()),
                ssid: "Home: 2".into(),
                save: true,
                key_mgmt: Some("sae".into()),
                pmf: Some("required".into()),
                psk: None,
            })
        );
        assert!(parse(&args("connection add type ethernet ifname eth0")).is_err());
    }

    #[test]
    fn refuses_the_rest() {
        assert!(parse(&args("connection modify uuid abc ipv4.method manual")).is_err());
        assert!(parse(&args("networking off")).is_err());
    }

    #[test]
    fn reads_a_passwd_file() {
        assert_eq!(
            passwd_file_psk("802-11-wireless-security.psk:a:b c\n").as_deref(),
            Some("a:b c")
        );
        assert_eq!(passwd_file_psk("nothing\n"), None);
    }

    #[test]
    fn prints_like_nmcli() {
        let rows: Vec<Row> = vec![
            vec![
                ("DEVICE", "wlan0".into()),
                ("TYPE", "wifi".into()),
                ("STATE", "connected".into()),
                ("CONNECTION", "Home:2".into()),
            ],
            vec![
                ("DEVICE", "eth0".into()),
                ("TYPE", "ethernet".into()),
                ("STATE", "unavailable".into()),
                ("CONNECTION", "".into()),
            ],
        ];
        let terse = Output {
            terse: true,
            fields: Some(vec!["DEVICE".into(), "CONNECTION".into()]),
            values_only: false,
        };
        assert_eq!(
            format(&rows, &terse, &[]).unwrap(),
            "wlan0:Home\\:2\neth0:\n"
        );
        let table = Output {
            terse: false,
            fields: None,
            values_only: false,
        };
        assert_eq!(
            format(&rows, &table, &["DEVICE", "STATE"]).unwrap(),
            "DEVICE  STATE\nwlan0   connected\neth0    unavailable\n"
        );
        let bad = Output {
            terse: true,
            fields: Some(vec!["NOPE".into()]),
            values_only: false,
        };
        assert!(format(&rows, &bad, &[]).is_err());
        assert_eq!(hex(b"a\"b"), "612262");
    }
}
