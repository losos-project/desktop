//! Signing the backups in through LosOS Login, the box's own sign-in.
//!
//! The device code flow, as `losos agents install` signs its companion in:
//! the box gives a link and a short code, the person opens the link in a
//! browser, signs in with their password and their second factor when they
//! have one, and approves "LosOS Desktop backups" on LosOS Login's consent
//! page. Nothing typed here, so an account with a passkey or a code from an
//! app backs up like any other.
//!
//! The token LosOS Login then hands out is good for ten minutes and for one
//! request: `POST /api/devices/backups`, where lososd makes this computer a
//! LosOS Cloud app password and a LosOS Git token of the person's own. Those
//! are what the runs use, as after a sign-in with a password; the LosOS
//! Login token is dropped.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::Error;

/// LosOS Login's name for the backups (`losos_access::BACKUPS_CLIENT_ID`).
pub const CLIENT_ID: &str = "losos-backups";

const SCOPE: &str = "openid profile";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// What the person is asked to do in their browser.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DeviceCode {
    /// Kept here, never shown: what the polls present.
    pub device_code: String,
    /// The code to type, which the link below already carries.
    pub user_code: String,
    /// LosOS Login's page for codes.
    pub verification_uri: String,
    /// That page with the code in it, when the box gives one.
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// Seconds between polls.
    #[serde(default = "five")]
    pub interval: u64,
    /// Seconds the code is good for.
    #[serde(default = "ten_minutes")]
    pub expires_in: u64,
}

fn five() -> u64 {
    5
}

fn ten_minutes() -> u64 {
    600
}

impl DeviceCode {
    /// The link to open: with the code in it, when the box gives one.
    pub fn link(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }

    /// The one line Settings reads to show the step, as JSON.
    pub fn prompt(&self) -> Value {
        json!({
            "link": self.link(),
            "page": self.verification_uri,
            "code": self.user_code,
            "expiresIn": self.expires_in,
        })
    }
}

/// One app's credential for this computer, as lososd made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    /// The account's own name in the app, which its paths use.
    pub user: String,
    /// The app password or token.
    pub secret: String,
}

/// What `POST /api/devices/backups` answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub cloud: Option<Credential>,
    pub git: Option<Credential>,
    /// The box runs no LosOS Git: repositories go to LosOS Cloud as files.
    pub git_off: bool,
    /// Why an app made nothing, when the other did.
    pub problems: Vec<String>,
}

/// LosOS Login's address on `box_name`.
pub fn base(box_name: &str) -> String {
    format!("https://{box_name}/auth")
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", crate::encode_segment(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The OAuth `error` of a refused request, or its status.
fn oauth_error(e: ureq::Error) -> Result<String, Error> {
    match e {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            Ok(serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("status {code}")))
        }
        e => Err(e.into()),
    }
}

/// Asks LosOS Login for a code.
// ureq's error carries the whole response, so it is large wherever it goes.
#[allow(clippy::result_large_err)]
pub fn start(agent: &ureq::Agent, box_name: &str) -> Result<DeviceCode, Error> {
    agent
        .post(&format!("{}/api/oidc/device-authorization", base(box_name)))
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept", "application/json")
        .send_string(&form(&[("client_id", CLIENT_ID), ("scope", SCOPE)]))
        .map_err(|e| match e {
            ureq::Error::Status(400 | 401 | 404, _) => Error::new(
                "This box's LosOS Login does not know LosOS Desktop's backups yet. Update the box, or sign in to LosOS cloud with a password.",
            ),
            e => e.into(),
        })?
        .into_json()
        .map_err(Error::from)
}

/// Waits for the person to approve `code`, asking every `interval` seconds
/// as LosOS Login said, and returns the access token it then hands out.
#[allow(clippy::result_large_err)]
pub fn finish(agent: &ureq::Agent, box_name: &str, code: &DeviceCode) -> Result<String, Error> {
    let mut interval = code.interval.max(1);
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    loop {
        std::thread::sleep(Duration::from_secs(interval));
        let answer = agent
            .post(&format!("{}/api/oidc/token", base(box_name)))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("Accept", "application/json")
            .send_string(&form(&[
                ("grant_type", DEVICE_GRANT),
                ("device_code", &code.device_code),
                ("client_id", CLIENT_ID),
            ]));
        match answer {
            Ok(reply) => {
                let reply: Value = reply.into_json()?;
                return reply
                    .get("access_token")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new("LosOS Login gave no token."));
            }
            Err(e) => match oauth_error(e)?.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += 5,
                "access_denied" => {
                    return Err(Error::new("The sign-in was turned down in LosOS Login."))
                }
                "expired_token" => {
                    return Err(Error::new("The code ran out. Sign in again."));
                }
                other => {
                    return Err(Error::new(format!(
                        "LosOS Login refused the sign-in ({other})."
                    )))
                }
            },
        }
        if Instant::now() > deadline {
            return Err(Error::new("The code ran out. Sign in again."));
        }
    }
}

/// Trades `token` for this computer's credentials, named for `host`.
#[allow(clippy::result_large_err)]
pub fn credentials(
    agent: &ureq::Agent,
    box_name: &str,
    token: &str,
    host: &str,
) -> Result<Credentials, Error> {
    let reply: Value = agent
        .post(&format!("https://{box_name}/api/devices/backups"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .send_json(json!({ "device": host }))
        .map_err(|e| match e {
            ureq::Error::Status(404 | 405, _) => Error::new(
                "This box cannot make credentials for backups yet. Update the box, or sign in to LosOS cloud with a password.",
            ),
            ureq::Error::Status(_, response) => {
                let body = response.into_string().unwrap_or_default();
                Error::new(
                    serde_json::from_str::<Value>(&body)
                        .ok()
                        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
                        .unwrap_or_else(|| crate::server_message(&body)),
                )
            }
            e => e.into(),
        })?
        .into_json()?;
    Ok(parse(&reply))
}

/// Reads lososd's answer.
pub fn parse(reply: &Value) -> Credentials {
    let one = |app: &str, secret: &str| {
        let doc = reply.get(app)?;
        Some(Credential {
            user: doc.get("user")?.as_str()?.to_owned(),
            secret: doc.get(secret)?.as_str()?.to_owned(),
        })
    };
    Credentials {
        cloud: one("cloud", "appPassword"),
        git: one("git", "token"),
        git_off: reply.get("gitOff").and_then(Value::as_bool) == Some(true),
        problems: reply
            .get("problems")
            .and_then(Value::as_array)
            .map(|p| {
                p.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_device_code() {
        let code: DeviceCode = serde_json::from_str(
            r#"{"device_code":"authelia_dc_x","user_code":"RDQXQRJZ",
                "verification_uri":"https://mattbox.local/auth/consent/openid/device-authorization",
                "verification_uri_complete":"https://mattbox.local/auth/consent/openid/device-authorization?user_code=RDQXQRJZ",
                "expires_in":300}"#,
        )
        .unwrap();
        assert!(code.link().ends_with("?user_code=RDQXQRJZ"));
        assert_eq!(code.interval, 5);
        let prompt = code.prompt();
        assert_eq!(prompt["code"], "RDQXQRJZ");
        assert_eq!(prompt["expiresIn"], 300);
        assert!(prompt.get("device_code").is_none());
    }

    #[test]
    fn reads_both_credentials_or_one() {
        let both = parse(&json!({
            "cloud": {"user": "notshared", "appPassword": "a-b-c", "path": "/remote.php/dav/files/notshared/"},
            "git": {"user": "notshared", "token": "t0k", "path": "/git/"},
        }));
        assert_eq!(both.cloud.unwrap().user, "notshared");
        assert_eq!(both.git.unwrap().secret, "t0k");
        let one = parse(&json!({
            "cloud": {"user": "ada", "appPassword": "x"},
            "git": null,
            "gitOff": true,
        }));
        assert!(one.git.is_none() && one.git_off);
        let refused = parse(&json!({
            "cloud": null,
            "git": {"user": "ada", "token": "t"},
            "problems": ["LosOS Cloud could not be reached."],
        }));
        assert!(refused.cloud.is_none());
        assert_eq!(refused.problems.len(), 1);
    }

    #[test]
    fn encodes_the_form() {
        assert_eq!(
            form(&[("grant_type", DEVICE_GRANT), ("scope", SCOPE)]),
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&scope=openid%20profile"
        );
    }
}
