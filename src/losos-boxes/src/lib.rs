//! LosOS boxes on the local network, found through systemd-resolved's DNS-SD
//! browser and offered as applications (docs/losos-boxes.md).
//!
//! A box announces itself as `_losos._tcp` on its front port. Boxes from
//! before that announcement only publish `_workstation._tcp`, as most Linux
//! machines running Avahi do, so each workstation is also asked for
//! `/setup/state.json`, a document only a box serves.
//!
//! This file is the part without I/O: the Varlink framing, what resolved's
//! replies mean, which boxes are present, and the `.desktop` file each one
//! gets. main.rs does the talking.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde_json::{json, Value};

/// resolved's Varlink socket, which any user may call for lookups and
/// browsing.
pub const RESOLVE_SOCKET: &str = "/run/systemd/resolve/io.systemd.Resolve";
/// Where the LosOS boxes machine reaches the host's resolved instead
/// (`relay`): a vsock port on the host, address 2.
pub const RELAY_PORT: u32 = 7351;
/// The template a machine is made from to be let through to the host's
/// resolved: the LosOS boxes machine's (nixos/guest/boxes.nix).
pub const MACHINE_TEMPLATE: &str = "losos-boxes";
/// Where that machine tells the host which names are boxes (`route`), so
/// the host's DNS for machines answers them with the machine's address.
pub const NAMES_PORT: u32 = 7352;
/// Where the host keeps those answers: a hosts file per LosOS boxes machine,
/// which libvirt's dnsmasq reads as it changes (libvirt.nix).
pub const HOSTS_DIR: &str = "/run/losos-boxes/hosts";
/// The ports the LosOS boxes machine passes on to a box for the other
/// machines: the front door's HTTP and HTTPS, where a box serves its admin
/// pages, its cloud and its Git.
pub const ROUTED_PORTS: [u16; 2] = [80, 443];
/// What a box announces.
pub const BOX_TYPE: &str = "_losos._tcp";
/// What a box from before `_losos._tcp` announces, among every other Avahi
/// machine.
pub const WORKSTATION_TYPE: &str = "_workstation._tcp";
/// mDNS's domain, the only one a box is on.
pub const DOMAIN: &str = "local";
/// Every entry this program writes starts with this, so it can find and
/// remove its own and nothing else.
pub const ENTRY_PREFIX: &str = "losos-box-";

/// One Varlink call: its JSON and the NUL that ends a message.
pub fn frame(method: &str, parameters: Value, more: bool) -> Vec<u8> {
    let mut call = json!({ "method": method, "parameters": parameters });
    if more {
        call["more"] = Value::Bool(true);
    }
    let mut bytes = serde_json::to_vec(&call).expect("a JSON value serializes");
    bytes.push(0);
    bytes
}

/// One Varlink reply: its parameters, and whether more replies follow.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub parameters: Value,
    pub continues: bool,
}

/// Parses one message, without its NUL. A Varlink error becomes `Err` with
/// its name and parameters, so the caller can say what resolved refused.
pub fn parse_reply(message: &[u8]) -> Result<Reply, String> {
    let value: Value =
        serde_json::from_slice(message).map_err(|e| format!("not a Varlink reply: {e}"))?;
    let parameters = value.get("parameters").cloned().unwrap_or(Value::Null);
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(if parameters.is_null() {
            error.to_owned()
        } else {
            format!("{error} {parameters}")
        });
    }
    Ok(Reply {
        continues: value.get("continues").and_then(Value::as_bool) == Some(true),
        parameters,
    })
}

/// One service instance as resolved's browser reports it. A box on two links,
/// or on one link over IPv4 and IPv6, is several instances of one service.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instance {
    pub name: String,
    pub service_type: String,
    pub domain: String,
    pub ifindex: i64,
    pub family: i64,
}

/// A browser's news about one instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Added(Instance),
    Removed(Instance),
}

/// The instances one `BrowseServices` reply adds and removes. Entries without
/// a name, which resolved allows for a plain SRV service, cannot be resolved
/// by name and are skipped.
pub fn browse_events(parameters: &Value) -> impl Iterator<Item = Seen> + '_ {
    parameters
        .get("browserServiceData")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let instance = Instance {
                name: item.get("name")?.as_str()?.to_owned(),
                service_type: item.get("type")?.as_str()?.to_owned(),
                domain: item.get("domain")?.as_str()?.to_owned(),
                ifindex: item.get("ifindex")?.as_i64()?,
                family: item.get("family")?.as_i64()?,
            };
            match item.get("updateFlag")?.as_str()? {
                "added" => Some(Seen::Added(instance)),
                "removed" => Some(Seen::Removed(instance)),
                _ => None,
            }
        })
}

/// The parameters of a `ResolveService` call for one instance.
pub fn resolve_parameters(instance: &Instance) -> Value {
    json!({
        "name": instance.name,
        "type": instance.service_type,
        "domain": instance.domain,
        "ifindex": instance.ifindex,
    })
}

/// What `ResolveService` says about an instance: the host and port of its
/// SRV record, the host's addresses, and its TXT strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The host name without its final dot, `mattbox.local`.
    pub host: String,
    pub port: u16,
    /// Each address with the interface it was found on, which an IPv6
    /// link-local address needs as its scope.
    pub addresses: Vec<(IpAddr, u32)>,
    pub txt: Vec<String>,
}

/// Reads a `ResolveService` reply. The first SRV record wins: a box
/// publishes one.
pub fn parse_resolved(parameters: &Value) -> Option<Resolved> {
    let service = parameters.get("services")?.as_array()?.first()?;
    let host = service
        .get("hostname")?
        .as_str()?
        .trim_end_matches('.')
        .to_owned();
    let port = u16::try_from(service.get("port")?.as_u64()?).ok()?;
    let addresses = service
        .get("addresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let ifindex = a
                .get("ifindex")
                .and_then(Value::as_u64)
                .and_then(|i| u32::try_from(i).ok())
                .unwrap_or(0);
            let octets: Vec<u8> = a
                .get("address")?
                .as_array()?
                .iter()
                .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect::<Option<_>>()?;
            let ip = match octets.len() {
                4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(octets).ok()?)),
                16 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(octets).ok()?)),
                _ => return None,
            };
            Some((ip, ifindex))
        })
        .collect();
    let txt = parameters
        .get("txt")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect();
    Some(Resolved {
        host,
        port,
        addresses,
        txt,
    })
}

/// A TXT record's value for `key`, from its `key=value` strings.
pub fn txt_value<'a>(txt: &'a [String], key: &str) -> Option<&'a str> {
    txt.iter()
        .filter_map(|t| t.split_once('='))
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

/// Whether `body` is a box's `/setup/state.json`. Every box writes
/// `hostName`, `fqdn` and `tls` into it (setup.nix in the LosOS repository),
/// which together do not turn up by accident.
pub fn is_box_state(body: &[u8]) -> bool {
    let Ok(state) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    state.get("hostName").is_some_and(Value::is_string)
        && state.get("fqdn").is_some_and(Value::is_string)
        && state.get("tls").is_some_and(Value::is_boolean)
}

/// The request that asks a workstation for a box's state, over HTTP/1.0 so
/// the answer comes whole, without chunks, and the connection closes after.
pub fn state_request(host: &str) -> String {
    format!("GET /setup/state.json HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\n\r\n")
}

/// The body of an HTTP response if its status is 200.
pub fn ok_body(response: &[u8]) -> Option<&[u8]> {
    let end = response.windows(4).position(|w| w == b"\r\n\r\n")?;
    let status = response[..end].split(|&b| b == b'\r').next()?;
    let mut words = status.split(|&b| b == b' ');
    words.next()?.starts_with(b"HTTP/").then_some(())?;
    (words.next()? == b"200").then_some(&response[end + 4..])
}

/// A box, as the entry for it shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LososBox {
    /// Its `.local` name, `mattbox.local`.
    pub host: String,
    /// Its front port, 80 on every box so far.
    pub port: u16,
    /// The release its announcement names, when it announces one.
    pub version: Option<String>,
}

impl LososBox {
    /// A box at `host`, if `host` is a host name. Anything on the LAN can
    /// answer mDNS, and the name ends up on an `Exec` line, so only the
    /// characters a DNS name has get that far.
    pub fn new(host: &str, port: u16, version: Option<&str>) -> Option<LososBox> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let valid = !host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            });
        valid.then(|| LososBox {
            host,
            port,
            version: version
                .filter(|v| {
                    !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_graphic())
                })
                .map(str::to_owned),
        })
    }

    /// A box from its `_losos._tcp` announcement.
    pub fn announced(resolved: &Resolved) -> Option<LososBox> {
        LososBox::new(
            &resolved.host,
            resolved.port,
            txt_value(&resolved.txt, "version"),
        )
    }

    /// The address of its admin pages, and of the box for `losos`, which
    /// pins whatever certificate the box serves there. By name rather than
    /// by address, as the handbook's links are, so it survives a new lease.
    pub fn url(&self) -> String {
        match self.port {
            80 => format!("http://{}", self.host),
            port => format!("http://{}:{port}", self.host),
        }
    }

    /// What the box calls itself: the name without `.local`.
    pub fn name(&self) -> &str {
        self.host
            .strip_suffix(".local")
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.host)
    }

    /// The file its entry is written to: the prefix and the host.
    pub fn file_name(&self) -> String {
        format!("{ENTRY_PREFIX}{}.desktop", self.host)
    }

    /// The `.desktop` file that offers the box in the palette and the app
    /// list. Opening it does what `opens` says, and its one action does the
    /// other: the admin pages in the default browser, or the box in `losos`,
    /// in a terminal.
    pub fn desktop_entry(&self, opens: Opens) -> String {
        let url = self.url();
        let comment = match &self.version {
            Some(version) => format!("LosOS {version} box at {}", self.host),
            None => format!("LosOS box at {}", self.host),
        };
        let name = escape(self.name());
        let pages = format!("xdg-open {url}/");
        let terminal = format!(
            "derisk-terminal --app-id=org.losos.Boxes --title={name} losos-boxes terminal {url}"
        );
        let (exec, action, action_name, action_exec) = match opens {
            Opens::AdminPages => (pages, "terminal", "Manage in the terminal", terminal),
            Opens::Terminal => (terminal, "pages", "Open the admin pages", pages),
        };
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={name}\n\
             GenericName=LosOS box\n\
             Comment={comment}\n\
             Keywords=LosOS;box;server;home;{host};\n\
             Icon=network-server\n\
             Exec={exec}\n\
             Actions={action};\n\
             \n\
             [Desktop Action {action}]\n\
             Name={action_name}\n\
             Exec={action_exec}\n",
            comment = escape(&comment),
            host = escape(&self.host),
        )
    }
}

/// What opening a box's entry does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opens {
    /// Its admin pages, in the default browser. The action opens `losos`.
    AdminPages,
    /// `losos`, in a terminal, as the LosOS boxes machine's entries do: that
    /// machine is where boxes are managed. The action opens the pages.
    Terminal,
}

/// The vsock address and the template of a machine, from its libvirt
/// definition: the `<cid>` libvirt gave it, and the template pm recorded in
/// its `<metadata>` (`<pm:template name='losos-boxes' .../>`), under
/// whatever prefix libvirt writes the namespace with.
pub fn machine_cid_and_template(xml: &str) -> (Option<u32>, Option<&str>) {
    let attribute = |element: &str, name: &str| -> Option<&str> {
        let start = xml.find(element)? + element.len();
        let rest = &xml[start..];
        let rest = &rest[..rest.find('>')?];
        let value = &rest[rest.find(&format!("{name}="))? + name.len() + 1..];
        let quote = value.chars().next().filter(|q| *q == '\'' || *q == '"')?;
        let value = &value[1..];
        Some(&value[..value.find(quote)?])
    };
    let cid = attribute("<cid ", "address").and_then(|a| a.parse().ok());
    let template = attribute(":template ", "name").or_else(|| attribute("<template ", "name"));
    (cid, template)
}

/// The addresses in a `ResolveHostname` reply, IPv4 first.
pub fn hostname_addresses(parameters: &Value) -> Vec<IpAddr> {
    let mut addresses: Vec<IpAddr> = parameters
        .get("addresses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let octets: Vec<u8> = a
                .get("address")?
                .as_array()?
                .iter()
                .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect::<Option<_>>()?;
            match octets.len() {
                4 => Some(IpAddr::V4(Ipv4Addr::from(
                    <[u8; 4]>::try_from(octets).ok()?,
                ))),
                16 => Some(IpAddr::V6(Ipv6Addr::from(
                    <[u8; 16]>::try_from(octets).ok()?,
                ))),
                _ => None,
            }
        })
        .collect();
    addresses.sort_by_key(IpAddr::is_ipv6);
    addresses
}

/// The host an HTTP request is for, from its `Host` header, without a port
/// and in lower case; `None` until `head` holds every header line.
pub fn http_host(head: &[u8]) -> Option<String> {
    let end = head.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&head[..end]).ok()?;
    let value = head.split("\r\n").skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("host")
            .then(|| value.trim())
    })?;
    let host = match value.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => value,
    };
    Some(host.trim_end_matches('.').to_ascii_lowercase())
}

/// How many bytes the TLS record that `start` begins holds, header and all,
/// once its header is there: what has to be read before
/// [`tls_server_name`] can look.
pub fn tls_record_len(start: &[u8]) -> Option<usize> {
    (start.len() >= 5).then(|| 5 + usize::from(u16::from_be_bytes([start[3], start[4]])))
}

/// The name a TLS client asked for in its ClientHello's server_name
/// extension, in lower case. `hello` is the first record the client sent.
pub fn tls_server_name(hello: &[u8]) -> Option<String> {
    let take = |data: &mut &[u8], n: usize| -> Option<Vec<u8>> {
        let (head, rest) = (data.get(..n)?, data.get(n..)?);
        *data = rest;
        Some(head.to_vec())
    };
    let len = |data: &mut &[u8], n: usize| -> Option<usize> {
        Some(
            take(data, n)?
                .iter()
                .fold(0, |sum, b| sum << 8 | usize::from(*b)),
        )
    };
    let mut data = hello;
    // A handshake record, holding a ClientHello.
    if take(&mut data, 5)?.first() != Some(&0x16) || take(&mut data, 1)? != [1] {
        return None;
    }
    let n = len(&mut data, 3)?;
    let mut data = data.get(..n)?;
    // Version and random, then the session, cipher suites and compression.
    take(&mut data, 34)?;
    for width in [1, 2, 1] {
        let n = len(&mut data, width)?;
        take(&mut data, n)?;
    }
    let n = len(&mut data, 2)?;
    let mut extensions = data.get(..n)?;
    while !extensions.is_empty() {
        let kind = len(&mut extensions, 2)?;
        let size = len(&mut extensions, 2)?;
        let body = take(&mut extensions, size)?;
        if kind != 0 {
            continue;
        }
        let mut names = body.get(2..)?;
        while !names.is_empty() {
            let name_type = len(&mut names, 1)?;
            let n = len(&mut names, 2)?;
            let name = take(&mut names, n)?;
            if name_type == 0 {
                let name = String::from_utf8(name).ok()?;
                return Some(name.trim_end_matches('.').to_ascii_lowercase());
            }
        }
    }
    None
}

/// A hosts file for libvirt's dnsmasq: each of `names` at `address`.
pub fn hosts_file<'a>(address: IpAddr, names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(|name| format!("{address} {name}\n"))
        .collect()
}

/// A string value for a `.desktop` file: its escapes, and `;` kept literal
/// where a list would split it.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace(';', "\\;")
}

/// The boxes present, by host, each with the instances that make it present.
/// A box is announced once per link and address family, and again as a
/// workstation, so it stays until the last of those is gone.
#[derive(Debug, Default)]
pub struct Boxes {
    boxes: BTreeMap<String, (LososBox, BTreeSet<Instance>)>,
    hosts: BTreeMap<Instance, String>,
}

/// What a change to [`Boxes`] means for the entries on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Write this box's entry, new or with something in it changed.
    Write(LososBox),
    /// Remove the entry with this file name.
    Remove(String),
}

impl Boxes {
    /// Records `instance` as showing `found`. A box already present keeps
    /// the version its own announcement gave when a workstation probe, which
    /// knows none, arrives later.
    pub fn add(&mut self, instance: Instance, found: LososBox) -> Option<Change> {
        if let Some(previous) = self.hosts.get(&instance) {
            if *previous != found.host {
                // The instance moved to another host: forget the old pairing.
                let change = self.remove(&instance);
                let added = self.add(instance, found);
                return added.or(change);
            }
        }
        self.hosts.insert(instance.clone(), found.host.clone());
        match self.boxes.get_mut(&found.host) {
            Some((current, instances)) => {
                instances.insert(instance);
                let merged = LososBox {
                    version: found.version.or_else(|| current.version.clone()),
                    ..found
                };
                (merged != *current).then(|| {
                    *current = merged.clone();
                    Change::Write(merged)
                })
            }
            None => {
                self.boxes.insert(
                    found.host.clone(),
                    (found.clone(), BTreeSet::from([instance])),
                );
                Some(Change::Write(found))
            }
        }
    }

    /// Forgets `instance`, and the box with it when it was the last.
    pub fn remove(&mut self, instance: &Instance) -> Option<Change> {
        let host = self.hosts.remove(instance)?;
        let (found, instances) = self.boxes.get_mut(&host)?;
        instances.remove(instance);
        if !instances.is_empty() {
            return None;
        }
        let file = found.file_name();
        self.boxes.remove(&host);
        Some(Change::Remove(file))
    }

    /// The boxes present, by host.
    pub fn iter(&self) -> impl Iterator<Item = &LososBox> {
        self.boxes.values().map(|(b, _)| b)
    }
}

/// The URLs `losos hosts` lists: each host's line starts in the first
/// column with its URL, and the lines under it are indented.
pub fn logged_in_hosts(output: &str) -> impl Iterator<Item = &str> {
    output
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|url| url.contains("://"))
}

/// The boxes among those, from `losos --lang en hosts`: the word after a
/// host's URL is its kind, and an edge has no files to search.
pub fn logged_in_boxes(output: &str) -> impl Iterator<Item = &str> {
    output
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?, words.next()?))
        })
        .filter(|(url, kind)| url.contains("://") && *kind == "box")
        .map(|(url, _)| url)
}

/// What a box is asked for at most per search and app: the palette shows
/// nine rows, and the desktop's own files share them.
pub const SEARCH_LIMIT: usize = 5;

/// The two searches inside a box's files that `losos-boxes search` makes:
/// LosOS Cloud's files and LosOS Git's repositories, the paths
/// `losos call GET` takes.
pub fn text_paths(text: &str) -> [(&'static str, String); 2] {
    let q = encode(text);
    ["cloud", "git"].map(|app| (app, format!("/api/{app}/text?q={q}&limit={SEARCH_LIMIT}")))
}

/// `text` as one URL component: everything but RFC 3986's unreserved
/// characters is percent-encoded, a byte at a time.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Each segment of a `/`-separated path encoded on its own, empty ones
/// dropped, as the box's own palette builds its links.
fn segments(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// The box's name in a URL `losos hosts` lists: its host without `.local`,
/// as the desktop names the box's app.
pub fn box_name(url: &str) -> &str {
    let host = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = host.split(['/', ':']).next().unwrap_or(host);
    host.strip_suffix(".local").unwrap_or(host)
}

/// A box's answer to one search as rows for the palette, each with the
/// address that shows it in the box's own pages. An answer the box could
/// not give (`available: false`, an app it does not run, a search that is
/// off) has none, and so has one that is not JSON.
///
/// Each row is `{box, app, title, place, snippet, url}`: the file's name,
/// the folder or repository it is in, the text that matched as one line,
/// and where to open it. A cloud file opens its folder in LosOS Cloud,
/// which knows paths and not the ids its own links use; a line in Git
/// opens the file at that line.
pub fn text_hits(base: &str, app: &str, answer: &Value) -> Vec<Value> {
    let base = base.trim_end_matches('/');
    let name = box_name(base);
    let none = Vec::new();
    let hits = match answer.get("available") {
        Some(Value::Bool(false)) => &none,
        _ => answer["hits"].as_array().unwrap_or(&none),
    };
    hits.iter()
        .filter_map(|hit| {
            let text = |key: &str| hit[key].as_str().filter(|s| !s.is_empty());
            let path = text("path")?.trim_matches('/');
            let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
            let snippet = one_line(&hit["snippet"]);
            let (place, url) = match app {
                "cloud" => (
                    if folder.is_empty() {
                        "LosOS Cloud".to_owned()
                    } else {
                        format!("LosOS Cloud › {folder}")
                    },
                    format!("{base}/cloud/files/{}", segments(folder))
                        .trim_end_matches('/')
                        .to_owned(),
                ),
                "git" => {
                    let owner = text("owner").filter(|o| is_git_name(o))?;
                    let repo = text("repo").filter(|r| is_git_name(r))?;
                    let branch = text("branch")?;
                    let line = hit["line"].as_u64().filter(|l| *l >= 1)?;
                    (
                        format!("{owner}/{repo}"),
                        format!(
                            "{base}/git/{owner}/{repo}/src/branch/{}/{}#L{line}",
                            segments(branch),
                            segments(path)
                        ),
                    )
                }
                _ => return None,
            };
            Some(json!({
                "box": name,
                "app": app,
                "title": file,
                "place": place,
                "snippet": snippet,
                "url": url,
            }))
        })
        .collect()
}

/// An owner's or a repository's name as LosOS Git allows it, which is what
/// goes into a link unescaped.
fn is_git_name(name: &str) -> bool {
    name.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// A box's snippet, alternating runs of plain and matched text, as one
/// line: the runs joined, every run of whitespace one space.
fn one_line(snippet: &Value) -> String {
    let joined: String = snippet
        .as_array()
        .map(|runs| runs.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str, service_type: &str, family: i64) -> Instance {
        Instance {
            name: name.into(),
            service_type: service_type.into(),
            domain: "local".into(),
            ifindex: 2,
            family,
        }
    }

    fn mattbox(version: Option<&str>) -> LososBox {
        LososBox {
            host: "mattbox.local".into(),
            port: 80,
            version: version.map(str::to_owned),
        }
    }

    #[test]
    fn frames_a_call_with_more() {
        let bytes = frame(
            "io.systemd.Resolve.BrowseServices",
            json!({"type": BOX_TYPE, "domain": DOMAIN}),
            true,
        );
        assert_eq!(bytes.last(), Some(&0));
        let call: Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(call["method"], "io.systemd.Resolve.BrowseServices");
        assert_eq!(call["parameters"]["type"], "_losos._tcp");
        assert_eq!(call["more"], true);
        let once = frame("io.systemd.Resolve.ResolveService", json!({}), false);
        let call: Value = serde_json::from_slice(&once[..once.len() - 1]).unwrap();
        assert!(call.get("more").is_none());
    }

    #[test]
    fn reads_replies_and_errors() {
        let reply = parse_reply(br#"{"parameters":{"a":1},"continues":true}"#).unwrap();
        assert!(reply.continues);
        assert_eq!(reply.parameters["a"], 1);
        let error = parse_reply(br#"{"error":"io.systemd.Resolve.NoSuchResourceRecord"}"#);
        assert_eq!(
            error,
            Err("io.systemd.Resolve.NoSuchResourceRecord".to_owned())
        );
        assert!(parse_reply(b"not json").is_err());
    }

    #[test]
    fn reads_browse_events() {
        // The shape resolved's varlink-io.systemd.Resolve.c defines.
        let reply = json!({"browserServiceData": [
            {"updateFlag": "added", "family": 2, "name": "mattbox",
             "type": "_losos._tcp", "domain": "local", "ifindex": 2},
            {"updateFlag": "removed", "family": 10, "name": "old",
             "type": "_losos._tcp", "domain": "local", "ifindex": 3},
            {"updateFlag": "added", "family": 2, "name": null,
             "type": "_losos._tcp", "domain": "local", "ifindex": 2}
        ]});
        let events: Vec<Seen> = browse_events(&reply).collect();
        assert_eq!(
            events,
            [
                Seen::Added(instance("mattbox", BOX_TYPE, 2)),
                Seen::Removed(Instance {
                    ifindex: 3,
                    ..instance("old", BOX_TYPE, 10)
                }),
            ]
        );
        assert_eq!(browse_events(&json!({})).count(), 0);
    }

    #[test]
    fn reads_a_resolved_service() {
        let reply = json!({
            "services": [{"priority": 0, "weight": 0, "port": 80,
                "hostname": "mattbox.local.",
                "addresses": [
                    {"ifindex": 2, "family": 2, "address": [192, 168, 1, 23]},
                    {"ifindex": 2, "family": 10,
                     "address": [254,128,0,0,0,0,0,0,0,0,0,0,0,0,0,1]}
                ]}],
            "txt": ["txtvers=1", "version=v0.4.0"],
            "canonical": {"name": "mattbox", "type": "_losos._tcp", "domain": "local"},
            "flags": 0
        });
        let resolved = parse_resolved(&reply).unwrap();
        assert_eq!(resolved.host, "mattbox.local");
        assert_eq!(resolved.port, 80);
        assert_eq!(
            resolved.addresses,
            [
                ("192.168.1.23".parse().unwrap(), 2),
                ("fe80::1".parse().unwrap(), 2)
            ]
        );
        assert_eq!(txt_value(&resolved.txt, "version"), Some("v0.4.0"));
        assert_eq!(
            LososBox::announced(&resolved),
            Some(mattbox(Some("v0.4.0")))
        );
        assert!(parse_resolved(&json!({"services": []})).is_none());
    }

    #[test]
    fn knows_a_box_by_its_state() {
        let state = br#"{"hostName":"mattbox","fqdn":"mattbox.local","address":"","tls":true,
            "certificate":{"fingerprint":"sha256:00"}}"#;
        assert!(is_box_state(state));
        // Something else's JSON, or a page that is not JSON at all.
        assert!(!is_box_state(br#"{"hostName":"x"}"#));
        assert!(!is_box_state(b"<html>"));
    }

    #[test]
    fn reads_an_http_answer() {
        assert!(state_request("mattbox.local").starts_with("GET /setup/state.json HTTP/1.0\r\n"));
        assert!(state_request("mattbox.local").ends_with("\r\n\r\n"));
        let ok = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        assert_eq!(ok_body(ok), Some(&b"{\"a\":1}"[..]));
        assert_eq!(ok_body(b"HTTP/1.1 404 Not Found\r\n\r\nnope"), None);
        assert_eq!(ok_body(b"HTTP/1.1 200 OK\r\n"), None);
        assert_eq!(ok_body(b"SSH-2.0-OpenSSH\r\n\r\n"), None);
    }

    #[test]
    fn writes_an_entry() {
        let entry = mattbox(Some("v0.4.0")).desktop_entry(Opens::AdminPages);
        assert!(entry.contains("\nName=mattbox\n"));
        assert!(entry.contains("\nComment=LosOS v0.4.0 box at mattbox.local\n"));
        assert!(entry.contains("\nExec=xdg-open http://mattbox.local/\n"));
        assert!(entry.contains("\n[Desktop Action terminal]\nName=Manage in the terminal\n"));
        assert!(entry.contains("losos-boxes terminal http://mattbox.local\n"));
        let entry = mattbox(None).desktop_entry(Opens::Terminal);
        assert!(entry.contains(
            "\nExec=derisk-terminal --app-id=org.losos.Boxes --title=mattbox \
             losos-boxes terminal http://mattbox.local\n"
        ));
        assert!(entry.contains("\n[Desktop Action pages]\nName=Open the admin pages\n"));
        assert!(entry.contains("\nExec=xdg-open http://mattbox.local/\n"));
        assert_eq!(mattbox(None).file_name(), "losos-box-mattbox.local.desktop");
        let other = LososBox::new("MattBox.local.", 8080, Some("v0.4.0 x")).unwrap();
        assert_eq!(other.host, "mattbox.local");
        assert_eq!(other.version, None);
        assert_eq!(other.url(), "http://mattbox.local:8080");
        // Whatever a stranger on the LAN answers mDNS with, nothing but a
        // host name reaches an Exec line.
        for host in [
            "box 1.local",
            "box;rm.local",
            "",
            "-x.local",
            "a..local",
            "x\n.local",
        ] {
            assert_eq!(LososBox::new(host, 80, None), None, "{host}");
        }
    }

    #[test]
    fn reads_a_machines_address_and_template() {
        // Abridged from `virsh dumpxml` of a running machine pm made.
        let xml = "<domain type='qemu' id='1'>\n  <name>boxes</name>\n  <metadata>\n    \
                   <pm:machine xmlns:pm=\"https://losos-project.github.io/xmlns/pm-machine/1\">\n      \
                   <pm:label>green</pm:label>\n      \
                   <pm:template name='losos-boxes' digest='0123abcd'/>\n    \
                   </pm:machine>\n  </metadata>\n  <devices>\n    \
                   <vsock model='virtio'>\n      <cid auto='yes' address='3'/>\n    </vsock>\n";
        assert_eq!(
            machine_cid_and_template(xml),
            (Some(3), Some("losos-boxes"))
        );
        let other = xml.replace("pm:", "ns0:").replace("'3'", "'17'");
        assert_eq!(
            machine_cid_and_template(&other),
            (Some(17), Some("losos-boxes"))
        );
        // Not running, so no address; made from a CD, so no template.
        assert_eq!(
            machine_cid_and_template("<domain><name>win</name></domain>"),
            (None, None)
        );
    }

    #[test]
    fn reads_resolved_hostnames() {
        let reply = json!({ "addresses": [
            { "ifindex": 2, "family": 10, "address": [254, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1] },
            { "ifindex": 2, "family": 2, "address": [192, 168, 1, 20] },
        ], "name": "mattbox.local" });
        assert_eq!(
            hostname_addresses(&reply),
            [
                "192.168.1.20".parse::<IpAddr>().unwrap(),
                "fe80::1".parse().unwrap()
            ]
        );
        assert_eq!(hostname_addresses(&json!({})), Vec::<IpAddr>::new());
    }

    #[test]
    fn reads_the_host_a_request_is_for() {
        let request = b"GET / HTTP/1.1\r\nUser-Agent: x\r\nHOST: MattBox.local:80\r\n\r\nbody";
        assert_eq!(http_host(request).as_deref(), Some("mattbox.local"));
        assert_eq!(
            http_host(b"GET / HTTP/1.1\r\nHost: mattbox.local\r\n"),
            None
        );
        assert_eq!(http_host(b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n"), None);
    }

    #[test]
    fn reads_the_name_a_tls_client_asks_for() {
        // A ClientHello for mattbox.local, as rustls sends one, cut to the
        // fields the parser walks: one cipher suite, no session, and the
        // server_name extension after another one.
        let name = b"MattBox.local";
        let mut sni = vec![0, 0];
        let list_len = (3 + name.len()) as u16;
        let mut list = list_len.to_be_bytes().to_vec();
        list.push(0);
        list.extend((name.len() as u16).to_be_bytes());
        list.extend(name);
        sni.extend((list.len() as u16).to_be_bytes());
        sni.extend(&list);
        let mut extensions = vec![0, 23, 0, 0];
        extensions.extend(sni);
        let mut body = vec![3, 3];
        body.extend([7; 32]);
        body.extend([0, 0, 2, 0x13, 1, 1, 0]);
        body.extend((extensions.len() as u16).to_be_bytes());
        body.extend(extensions);
        let mut handshake = vec![1, 0];
        handshake.extend((body.len() as u16).to_be_bytes());
        handshake.extend(body);
        let mut record = vec![0x16, 3, 1];
        record.extend((handshake.len() as u16).to_be_bytes());
        record.extend(handshake);
        assert_eq!(tls_record_len(&record), Some(record.len()));
        assert_eq!(tls_server_name(&record).as_deref(), Some("mattbox.local"));
        assert_eq!(tls_server_name(&record[..record.len() - 3]), None);
        assert_eq!(tls_server_name(b"GET / HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn writes_a_hosts_file() {
        let at = "192.168.122.40".parse().unwrap();
        assert_eq!(
            hosts_file(at, ["mattbox.local", "oldbox.local"]),
            "192.168.122.40 mattbox.local\n192.168.122.40 oldbox.local\n"
        );
    }

    #[test]
    fn a_box_stays_until_its_last_instance_goes() {
        let mut boxes = Boxes::default();
        let v4 = instance("mattbox", BOX_TYPE, 2);
        let v6 = instance("mattbox", BOX_TYPE, 10);
        let ws = instance("mattbox [00:11:22:33:44:55]", WORKSTATION_TYPE, 2);

        assert_eq!(
            boxes.add(v4.clone(), mattbox(Some("v0.4.0"))),
            Some(Change::Write(mattbox(Some("v0.4.0"))))
        );
        assert_eq!(boxes.add(v6.clone(), mattbox(Some("v0.4.0"))), None);
        // The workstation probe knows no version and keeps the announced one.
        assert_eq!(boxes.add(ws.clone(), mattbox(None)), None);
        assert_eq!(boxes.iter().count(), 1);

        assert_eq!(boxes.remove(&v4), None);
        assert_eq!(boxes.remove(&ws), None);
        assert_eq!(
            boxes.remove(&v6),
            Some(Change::Remove("losos-box-mattbox.local.desktop".into()))
        );
        assert_eq!(boxes.remove(&v6), None);
        assert_eq!(boxes.iter().count(), 0);
    }

    #[test]
    fn a_later_announcement_adds_the_version() {
        let mut boxes = Boxes::default();
        let ws = instance("mattbox [00:11:22:33:44:55]", WORKSTATION_TYPE, 2);
        assert_eq!(
            boxes.add(ws, mattbox(None)),
            Some(Change::Write(mattbox(None)))
        );
        assert_eq!(
            boxes.add(instance("mattbox", BOX_TYPE, 2), mattbox(Some("v0.5.0"))),
            Some(Change::Write(mattbox(Some("v0.5.0"))))
        );
    }

    #[test]
    fn reads_losos_hosts() {
        let output = "https://mattbox.local  box\n  host key: AAAA BBBB\n  your key: CCCC\n\
                      http://edge.example  edge\n  host key: DDDD\n";
        assert_eq!(
            logged_in_hosts(output).collect::<Vec<_>>(),
            ["https://mattbox.local", "http://edge.example"]
        );
        assert_eq!(
            logged_in_hosts("You are not logged in to any host.\n").count(),
            0
        );
    }

    #[test]
    fn searches_only_boxes() {
        let output = "https://mattbox.local  box\n  host key: AAAA BBBB\n\
                      http://edge.example  edge\n  host key: DDDD\n";
        assert_eq!(
            logged_in_boxes(output).collect::<Vec<_>>(),
            ["https://mattbox.local"]
        );
    }

    #[test]
    fn asks_both_apps_with_the_text_encoded() {
        assert_eq!(
            text_paths("rent & milk/ä"),
            [
                (
                    "cloud",
                    "/api/cloud/text?q=rent%20%26%20milk%2F%C3%A4&limit=5".to_owned()
                ),
                (
                    "git",
                    "/api/git/text?q=rent%20%26%20milk%2F%C3%A4&limit=5".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn names_the_box_after_its_host() {
        assert_eq!(box_name("http://mattbox.local"), "mattbox");
        assert_eq!(box_name("https://mattbox.local:8443/"), "mattbox");
        assert_eq!(box_name("https://box.example.org"), "box.example.org");
    }

    #[test]
    fn a_cloud_hit_opens_its_folder() {
        let answer = json!({
            "available": true,
            "partial": false,
            "hits": [
                {"path": "Documents/Taxes 2026/budget.md", "snippet": ["the ", "rent", " is\n  due"]},
                {"path": "todo.txt", "snippet": ["", "rent"]},
                {"path": "", "snippet": []},
            ],
        });
        let hits = text_hits("http://mattbox.local/", "cloud", &answer);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0],
            json!({
                "box": "mattbox",
                "app": "cloud",
                "title": "budget.md",
                "place": "LosOS Cloud › Documents/Taxes 2026",
                "snippet": "the rent is due",
                "url": "http://mattbox.local/cloud/files/Documents/Taxes%202026",
            })
        );
        assert_eq!(hits[1]["place"], "LosOS Cloud");
        assert_eq!(hits[1]["url"], "http://mattbox.local/cloud/files");
    }

    #[test]
    fn a_git_hit_opens_the_line() {
        let answer = json!({
            "available": true,
            "hits": [
                {"owner": "notshared", "repo": "notes", "branch": "main",
                 "path": "plans/rent.md", "line": 12, "snippet": ["pay ", "rent"]},
                {"owner": "x\"><script>", "repo": "notes", "branch": "main",
                 "path": "a", "line": 1, "snippet": []},
                {"owner": "notshared", "repo": "notes", "branch": "main",
                 "path": "a", "line": 0, "snippet": []},
            ],
        });
        let hits = text_hits("http://mattbox.local", "git", &answer);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], "rent.md");
        assert_eq!(hits[0]["place"], "notshared/notes");
        assert_eq!(
            hits[0]["url"],
            "http://mattbox.local/git/notshared/notes/src/branch/main/plans/rent.md#L12"
        );
    }

    #[test]
    fn an_unavailable_search_has_no_hits() {
        let off = json!({"available": false, "reason": "noGit"});
        assert!(text_hits("http://mattbox.local", "git", &off).is_empty());
        assert!(text_hits("http://mattbox.local", "cloud", &json!("not json")).is_empty());
    }
}
