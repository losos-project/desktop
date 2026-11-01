//! Web Authentication: a site asks for a security key, to register one or
//! to sign in with it. WebKit's own WebAuthn is built only for Apple's
//! ports, so on Linux `navigator.credentials` does not exist and a site
//! that needs a key (Proton, GitHub, a bank) has no way to ask for one.
//! Danube answers instead (docs/danube.md, "Security keys"):
//!
//! - webauthn.js, in the page's own world of the top frame, defines
//!   `navigator.credentials.create`/`get` and `PublicKeyCredential` as the
//!   specification has them, and turns each call into a request.
//! - webauthn-bridge.js, in a world of Danube's own in that same frame,
//!   passes the request to [`crate::engine`] and the answer back. A frame
//!   inside the page never has the bridge, so an embedded site cannot ask
//!   in the top site's name.
//! - This module checks the request against the address WebKit has for the
//!   tab, never against anything the page says, builds the client data the
//!   key signs, and talks CTAP2 (or CTAP1, for an old U2F key) to every
//!   key on USB through Mozilla's authenticator crate, Firefox's own. In a
//!   machine those keys are the host's, which losos-tokens lends it
//!   (docs/tokens.md).
//!
//! The person sees a bar over the page naming the site, touches the key,
//! and types its PIN in the bar when the key asks for one.
//!
//! The browser's sandbox has no /dev of the host's, where a key's device
//! node appears when it is plugged in. The launcher, which runs outside it,
//! asks the keys instead ([`serve`]): the browser hands it each request
//! over a socket bound into the sandbox, with the PIN and Cancel the person
//! gives, and the launcher checks the request again before a key hears
//! of it.

use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use authenticator::authenticatorservice::{AuthenticatorService, RegisterArgs, SignArgs};
use authenticator::crypto::COSEAlgorithm;
use authenticator::ctap2::server::{
    AuthenticationExtensionsClientInputs, PublicKeyCredentialDescriptor,
    PublicKeyCredentialParameters, PublicKeyCredentialUserEntity, RelyingParty,
    ResidentKeyRequirement, Transport, UserVerificationRequirement,
};
use authenticator::errors::AuthenticatorError;
use authenticator::statecallback::StateCallback;
use authenticator::{Pin, StatusPinUv, StatusUpdate};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use crossbeam_channel::{select, Receiver};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

/// The page's half, in its own world of the top frame.
pub const PAGE_SCRIPT: &str = include_str!("webauthn.js");
/// The bridge, in [`WORLD`].
pub const BRIDGE_SCRIPT: &str = include_str!("webauthn-bridge.js");
/// Danube's world for the bridge, which the page cannot reach.
pub const WORLD: &str = "danube-webauthn";
/// The script message handler the bridge posts to.
pub const HANDLER: &str = "danubeWebAuthn";
/// Names the launcher's socket inside the sandbox (danube-sandbox's
/// browser plan sets it).
pub const KEYS_ENV: &str = "DANUBE_KEYS";

/// How long a key is waited for when the site does not say, and the
/// bounds on what it may say: the specification's recommendation, and room
/// to find a key in a bag.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const MIN_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// Registering a new key, or signing in with one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Create,
    Get,
}

impl Op {
    /// The client data's `type`.
    fn client_data_type(self) -> &'static str {
        match self {
            Op::Create => "webauthn.create",
            Op::Get => "webauthn.get",
        }
    }

    /// What the bar says the site wants.
    pub fn describe(self) -> &'static str {
        match self {
            Op::Create => "register your security key",
            Op::Get => "sign in with your security key",
        }
    }
}

/// Why a request ends without a credential, as the `DOMException` name the
/// page's promise is rejected with and a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub name: &'static str,
    pub message: String,
}

impl Refusal {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }

    /// The bridge's form of it: name and message, which the page turns
    /// into a `DOMException`.
    pub fn to_json(&self) -> String {
        json!({ "error": { "name": self.name, "message": self.message } }).to_string()
    }

    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::new("NotAllowedError", message)
    }

    fn security(message: impl Into<String>) -> Self {
        Self::new("SecurityError", message)
    }

    fn not_supported(message: impl Into<String>) -> Self {
        Self::new("NotSupportedError", message)
    }

    /// The person pressed Cancel, or the page aborted.
    pub fn cancelled() -> Self {
        Self::not_allowed("The request was cancelled.")
    }
}

/// A request the engine has checked: everything the key is asked, with
/// the client data it signs.
#[derive(Debug)]
pub struct Request {
    pub op: Op,
    /// The origin, from the tab's address.
    pub origin: String,
    pub rp_id: String,
    pub client_data_json: String,
    pub timeout: Duration,
    args: Args,
}

#[derive(Debug)]
enum Args {
    Create {
        rp_name: Option<String>,
        user: PublicKeyCredentialUserEntity,
        algorithms: Vec<COSEAlgorithm>,
        exclude: Vec<Vec<u8>>,
        resident_key: ResidentKeyRequirement,
        user_verification: UserVerificationRequirement,
        /// Keep the key's attestation, which says what make of key it is.
        attest: bool,
        cred_props: bool,
    },
    Get {
        allow: Vec<Vec<u8>>,
        user_verification: UserVerificationRequirement,
        app_id: Option<String>,
    },
}

/// The origin of `uri`, `https://host[:port]`, if a page there may use
/// WebAuthn: https, or http on the machine itself, as the specification's
/// "potentially trustworthy" origins go.
pub fn origin_of(uri: &str) -> Option<(String, String)> {
    let (scheme, rest) = uri.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // No user:password@ in front of the host.
    let authority = authority.rsplit('@').next()?;
    let host = match authority.strip_prefix('[') {
        // An IPv6 literal: never a valid RP ID, and no use to anyone here.
        Some(_) => return None,
        None => authority.split(':').next()?,
    }
    .to_ascii_lowercase();
    let trustworthy = match scheme {
        "https" => true,
        "http" => host == "localhost" || host.ends_with(".localhost"),
        _ => false,
    };
    if !trustworthy || host.is_empty() {
        return None;
    }
    Some((
        format!("{scheme}://{}", authority.to_ascii_lowercase()),
        host,
    ))
}

/// Whether a page on `host` may use `rp_id`: the host itself or a domain it
/// is under, as long as that is not a public suffix such as `co.uk`, which
/// would let every site under it share credentials.
pub fn rp_id_allowed(rp_id: &str, host: &str) -> bool {
    let rp_id = rp_id.to_ascii_lowercase();
    if rp_id.is_empty() || rp_id.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let under = host == rp_id || host.ends_with(&format!(".{rp_id}"));
    if !under {
        return false;
    }
    if rp_id == "localhost" || rp_id.ends_with(".localhost") {
        return true;
    }
    // psl names the public suffix a domain ends in; a domain that is its
    // own suffix is one nobody registers.
    match psl::suffix(rp_id.as_bytes()) {
        Some(suffix) => suffix.as_bytes() != rp_id.as_bytes(),
        None => false,
    }
}

/// The client data the key signs a hash of, as the specification
/// serialises it: its members in this order, with `crossOrigin` false
/// because only a top frame can ask.
pub fn client_data_json(op: Op, challenge: &[u8], origin: &str) -> String {
    format!(
        "{{\"type\":{},\"challenge\":{},\"origin\":{},\"crossOrigin\":false}}",
        Value::from(op.client_data_type()),
        Value::from(URL_SAFE_NO_PAD.encode(challenge)),
        Value::from(origin),
    )
}

fn b64(value: Option<&Value>, what: &str) -> Result<Vec<u8>, Refusal> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::new("TypeError", format!("{what} is missing")))?;
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| Refusal::new("TypeError", format!("{what} is not base64url")))
}

fn credential_ids(list: Option<&Value>) -> Result<Vec<Vec<u8>>, Refusal> {
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| {
            c.get("type")
                .and_then(Value::as_str)
                .unwrap_or("public-key")
                == "public-key"
        })
        .map(|c| b64(c.get("id"), "a credential's id"))
        .collect()
}

fn user_verification(value: Option<&Value>) -> UserVerificationRequirement {
    match value.and_then(Value::as_str) {
        Some("required") => UserVerificationRequirement::Required,
        Some("discouraged") => UserVerificationRequirement::Discouraged,
        _ => UserVerificationRequirement::Preferred,
    }
}

fn timeout(options: &Map<String, Value>) -> Duration {
    options
        .get("timeout")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
        .clamp(MIN_TIMEOUT, MAX_TIMEOUT)
}

/// Checks the bridge's `message` from the tab whose address is `uri`.
/// `message` is `{"op": "create"|"get", "origin": ..., "publicKey": {...}}`
/// with every buffer in base64url; the origin it names must be the tab's.
pub fn parse(message: &str, uri: &str) -> Result<Request, Refusal> {
    let message: Value = serde_json::from_str(message)
        .map_err(|_| Refusal::new("TypeError", "The request is not JSON."))?;
    let op = match message.get("op").and_then(Value::as_str) {
        Some("create") => Op::Create,
        Some("get") => Op::Get,
        _ => {
            return Err(Refusal::not_supported(
                "Only public key credentials are supported.",
            ))
        }
    };
    let (origin, host) = origin_of(uri)
        .ok_or_else(|| Refusal::security("Security keys need a page served over HTTPS."))?;
    // The bridge says which page it ran in. The tab may have moved on since,
    // and then the request is not this page's to make.
    if message.get("origin").and_then(Value::as_str) != Some(origin.as_str()) {
        return Err(Refusal::not_allowed("The page changed while it asked."));
    }
    let options = message
        .get("publicKey")
        .and_then(Value::as_object)
        .ok_or_else(|| Refusal::not_supported("Only public key credentials are supported."))?;
    let challenge = b64(options.get("challenge"), "The challenge")?;
    let timeout = timeout(options);

    let (rp_id, args) = match op {
        Op::Create => {
            let rp = options.get("rp").and_then(Value::as_object);
            let rp_id = rp
                .and_then(|rp| rp.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| host.clone());
            let user = options
                .get("user")
                .and_then(Value::as_object)
                .ok_or_else(|| Refusal::new("TypeError", "The user is missing."))?;
            let user = PublicKeyCredentialUserEntity {
                id: b64(user.get("id"), "The user's id")?,
                name: user.get("name").and_then(Value::as_str).map(str::to_owned),
                display_name: user
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            if user.id.is_empty() || user.id.len() > 64 {
                return Err(Refusal::new(
                    "TypeError",
                    "The user's id must be 1 to 64 bytes.",
                ));
            }
            // An empty list means the two the specification names.
            let mut algorithms: Vec<COSEAlgorithm> = options
                .get("pubKeyCredParams")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("public-key"))
                .filter_map(|p| p.get("alg").and_then(Value::as_i64))
                .filter_map(|alg| COSEAlgorithm::try_from(alg).ok())
                .collect();
            if algorithms.is_empty() {
                if options
                    .get("pubKeyCredParams")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                {
                    return Err(Refusal::not_supported(
                        "None of the algorithms is supported.",
                    ));
                }
                algorithms = vec![COSEAlgorithm::ES256, COSEAlgorithm::RS256];
            }
            let selection = options
                .get("authenticatorSelection")
                .and_then(Value::as_object);
            let resident_key = match selection
                .and_then(|s| s.get("residentKey"))
                .and_then(Value::as_str)
            {
                Some("required") => ResidentKeyRequirement::Required,
                Some("preferred") => ResidentKeyRequirement::Preferred,
                Some(_) => ResidentKeyRequirement::Discouraged,
                // Level 1's boolean, which the specification still honours.
                None if selection
                    .and_then(|s| s.get("requireResidentKey"))
                    .and_then(Value::as_bool)
                    == Some(true) =>
                {
                    ResidentKeyRequirement::Required
                }
                None => ResidentKeyRequirement::Discouraged,
            };
            if selection
                .and_then(|s| s.get("authenticatorAttachment"))
                .and_then(Value::as_str)
                == Some("platform")
            {
                // There is no key built into the computer to use.
                return Err(Refusal::not_allowed(
                    "This computer has no built-in authenticator.",
                ));
            }
            let args = Args::Create {
                rp_name: rp
                    .and_then(|rp| rp.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                user,
                algorithms,
                exclude: credential_ids(options.get("excludeCredentials"))?,
                resident_key,
                user_verification: user_verification(
                    selection.and_then(|s| s.get("userVerification")),
                ),
                attest: matches!(
                    options.get("attestation").and_then(Value::as_str),
                    Some("direct" | "enterprise")
                ),
                cred_props: options
                    .get("extensions")
                    .and_then(|e| e.get("credProps"))
                    .and_then(Value::as_bool)
                    == Some(true),
            };
            (rp_id, args)
        }
        Op::Get => {
            let rp_id = options
                .get("rpId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| host.clone());
            // FIDO U2F's AppID, for a key registered before WebAuthn: only
            // one under the same site, which is as far as the FIDO facet
            // rules ever let a site go.
            let app_id = options
                .get("extensions")
                .and_then(|e| e.get("appid"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(app_id) = &app_id {
                let allowed = app_id.starts_with("https://")
                    && origin_of(app_id).is_some_and(|(_, app_host)| same_site(&app_host, &host));
                if !allowed {
                    return Err(Refusal::security("The AppID is not this site's."));
                }
            }
            let args = Args::Get {
                allow: credential_ids(options.get("allowCredentials"))?,
                user_verification: user_verification(options.get("userVerification")),
                app_id,
            };
            (rp_id, args)
        }
    };
    if !rp_id_allowed(&rp_id, &host) {
        return Err(Refusal::security(format!(
            "{rp_id} is not a domain this page may use."
        )));
    }
    Ok(Request {
        op,
        client_data_json: client_data_json(op, &challenge, &origin),
        origin,
        rp_id: rp_id.to_ascii_lowercase(),
        timeout,
        args,
    })
}

/// Whether two hosts are under the same registrable domain.
fn same_site(a: &str, b: &str) -> bool {
    match (psl::domain(a.as_bytes()), psl::domain(b.as_bytes())) {
        (Some(a), Some(b)) => a.as_bytes() == b.as_bytes(),
        _ => false,
    }
}

/// How the request is going, for the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a touch. With several keys in, the one touched is used.
    Touch,
    /// The key wants its PIN; `retries` when it said how many are left,
    /// `wrong` after a wrong one.
    Pin { retries: Option<u8>, wrong: bool },
}

impl Status {
    /// As the launcher sends it to the browser.
    fn to_json(&self) -> Value {
        match self {
            Status::Touch => json!({ "status": "touch" }),
            Status::Pin { retries, wrong } => {
                json!({ "status": "pin", "retries": retries, "wrong": wrong })
            }
        }
    }

    fn from_json(said: &Value) -> Option<Self> {
        match said.get("status")?.as_str()? {
            "touch" => Some(Status::Touch),
            "pin" => Some(Status::Pin {
                retries: said
                    .get("retries")
                    .and_then(Value::as_u64)
                    .and_then(|n| u8::try_from(n).ok()),
                wrong: said.get("wrong").and_then(Value::as_bool) == Some(true),
            }),
            _ => None,
        }
    }
}

/// Answers the bridge's `message` from the tab at `uri`, which the engine
/// has checked as `request`: through the launcher when the browser runs in
/// its sandbox, or here when it runs without one (`--no-sandbox`).
pub fn answer(
    message: &str,
    uri: &str,
    request: Request,
    status: impl Fn(Status),
    pins: &Receiver<String>,
    cancel: &Receiver<()>,
) -> String {
    let Some(socket) = std::env::var_os(KEYS_ENV) else {
        return run(request, status, pins, cancel);
    };
    through(Path::new(&socket), message, uri, status, pins, cancel).unwrap_or_else(|error| {
        warn!(%error, "cannot reach the launcher to ask the security keys");
        Refusal::not_allowed("Danube cannot reach the security keys.").to_json()
    })
}

/// The browser's half of a request the launcher asks the keys for: the
/// request goes as one line, the PIN and Cancel the person gives follow
/// it, and the launcher's lines say how it is going until its answer.
fn through(
    socket: &Path,
    message: &str,
    uri: &str,
    status: impl Fn(Status),
    pins: &Receiver<String>,
    cancel: &Receiver<()>,
) -> io::Result<String> {
    let stream = UnixStream::connect(socket)?;
    let writer = stream.try_clone()?;
    writeln!(&writer, "{}", json!({ "message": message, "uri": uri }))?;
    let (heard, lines) = crossbeam_channel::bounded::<String>(8);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if heard.send(line).is_err() {
                    break;
                }
            }
        });
        // A side that has gone is waited on no more.
        let (mut pins, mut cancel) = (pins.clone(), cancel.clone());
        let result = loop {
            select! {
                recv(lines) -> line => {
                    let Ok(line) = line else {
                        break Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the launcher hung up"));
                    };
                    let Ok(said) = serde_json::from_str::<Value>(&line) else { continue };
                    if let Some(answer) = said.get("answer").and_then(Value::as_str) {
                        break Ok(answer.to_owned());
                    }
                    if let Some(now) = Status::from_json(&said) {
                        status(now);
                    }
                }
                recv(pins) -> pin => match pin {
                    Ok(pin) => {
                        let _ = writeln!(&writer, "{}", json!({ "pin": pin }));
                    }
                    Err(_) => pins = crossbeam_channel::never(),
                },
                recv(cancel) -> asked => {
                    let _ = writeln!(&writer, "{}", json!({ "cancel": true }));
                    if asked.is_err() {
                        cancel = crossbeam_channel::never();
                    }
                }
            }
        };
        // Ends the reader, and tells the launcher nobody is listening.
        let _ = writer.shutdown(Shutdown::Both);
        result
    })
}

/// The launcher's half, outside the sandbox: each connection is one
/// request from the browser, asked of the keys on a thread of its own.
/// The request is checked again here, as if the browser had not.
pub fn serve(listener: UnixListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let spawned = std::thread::Builder::new()
            .name("security-key".into())
            .spawn(move || {
                if let Err(error) = serve_one(stream) {
                    debug!(%error, "a security key request ended early");
                }
            });
        if let Err(error) = spawned {
            warn!(%error, "cannot start asking the security keys");
        }
    }
}

fn serve_one(stream: UnixStream) -> io::Result<()> {
    let out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let asked: Value = serde_json::from_str(&first).map_err(io::Error::other)?;
    let message = asked
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let uri = asked.get("uri").and_then(Value::as_str).unwrap_or_default();
    let request = match parse(message, uri) {
        Ok(request) => request,
        Err(refusal) => {
            writeln!(&out, "{}", json!({ "answer": refusal.to_json() }))?;
            return Ok(());
        }
    };
    let (typed, pins) = crossbeam_channel::bounded(1);
    let (cancelled, cancel) = crossbeam_channel::bounded(1);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(said) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(pin) = said.get("pin").and_then(Value::as_str) {
                    let _ = typed.try_send(pin.to_owned());
                } else if said.get("cancel").is_some() {
                    let _ = cancelled.try_send(());
                }
            }
            // The browser hung up: nobody is left to touch the key for.
            let _ = cancelled.try_send(());
        });
        let answer = run(
            request,
            |now| {
                let _ = writeln!(&out, "{}", now.to_json());
            },
            &pins,
            &cancel,
        );
        let written = writeln!(&out, "{}", json!({ "answer": answer }));
        // Ends the reader, whose thread the scope waits for.
        let _ = out.shutdown(Shutdown::Both);
        written
    })
}

/// Asks the keys, and gives the bridge's answer: the credential's JSON, or
/// the refusal's. `status` hears how it is going, `pins` brings what the
/// person typed, and anything on `cancel` ends it.
fn run(
    request: Request,
    status: impl Fn(Status),
    pins: &Receiver<String>,
    cancel: &Receiver<()>,
) -> String {
    match ask(request, status, pins, cancel) {
        Ok(json) => json,
        Err(refusal) => refusal.to_json(),
    }
}

fn ask(
    request: Request,
    status: impl Fn(Status),
    pins: &Receiver<String>,
    cancel: &Receiver<()>,
) -> Result<String, Refusal> {
    let mut service = AuthenticatorService::new()
        .map_err(|e| Refusal::not_allowed(format!("Cannot look for security keys: {e:?}")))?;
    service.add_u2f_usb_hid_platform_transports();

    let hash: [u8; 32] = Sha256::digest(request.client_data_json.as_bytes()).into();
    let timeout_ms = request.timeout.as_millis() as u64;
    let (updates, updated) = mpsc::channel::<StatusUpdate>();
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let origin = request.origin.clone();

    match &request.args {
        Args::Create {
            rp_name,
            user,
            algorithms,
            exclude,
            resident_key,
            user_verification,
            cred_props,
            ..
        } => {
            let args = RegisterArgs {
                client_data_hash: hash,
                relying_party: RelyingParty {
                    id: request.rp_id.clone(),
                    name: rp_name.clone(),
                },
                origin,
                user: user.clone(),
                pub_cred_params: algorithms
                    .iter()
                    .map(|&alg| PublicKeyCredentialParameters { alg })
                    .collect(),
                exclude_list: descriptors(exclude),
                user_verification_req: *user_verification,
                resident_key_req: *resident_key,
                extensions: AuthenticationExtensionsClientInputs {
                    cred_props: Some(*cred_props),
                    ..Default::default()
                },
                pin: None,
                use_ctap1_fallback: false,
            };
            let callback = StateCallback::new(Box::new(move |result| {
                let _ = done_tx.send(Done::Registered(result));
            }));
            service
                .register(timeout_ms, args, updates, callback)
                .map_err(|e| Refusal::not_allowed(format!("Cannot ask the security key: {e:?}")))?;
        }
        Args::Get {
            allow,
            user_verification,
            app_id,
        } => {
            let args = SignArgs {
                client_data_hash: hash,
                origin,
                relying_party_id: request.rp_id.clone(),
                allow_list: descriptors(allow),
                user_verification_req: *user_verification,
                user_presence_req: true,
                extensions: AuthenticationExtensionsClientInputs {
                    app_id: app_id.clone(),
                    ..Default::default()
                },
                pin: None,
                use_ctap1_fallback: false,
            };
            let callback = StateCallback::new(Box::new(move |result| {
                let _ = done_tx.send(Done::Signed(result));
            }));
            service
                .sign(timeout_ms, args, updates, callback)
                .map_err(|e| Refusal::not_allowed(format!("Cannot ask the security key: {e:?}")))?;
        }
    }
    info!(op = ?request.op, rp = %request.rp_id, "asking the security keys");
    status(Status::Touch);

    // The crate reports on a std channel; a thread of its own carries each
    // report over to one select can wait on with the rest.
    let (relay, reports) = crossbeam_channel::bounded::<StatusUpdate>(8);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for update in updated {
                if relay.send(update).is_err() {
                    return;
                }
            }
        });
        // The PIN the key asked for goes back on the sender it came with.
        let mut waiting: Option<mpsc::Sender<Pin>> = None;
        let result = loop {
            select! {
                recv(done) -> result => break result.ok(),
                recv(reports) -> update => match update {
                    Ok(StatusUpdate::PinUvError(StatusPinUv::PinRequired(sender))) => {
                        status(Status::Pin { retries: None, wrong: false });
                        waiting = Some(sender);
                    }
                    Ok(StatusUpdate::PinUvError(StatusPinUv::InvalidPin(sender, retries))) => {
                        status(Status::Pin { retries, wrong: true });
                        waiting = Some(sender);
                    }
                    Ok(StatusUpdate::PinUvError(other)) => {
                        // Blocked, not set, too short: the request ends
                        // with the key's error, which says so.
                        debug!(?other, "PIN trouble");
                    }
                    Ok(StatusUpdate::PresenceRequired | StatusUpdate::SelectDeviceNotice) => {
                        status(Status::Touch);
                    }
                    Ok(other) => debug!(?other, "status"),
                    // The crate is done reporting; its answer is on its way.
                    Err(_) => {}
                },
                recv(pins) -> pin => match pin {
                    Ok(pin) => {
                        if let Some(sender) = waiting.take() {
                            status(Status::Touch);
                            let _ = sender.send(Pin::new(&pin));
                        }
                    }
                    // Nobody is left to type a PIN or press Cancel.
                    Err(_) => {
                        let _ = service.cancel();
                        break None;
                    }
                },
                recv(cancel) -> _ => {
                    let _ = service.cancel();
                    break None;
                }
            }
        };
        // Dropping the service ends its reports, and with them the relay.
        drop(service);
        match result {
            Some(Done::Registered(Ok(made))) => Ok(registered(&request, made)),
            Some(Done::Signed(Ok(signed))) => Ok(signed_in(&request, signed)),
            Some(Done::Registered(Err(e)) | Done::Signed(Err(e))) => Err(refusal(e)),
            None => Err(Refusal::cancelled()),
        }
    })
}

enum Done {
    Registered(authenticator::Result<authenticator::RegisterResult>),
    Signed(authenticator::Result<authenticator::SignResult>),
}

fn descriptors(ids: &[Vec<u8>]) -> Vec<PublicKeyCredentialDescriptor> {
    ids.iter()
        .map(|id| PublicKeyCredentialDescriptor {
            id: id.clone(),
            transports: vec![Transport::USB],
        })
        .collect()
}

/// What a page is told when the keys said no. The specification hides
/// almost every reason behind NotAllowedError, so a site cannot probe which
/// credentials a key holds; only one already registered on the key gets
/// its own name, which is what `excludeCredentials` asks to find out.
fn refusal(error: AuthenticatorError) -> Refusal {
    warn!(?error, "the security key said no");
    match error {
        AuthenticatorError::CredentialExcluded => Refusal::new(
            "InvalidStateError",
            "This security key is already registered.",
        ),
        AuthenticatorError::CancelledByUser => Refusal::cancelled(),
        _ => Refusal::not_allowed("The security key did not complete the request."),
    }
}

fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The new credential, for webauthn.js to build a `PublicKeyCredential`
/// from.
fn registered(request: &Request, made: authenticator::RegisterResult) -> String {
    let Args::Create {
        attest, cred_props, ..
    } = &request.args
    else {
        unreachable!("a registration answers a create request");
    };
    let mut att_obj = made.att_obj;
    // What make of key it is stays with the person unless the site asked:
    // "none" is the default, and the specification lets a browser drop it.
    if !attest {
        att_obj.anonymize();
    }
    let auth_data = att_obj.auth_data.to_vec();
    let credential = att_obj.auth_data.credential_data.as_ref();
    let id = credential
        .map(|c| encode(&c.credential_id))
        .unwrap_or_default();
    let (public_key, algorithm) = credential
        .map(|c| {
            (
                c.credential_public_key.der_spki().ok().map(|k| encode(&k)),
                c.credential_public_key.alg as i64,
            )
        })
        .unwrap_or((None, 0));
    let attestation_object = attestation_cbor(&att_obj);
    let mut extensions = Map::new();
    if *cred_props {
        if let Some(props) = made.extensions.cred_props {
            extensions.insert("credProps".into(), json!({ "rk": props.rk }));
        }
    }
    json!({
        "id": id,
        "type": "public-key",
        "authenticatorAttachment": "cross-platform",
        "response": {
            "clientDataJSON": encode(request.client_data_json.as_bytes()),
            "attestationObject": attestation_object,
            "authenticatorData": encode(&auth_data),
            "transports": ["usb"],
            "publicKey": public_key,
            "publicKeyAlgorithm": algorithm,
        },
        "clientExtensionResults": extensions,
    })
    .to_string()
}

/// The attestation object as CBOR, which the crate's Serialize writes in
/// WebAuthn's layout: fmt, attStmt and authData.
fn attestation_cbor(att_obj: &authenticator::ctap2::attestation::AttestationObject) -> String {
    match serde_cbor::to_vec(att_obj) {
        Ok(bytes) => encode(&bytes),
        Err(e) => {
            warn!(error = ?e, "cannot encode the attestation object");
            String::new()
        }
    }
}

/// The assertion, for webauthn.js to build a `PublicKeyCredential` from.
fn signed_in(request: &Request, signed: authenticator::SignResult) -> String {
    let assertion = signed.assertion;
    // A key asked with an allow list of one may leave out which credential
    // it used; then it is that one.
    let id = assertion
        .credentials
        .as_ref()
        .map(|c| c.id.clone())
        .or_else(|| match &request.args {
            Args::Get { allow, .. } if allow.len() == 1 => Some(allow[0].clone()),
            _ => None,
        })
        .unwrap_or_default();
    let mut extensions = Map::new();
    if let Some(used) = signed.extensions.app_id {
        extensions.insert("appid".into(), Value::from(used));
    }
    json!({
        "id": encode(&id),
        "type": "public-key",
        "authenticatorAttachment": "cross-platform",
        "response": {
            "clientDataJSON": encode(request.client_data_json.as_bytes()),
            "authenticatorData": encode(&assertion.auth_data.to_vec()),
            "signature": encode(&assertion.signature),
            "userHandle": assertion.user.as_ref().map(|u| encode(&u.id)),
        },
        "clientExtensionResults": extensions,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_cross_the_socket() {
        for status in [
            Status::Touch,
            Status::Pin {
                retries: Some(3),
                wrong: true,
            },
            Status::Pin {
                retries: None,
                wrong: false,
            },
        ] {
            assert_eq!(Status::from_json(&status.to_json()), Some(status));
        }
    }

    #[test]
    fn the_launcher_checks_requests_again() {
        let dir = std::env::temp_dir().join(format!("danube-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("keys");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || serve(listener));
        let (_typed, pins) = crossbeam_channel::bounded(1);
        let (_cancelled, cancel) = crossbeam_channel::bounded(1);
        let message = json!({
            "op": "get",
            "origin": "http://example.com",
            "publicKey": { "challenge": "AQID" },
        })
        .to_string();
        // A page the browser should never have passed on is refused there
        // too, before any key hears of it.
        let answer = through(
            &socket,
            &message,
            "http://example.com/",
            |_| {},
            &pins,
            &cancel,
        );
        assert_eq!(
            answer.unwrap(),
            Refusal::security("Security keys need a page served over HTTPS.").to_json()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn b(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn origins() {
        assert_eq!(
            origin_of("https://account.proton.me/login?x=1"),
            Some((
                "https://account.proton.me".into(),
                "account.proton.me".into()
            ))
        );
        assert_eq!(
            origin_of("https://User@Example.com:8443/"),
            Some(("https://example.com:8443".into(), "example.com".into()))
        );
        assert_eq!(
            origin_of("http://localhost:8080/"),
            Some(("http://localhost:8080".into(), "localhost".into()))
        );
        assert_eq!(origin_of("http://example.com/"), None);
        assert_eq!(origin_of("file:///home/user/x.html"), None);
        assert_eq!(origin_of("https://[::1]/"), None);
    }

    #[test]
    fn rp_ids() {
        assert!(rp_id_allowed("proton.me", "account.proton.me"));
        assert!(rp_id_allowed("account.proton.me", "account.proton.me"));
        assert!(rp_id_allowed("slovensko.sk", "www.slovensko.sk"));
        // Another site, or a suffix everyone shares.
        assert!(!rp_id_allowed("google.com", "account.proton.me"));
        assert!(!rp_id_allowed("me", "account.proton.me"));
        assert!(!rp_id_allowed("co.uk", "bank.co.uk"));
        assert!(!rp_id_allowed("github.io", "someone.github.io"));
        assert!(!rp_id_allowed("ton.me", "account.proton.me"));
        assert!(rp_id_allowed("localhost", "localhost"));
        assert!(!rp_id_allowed("127.0.0.1", "127.0.0.1"));
    }

    #[test]
    fn client_data_is_serialised_in_order() {
        assert_eq!(
            client_data_json(Op::Get, &[1, 2, 3], "https://a.example"),
            r#"{"type":"webauthn.get","challenge":"AQID","origin":"https://a.example","crossOrigin":false}"#
        );
    }

    fn create(origin: &str, extra: Value) -> String {
        let mut options = json!({
            "rp": { "name": "Proton" },
            "user": { "id": b(b"user"), "name": "matus", "displayName": "Matus" },
            "challenge": b(&[7; 32]),
            "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }, { "type": "public-key", "alg": -257 }],
        });
        for (k, v) in extra.as_object().unwrap() {
            options[k] = v.clone();
        }
        json!({ "op": "create", "origin": origin, "publicKey": options }).to_string()
    }

    #[test]
    fn a_registration_is_checked_against_the_tab() {
        let uri = "https://account.proton.me/u/0/mail/account-password";
        let request = parse(&create("https://account.proton.me", json!({})), uri).unwrap();
        assert_eq!(request.op, Op::Create);
        assert_eq!(request.rp_id, "account.proton.me");
        assert_eq!(request.timeout, DEFAULT_TIMEOUT);
        assert!(request
            .client_data_json
            .contains(r#""origin":"https://account.proton.me""#));

        let request = parse(
            &create(
                "https://account.proton.me",
                json!({ "rp": { "id": "proton.me" }, "timeout": 1 }),
            ),
            uri,
        )
        .unwrap();
        assert_eq!(request.rp_id, "proton.me");
        assert_eq!(request.timeout, MIN_TIMEOUT);

        // The page says it is another site, or names one.
        assert_eq!(
            parse(&create("https://evil.example", json!({})), uri)
                .unwrap_err()
                .name,
            "NotAllowedError"
        );
        assert_eq!(
            parse(
                &create(
                    "https://account.proton.me",
                    json!({ "rp": { "id": "google.com" } })
                ),
                uri
            )
            .unwrap_err()
            .name,
            "SecurityError"
        );
        // Plain http.
        assert_eq!(
            parse(
                &create("http://account.proton.me", json!({})),
                "http://account.proton.me/"
            )
            .unwrap_err()
            .name,
            "SecurityError"
        );
        // Only algorithms nobody supports.
        assert_eq!(
            parse(
                &create(
                    "https://account.proton.me",
                    json!({ "pubKeyCredParams": [{ "type": "public-key", "alg": 12345 }] })
                ),
                uri
            )
            .unwrap_err()
            .name,
            "NotSupportedError"
        );
    }

    #[test]
    fn a_sign_in_takes_its_allow_list() {
        let message = json!({
            "op": "get",
            "origin": "https://account.proton.me",
            "publicKey": {
                "challenge": b(&[1; 32]),
                "rpId": "proton.me",
                "allowCredentials": [{ "type": "public-key", "id": b(&[9, 9]) }],
                "userVerification": "discouraged",
            },
        })
        .to_string();
        let request = parse(&message, "https://account.proton.me/login").unwrap();
        let Args::Get {
            allow,
            user_verification,
            app_id,
        } = &request.args
        else {
            panic!()
        };
        assert_eq!(allow, &[vec![9, 9]]);
        assert_eq!(*user_verification, UserVerificationRequirement::Discouraged);
        assert_eq!(app_id, &None);
        assert!(request
            .client_data_json
            .starts_with(r#"{"type":"webauthn.get""#));
    }

    #[test]
    fn refusals_are_json_the_page_can_throw() {
        let refusal: Value = serde_json::from_str(&Refusal::cancelled().to_json()).unwrap();
        assert_eq!(refusal["error"]["name"], "NotAllowedError");
    }
}
