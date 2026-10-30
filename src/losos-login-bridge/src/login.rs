//! Asking LosOS Login about a name and password.
//!
//! LosOS Login is Authelia (modules/identity.nix in the LosOS repository),
//! and this talks to it the way its own sign-in page does, under `/auth/`:
//!
//! 1. `POST /auth/api/firstfactor` with the name and password, which sets
//!    the session cookie;
//! 2. `GET /auth/api/authz/auth-request` for the box's own address, the
//!    question nginx asks before every admin page: 200 says who the
//!    session is, with their groups in `Remote-Groups`; 401 that their
//!    account wants a second factor first; 403 that a rule keeps them out;
//! 3. for a second factor, the code from their authenticator app,
//!    `POST /auth/api/secondfactor/totp`, and the question again;
//! 4. `POST /auth/api/logout` once done ([`Session`]'s drop).
//!
//! A passkey cannot be asked for through PAM, so an account that only has
//! one is told so instead.

use serde_json::{json, Value};

use crate::join::{Joined, Way};

/// Someone LosOS Login accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub display_name: String,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

/// What LosOS Login said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted(Person),
    /// It said no, with a sentence for the login screen.
    Refused(String),
    /// It could not be asked: no way answered, or LosOS Login itself was
    /// down behind the box's nginx.
    Unreachable(String),
}

/// Asks LosOS Login about `user` and `password`, each way in turn until
/// one answers. `ask_code` asks the person for their authenticator app's
/// code, when their account wants one; `None` means they gave none.
pub fn sign_in(
    joined: &Joined,
    user: &str,
    password: &str,
    ask_code: &mut dyn FnMut(&str) -> Option<String>,
) -> Outcome {
    let mut why = String::from("this computer is joined to no box it can reach");
    for way in joined.ways() {
        match sign_in_by(&way, user, password, ask_code) {
            Outcome::Unreachable(e) => {
                tracing::info!(base = way.base(), error = %e, "LosOS Login did not answer");
                why = e;
            }
            outcome => return outcome,
        }
    }
    Outcome::Unreachable(why)
}

/// One way's answer.
pub fn sign_in_by(
    way: &Way,
    user: &str,
    password: &str,
    ask_code: &mut dyn FnMut(&str) -> Option<String>,
) -> Outcome {
    match Session::open(way, user, password, ask_code) {
        Ok((_session, person)) => Outcome::Accepted(person),
        Err(outcome) => outcome,
    }
}

/// A signed-in session with LosOS Login, logged out when dropped, so the
/// box keeps none for a login that has already happened.
pub struct Session {
    agent: ureq::Agent,
    base: String,
    cookie: String,
}

/// What the question about the session answered.
enum Answer {
    Person(Person),
    SecondFactor,
    Denied,
}

impl Session {
    /// Signs `user` in through `way`: the person, or the [`Outcome`] that
    /// says why not.
    pub fn open(
        way: &Way,
        user: &str,
        password: &str,
        ask_code: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Result<(Session, Person), Outcome> {
        let agent = way.agent().map_err(|e| Outcome::Unreachable(e.message))?;
        let base = way.base();
        let first = agent
            .post(&format!("{base}/auth/api/firstfactor"))
            .send_json(json!({
                "username": user,
                "password": password,
                "keepMeLoggedIn": false,
            }));
        let cookie = match first {
            Ok(response) => cookie_header(response.all("set-cookie")),
            Err(ureq::Error::Status(401 | 403, _)) => {
                return Err(Outcome::Refused(
                    "LosOS Login did not accept that name and password.".into(),
                ))
            }
            Err(e) => return Err(unreachable(e)),
        };
        let session = Session {
            agent,
            base,
            cookie,
        };
        let person = match session.ask()? {
            Answer::Person(p) => p,
            Answer::Denied => return Err(denied(user)),
            Answer::SecondFactor => session.second_factor(user, ask_code)?,
        };
        let person = accepted(person, user)?;
        Ok((session, person))
    }

    /// `GET` one of the box's own pages as the signed-in person.
    pub fn get_json(&self, path: &str) -> Result<Value, crate::Error> {
        Ok(self
            .agent
            .get(&format!("{}{path}", self.base))
            .set("Cookie", &self.cookie)
            .call()?
            .into_json()?)
    }

    /// `POST` JSON to one of the box's own pages as the signed-in person:
    /// its answer, or `None` when the box has no such page (404).
    pub fn post_json(&self, path: &str, body: &Value) -> Result<Option<Value>, crate::Error> {
        match self
            .agent
            .post(&format!("{}{path}", self.base))
            .set("Cookie", &self.cookie)
            .send_json(body)
        {
            Ok(response) => Ok(Some(response.into_json()?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn ask(&self) -> Result<Answer, Outcome> {
        let answer = self
            .agent
            .get(&format!("{}/auth/api/authz/auth-request", self.base))
            .set("Cookie", &self.cookie)
            .set("X-Original-Method", "GET")
            .set("X-Original-URL", &format!("{}/", self.base))
            .call();
        match answer {
            Ok(response) => Ok(person_from_headers(|name| response.header(name))
                .map_or(Answer::Denied, Answer::Person)),
            Err(ureq::Error::Status(401, _)) => Ok(Answer::SecondFactor),
            Err(ureq::Error::Status(403, _)) => Ok(Answer::Denied),
            Err(e) => Err(unreachable(e)),
        }
    }

    fn second_factor(
        &self,
        user: &str,
        ask_code: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Result<Person, Outcome> {
        let info: Value = self
            .agent
            .get(&format!("{}/auth/api/user/info", self.base))
            .set("Cookie", &self.cookie)
            .call()
            .map_err(unreachable)?
            .into_json()
            .map_err(|e| Outcome::Unreachable(e.to_string()))?;
        if second_factor(&info) == SecondFactor::PasskeyOnly {
            return Err(Outcome::Refused(format!(
                "Your LosOS Login account asks for a passkey, which this login screen cannot take. \
                 Add an authenticator app to it at {}/auth/ to log in here.",
                self.base
            )));
        }
        let Some(code) = ask_code(CODE_PROMPT) else {
            return Err(Outcome::Refused(
                "LosOS Login asks for a verification code.".into(),
            ));
        };
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        match self
            .agent
            .post(&format!("{}/auth/api/secondfactor/totp", self.base))
            .set("Cookie", &self.cookie)
            .send_json(json!({ "token": code }))
        {
            Ok(_) => {}
            Err(ureq::Error::Status(401 | 403, _)) => {
                return Err(Outcome::Refused(
                    "LosOS Login did not accept that code.".into(),
                ))
            }
            Err(e) => return Err(unreachable(e)),
        }
        match self.ask()? {
            Answer::Person(p) => Ok(p),
            Answer::Denied | Answer::SecondFactor => Err(denied(user)),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self
            .agent
            .post(&format!("{}/auth/api/logout", self.base))
            .set("Cookie", &self.cookie)
            .send_json(json!({}));
    }
}

/// What the person is asked for their authenticator app's code.
pub const CODE_PROMPT: &str = "Verification code from LosOS Login: ";

fn accepted(person: Person, user: &str) -> Result<Person, Outcome> {
    // The session is the one this call made, so anything else would be
    // LosOS Login renaming people, which it does not do.
    if person.name == user {
        Ok(person)
    } else {
        Err(Outcome::Refused(format!(
            "LosOS Login signed in {} instead of {user}.",
            person.name
        )))
    }
}

fn denied(user: &str) -> Outcome {
    Outcome::Refused(format!("LosOS Login does not let {user} in."))
}

fn unreachable(e: ureq::Error) -> Outcome {
    match e {
        ureq::Error::Status(code, response) => {
            Outcome::Unreachable(format!("{} answered {code}", response.get_url()))
        }
        ureq::Error::Transport(t) => Outcome::Unreachable(t.to_string()),
    }
}

/// The `Cookie` header that sends back every cookie in `set_cookie`, the
/// `Set-Cookie` lines of a response.
pub fn cookie_header(set_cookie: Vec<&str>) -> String {
    set_cookie
        .into_iter()
        .filter_map(|line| line.split(';').next())
        .map(str::trim)
        .filter(|pair| pair.contains('='))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The person an authz answer names: `Remote-User`, `Remote-Name`,
/// `Remote-Email` and the comma-separated `Remote-Groups`.
pub fn person_from_headers<'a>(header: impl Fn(&str) -> Option<&'a str>) -> Option<Person> {
    let get = |name: &str| header(name).map(str::trim).filter(|v| !v.is_empty());
    let name = get("remote-user")?.to_owned();
    Some(Person {
        display_name: get("remote-name").unwrap_or(&name).to_owned(),
        email: get("remote-email").map(str::to_owned),
        groups: get("remote-groups")
            .map(|g| {
                g.split(',')
                    .map(str::trim)
                    .filter(|g| !g.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        name,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum SecondFactor {
    /// A code from an authenticator app, which PAM can ask for.
    Code,
    /// A passkey or nothing PAM can ask for.
    PasskeyOnly,
}

/// Which second factor `/auth/api/user/info` says the person has.
pub fn second_factor(info: &Value) -> SecondFactor {
    let data = info.get("data").unwrap_or(info);
    if data.get("has_totp").and_then(Value::as_bool) == Some(true) {
        SecondFactor::Code
    } else {
        SecondFactor::PasskeyOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_every_cookie_back() {
        assert_eq!(
            cookie_header(vec![
                "losos_session=abc; expires=Wed, 28 Oct 2026 18:00:00 GMT; path=/; HttpOnly; Secure",
                "other=1",
                "; broken",
            ]),
            "losos_session=abc; other=1"
        );
        assert_eq!(cookie_header(Vec::new()), "");
    }

    #[test]
    fn reads_the_person_from_the_answer() {
        let headers = [
            ("remote-user", "ada"),
            ("remote-name", "Ada Lovelace"),
            ("remote-email", ""),
            ("remote-groups", "losos-admins, staff,"),
        ];
        let person =
            person_from_headers(|name| headers.iter().find(|(k, _)| *k == name).map(|(_, v)| *v))
                .unwrap();
        assert_eq!(
            person,
            Person {
                name: "ada".into(),
                display_name: "Ada Lovelace".into(),
                email: None,
                groups: vec!["losos-admins".into(), "staff".into()],
            }
        );
        assert_eq!(person_from_headers(|_| None), None);
        let bare = person_from_headers(|n| (n == "remote-user").then_some("bob")).unwrap();
        assert_eq!(bare.display_name, "bob");
        assert!(bare.groups.is_empty());
    }

    #[test]
    fn asks_for_a_code_only_when_there_is_an_app() {
        let info = json!({"status": "OK", "data": {"display_name": "Ada", "method": "totp",
            "has_totp": true, "has_webauthn": true, "has_duo": false}});
        assert_eq!(second_factor(&info), SecondFactor::Code);
        let passkey = json!({"status": "OK", "data": {"has_totp": false, "has_webauthn": true}});
        assert_eq!(second_factor(&passkey), SecondFactor::PasskeyOnly);
    }

    #[test]
    fn a_login_names_the_person_who_typed() {
        let ada = Person {
            name: "ada".into(),
            display_name: "Ada".into(),
            email: None,
            groups: Vec::new(),
        };
        assert_eq!(accepted(ada.clone(), "ada"), Ok(ada.clone()));
        assert!(matches!(accepted(ada, "bob"), Err(Outcome::Refused(_))));
    }
}
