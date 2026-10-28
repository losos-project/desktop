//! The PAM module, `pam_losos_login.so`: the bridge at the login screen.
//!
//! It goes before pam_systemd_home in the `derisk-login` and `login` stacks
//! (nixos/modules/company.nix), as `requisite` with `bin=` naming where
//! homectl and userdbctl are, and reads the password the way every module
//! after it will (`pam_get_authtok`), so the person types it once. Then:
//!
//! - on a computer nobody joined, for a local account, or for a name userdb
//!   gives to something that is not a home, it steps aside (`PAM_IGNORE`);
//! - LosOS Login accepting the password makes the company account, or
//!   brings its record up to date, and lets pam_systemd_home go on with the
//!   same password; a password changed at the box re-keys the home first,
//!   with the previous one, which only the person knows;
//! - LosOS Login refusing ends the login there (`PAM_AUTH_ERR`);
//! - LosOS Login out of reach steps aside for someone who has logged in
//!   here before, so homed's record decides with the password LosOS Login
//!   last accepted, and stops anyone else (`PAM_AUTHINFO_UNAVAIL`), since
//!   there is nothing here to check them against.

use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    ptr,
};

use crate::{
    homed::{self, Known, Record},
    join::Joined,
    login::{self, Outcome, Person},
    valid_user_name, Error, JOINED_FILE,
};

#[allow(
    dead_code,
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    clippy::all
)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/pam.rs"));
}

/// What the person at the login screen is asked and told.
pub trait Conversation {
    /// The user name, asked for if the application gave none.
    fn user(&mut self) -> Option<String>;
    /// The password, asked for if no module before has.
    fn password(&mut self) -> Option<String>;
    /// One more answer: shown as typed, or hidden.
    fn ask(&mut self, prompt: &str, shown: bool) -> Option<String>;
    /// A line to show: an error, or news.
    fn tell(&mut self, message: &str, error: bool);
}

/// What homed does for the module; [`System`] is the real one.
pub trait Homes {
    fn lookup(&self, user: &str) -> Known;
    fn create(&self, person: &Person, joined: &Joined, password: &str) -> Result<(), Error>;
    fn update(&self, record: &Record, person: &Person, joined: &Joined) -> Result<(), Error>;
    fn authenticate(&self, user: &str, password: &str) -> bool;
    fn passwd(&self, user: &str, old: &str, new: &str) -> Result<(), Error>;
}

/// homed through homectl and userdbctl.
pub struct System;

impl Homes for System {
    fn lookup(&self, user: &str) -> Known {
        homed::lookup(user)
    }
    fn create(&self, person: &Person, joined: &Joined, password: &str) -> Result<(), Error> {
        homed::create(person, joined, password)
    }
    fn update(&self, record: &Record, person: &Person, joined: &Joined) -> Result<(), Error> {
        homed::update(record, person, joined)
    }
    fn authenticate(&self, user: &str, password: &str) -> bool {
        homed::authenticate(user, password)
    }
    fn passwd(&self, user: &str, old: &str, new: &str) -> Result<(), Error> {
        homed::passwd(user, old, new)
    }
}

/// What the module answers PAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Not the bridge's to decide.
    StepAside,
    /// The company account is ready for pam_systemd_home.
    Ready,
    /// LosOS Login refused.
    Refused,
    /// Nothing can check this person now.
    Unavailable,
    /// Something here failed.
    Failed,
}

impl Decision {
    fn code(self) -> c_int {
        (match self {
            Decision::StepAside => sys::PAM_IGNORE,
            Decision::Ready => sys::PAM_SUCCESS,
            Decision::Refused => sys::PAM_AUTH_ERR,
            Decision::Unavailable => sys::PAM_AUTHINFO_UNAVAIL,
            Decision::Failed => sys::PAM_SYSTEM_ERR,
        }) as c_int
    }
}

/// The asker [`decide`] takes: [`login::sign_in`] in the module, a stand-in
/// in tests.
pub type Ask<'a> =
    dyn FnMut(&Joined, &str, &str, &mut dyn FnMut(&str) -> Option<String>) -> Outcome + 'a;

/// The module's whole judgement, with its I/O passed in.
pub fn decide(
    joined: Option<&Joined>,
    conv: &mut dyn Conversation,
    homes: &dyn Homes,
    ask: &mut Ask<'_>,
) -> Decision {
    let Some(joined) = joined else {
        return Decision::StepAside;
    };
    let Some(user) = conv.user().filter(|u| valid_user_name(u)) else {
        return Decision::StepAside;
    };
    let record = match homes.lookup(&user) {
        Known::Other => return Decision::StepAside,
        Known::Home(r) if !r.is_company(joined) => return Decision::StepAside,
        Known::Home(r) => Some(r),
        Known::Nobody => None,
    };
    let Some(password) = conv.password() else {
        return Decision::Refused;
    };
    let outcome = {
        let mut code = |prompt: &str| conv.ask(prompt, true);
        ask(joined, &user, &password, &mut code)
    };
    match (outcome, record) {
        (Outcome::Unreachable(why), Some(_)) => {
            tracing::info!(%user, %why, "LosOS Login is out of reach; homed decides");
            Decision::StepAside
        }
        (Outcome::Unreachable(why), None) => {
            conv.tell(
                &format!(
                    "This computer cannot reach LosOS Login at {} to set it up for {user}: {why}",
                    joined.host
                ),
                true,
            );
            Decision::Unavailable
        }
        (Outcome::Refused(why), _) => {
            conv.tell(&why, true);
            Decision::Refused
        }
        (Outcome::Accepted(person), None) => {
            conv.tell(
                &format!("Setting up this computer for {}…", person.display_name),
                false,
            );
            match homes.create(&person, joined, &password) {
                Ok(()) => Decision::Ready,
                Err(e) => {
                    conv.tell(&format!("Could not set up {user}'s account: {e}"), true);
                    Decision::Failed
                }
            }
        }
        (Outcome::Accepted(person), Some(record)) => {
            if let Err(e) = homes.update(&record, &person, joined) {
                // Their old name or groups for one more login is no reason
                // to keep them out.
                tracing::warn!(%user, error = %e, "could not bring the record up to date");
            }
            if homes.authenticate(&user, &password) {
                return Decision::Ready;
            }
            let Some(old) = conv.ask(
                "Your LosOS Login password has changed. Your previous password, to move this computer to the new one: ",
                false,
            ) else {
                return Decision::Refused;
            };
            match homes.passwd(&user, &old, &password) {
                Ok(()) => Decision::Ready,
                Err(e) => {
                    conv.tell(
                        &format!("This computer kept the previous password: {e}"),
                        true,
                    );
                    Decision::Refused
                }
            }
        }
    }
}

/// A PAM handle, for as long as the call it came with.
struct Handle(*mut sys::pam_handle_t);

impl Handle {
    fn prompt(&self, style: u32, text: &str) -> Option<String> {
        let text = CString::new(text.replace('\0', "")).ok()?;
        let mut response: *mut c_char = ptr::null_mut();
        // SAFETY: the handle is the one PAM passed this call; "%s" makes
        // the text a plain argument; a response PAM sets is malloc'ed and
        // ours to free.
        let rc = unsafe {
            sys::pam_prompt(
                self.0,
                style as c_int,
                &mut response,
                c"%s".as_ptr(),
                text.as_ptr(),
            )
        };
        if rc != sys::PAM_SUCCESS as c_int || response.is_null() {
            return None;
        }
        // SAFETY: as above, a NUL-terminated string PAM allocated.
        let answer = unsafe { CStr::from_ptr(response) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: freed once, by its owner.
        unsafe { libc::free(response.cast::<c_void>()) };
        Some(answer)
    }
}

impl Conversation for Handle {
    fn user(&mut self) -> Option<String> {
        let mut user: *const c_char = ptr::null();
        // SAFETY: PAM owns the string it points `user` at, for the call.
        let rc = unsafe { sys::pam_get_user(self.0, &mut user, ptr::null()) };
        if rc != sys::PAM_SUCCESS as c_int || user.is_null() {
            return None;
        }
        // SAFETY: as above.
        Some(
            unsafe { CStr::from_ptr(user) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn password(&mut self) -> Option<String> {
        let mut token: *const c_char = ptr::null();
        // SAFETY: as for the user; PAM keeps the token as PAM_AUTHTOK,
        // where pam_systemd_home finds it.
        let rc = unsafe {
            sys::pam_get_authtok(self.0, sys::PAM_AUTHTOK as c_int, &mut token, ptr::null())
        };
        if rc != sys::PAM_SUCCESS as c_int || token.is_null() {
            return None;
        }
        // SAFETY: as above.
        Some(
            unsafe { CStr::from_ptr(token) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn ask(&mut self, prompt: &str, shown: bool) -> Option<String> {
        let style = if shown {
            sys::PAM_PROMPT_ECHO_ON
        } else {
            sys::PAM_PROMPT_ECHO_OFF
        };
        self.prompt(style, prompt)
    }

    fn tell(&mut self, message: &str, error: bool) {
        let style = if error {
            sys::PAM_ERROR_MSG
        } else {
            sys::PAM_TEXT_INFO
        };
        let _ = self.prompt(style, message);
    }
}

/// `auth`: the bridge. Panics become a system error, never an unwind
/// through libpam.
///
/// # Safety
/// Called by libpam with a live handle.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut sys::pam_handle_t,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| {
        // Into the display manager's journal, under its own unit.
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .without_time()
            .try_init();
        for i in 0..usize::try_from(argc).unwrap_or(0) {
            // SAFETY: libpam passes argc NUL-terminated strings.
            let arg = unsafe { CStr::from_ptr(*argv.add(i)) }.to_string_lossy();
            if let Some(dir) = arg.strip_prefix("bin=") {
                homed::use_bin(dir);
            }
        }
        let mut handle = Handle(pamh);
        let joined = match Joined::load(Path::new(JOINED_FILE)) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "the joined box cannot be read");
                return Decision::StepAside.code();
            }
        };
        let mut ask = |j: &Joined, u: &str, p: &str, c: &mut dyn FnMut(&str) -> Option<String>| {
            login::sign_in(j, u, p, c)
        };
        decide(joined.as_ref(), &mut handle, &System, &mut ask).code()
    }))
    .unwrap_or(sys::PAM_SYSTEM_ERR as c_int)
}

/// `auth`'s second half: nothing to set, pam_systemd_home does it.
///
/// # Safety
/// Called by libpam.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut sys::pam_handle_t,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    sys::PAM_IGNORE as c_int
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::{Gateway, BOX_ADMINS};
    use std::cell::RefCell;

    fn joined() -> Joined {
        Joined {
            host: "mattbox.local".into(),
            certificate: String::new(),
            public_name: None,
            gateway: Gateway {
                name: "office-1".into(),
                url: "http://gw.local:8443".into(),
                cluster: Some("office".into()),
            },
            realm: "office".into(),
            admin_groups: vec![BOX_ADMINS.into()],
            joined_by: "matus".into(),
            joined_at: 1,
        }
    }

    fn ada() -> Person {
        Person {
            name: "ada".into(),
            display_name: "Ada Lovelace".into(),
            email: None,
            groups: vec![BOX_ADMINS.into()],
        }
    }

    fn home(realm: Option<&str>) -> Known {
        Known::Home(Record {
            realm: realm.map(str::to_owned),
            real_name: Some("Ada Lovelace".into()),
            email: None,
            member_of: vec!["wheel".into()],
            locked: false,
        })
    }

    #[derive(Default)]
    struct Screen {
        user: &'static str,
        answers: Vec<&'static str>,
        told: Vec<String>,
    }

    impl Conversation for Screen {
        fn user(&mut self) -> Option<String> {
            Some(self.user.into())
        }
        fn password(&mut self) -> Option<String> {
            Some("new-secret".into())
        }
        fn ask(&mut self, _prompt: &str, _shown: bool) -> Option<String> {
            (!self.answers.is_empty()).then(|| self.answers.remove(0).into())
        }
        fn tell(&mut self, message: &str, _error: bool) {
            self.told.push(message.into());
        }
    }

    #[derive(Default)]
    struct FakeHomes {
        known: Option<Known>,
        password: &'static str,
        done: RefCell<Vec<String>>,
    }

    impl Homes for FakeHomes {
        fn lookup(&self, _user: &str) -> Known {
            self.known.clone().unwrap_or(Known::Nobody)
        }
        fn create(&self, person: &Person, _j: &Joined, password: &str) -> Result<(), Error> {
            self.done
                .borrow_mut()
                .push(format!("create {} {password}", person.name));
            Ok(())
        }
        fn update(&self, _r: &Record, person: &Person, _j: &Joined) -> Result<(), Error> {
            self.done
                .borrow_mut()
                .push(format!("update {}", person.name));
            Ok(())
        }
        fn authenticate(&self, _user: &str, password: &str) -> bool {
            password == self.password
        }
        fn passwd(&self, user: &str, old: &str, new: &str) -> Result<(), Error> {
            if old != self.password {
                return Err(Error::new("wrong"));
            }
            self.done
                .borrow_mut()
                .push(format!("passwd {user} {old} {new}"));
            Ok(())
        }
    }

    fn run(
        joined: Option<&Joined>,
        screen: &mut Screen,
        homes: &FakeHomes,
        outcome: Outcome,
    ) -> Decision {
        let mut ask = |_: &Joined, _: &str, _: &str, _: &mut dyn FnMut(&str) -> Option<String>| {
            outcome.clone()
        };
        decide(joined, screen, homes, &mut ask)
    }

    #[test]
    fn steps_aside_where_it_has_no_say() {
        let homes = FakeHomes::default();
        let mut screen = Screen {
            user: "ada",
            ..Screen::default()
        };
        let yes = Outcome::Accepted(ada());
        assert_eq!(
            run(None, &mut screen, &homes, yes.clone()),
            Decision::StepAside
        );
        let j = joined();
        let local = FakeHomes {
            known: Some(home(None)),
            ..FakeHomes::default()
        };
        assert_eq!(
            run(Some(&j), &mut screen, &local, yes.clone()),
            Decision::StepAside
        );
        let system = FakeHomes {
            known: Some(Known::Other),
            ..FakeHomes::default()
        };
        assert_eq!(
            run(Some(&j), &mut screen, &system, yes.clone()),
            Decision::StepAside
        );
        let mut bad = Screen {
            user: "Ada;rm",
            ..Screen::default()
        };
        assert_eq!(run(Some(&j), &mut bad, &homes, yes), Decision::StepAside);
        assert!(homes.done.borrow().is_empty());
    }

    #[test]
    fn makes_the_account_at_the_first_login() {
        let homes = FakeHomes::default();
        let mut screen = Screen {
            user: "ada",
            ..Screen::default()
        };
        let j = joined();
        assert_eq!(
            run(Some(&j), &mut screen, &homes, Outcome::Accepted(ada())),
            Decision::Ready
        );
        assert_eq!(*homes.done.borrow(), ["create ada new-secret"]);
        assert_eq!(screen.told, ["Setting up this computer for Ada Lovelace…"]);
    }

    #[test]
    fn falls_back_to_homed_only_for_someone_it_knows() {
        let j = joined();
        let mut screen = Screen {
            user: "ada",
            ..Screen::default()
        };
        let known = FakeHomes {
            known: Some(home(Some("office"))),
            ..FakeHomes::default()
        };
        let away = Outcome::Unreachable("no route to host".into());
        assert_eq!(
            run(Some(&j), &mut screen, &known, away.clone()),
            Decision::StepAside
        );
        let stranger = FakeHomes::default();
        assert_eq!(
            run(Some(&j), &mut screen, &stranger, away),
            Decision::Unavailable
        );
        assert!(screen.told[0].contains("cannot reach LosOS Login at mattbox.local"));
    }

    #[test]
    fn a_refusal_keeps_out_even_someone_it_knows() {
        let j = joined();
        let mut screen = Screen {
            user: "ada",
            ..Screen::default()
        };
        let known = FakeHomes {
            known: Some(home(Some("office"))),
            password: "new-secret",
            ..FakeHomes::default()
        };
        let no = Outcome::Refused("LosOS Login did not accept that name and password.".into());
        assert_eq!(run(Some(&j), &mut screen, &known, no), Decision::Refused);
        assert!(known.done.borrow().is_empty());
    }

    #[test]
    fn rekeys_the_home_when_the_password_changed_at_the_box() {
        let j = joined();
        let homes = FakeHomes {
            known: Some(home(Some("office"))),
            password: "old-secret",
            ..FakeHomes::default()
        };
        let mut screen = Screen {
            user: "ada",
            answers: vec!["old-secret"],
            ..Screen::default()
        };
        assert_eq!(
            run(Some(&j), &mut screen, &homes, Outcome::Accepted(ada())),
            Decision::Ready
        );
        assert_eq!(
            *homes.done.borrow(),
            ["update ada", "passwd ada old-secret new-secret"]
        );
        let mut wrong = Screen {
            user: "ada",
            answers: vec!["guess"],
            ..Screen::default()
        };
        let homes = FakeHomes {
            known: Some(home(Some("office"))),
            password: "old-secret",
            ..FakeHomes::default()
        };
        assert_eq!(
            run(Some(&j), &mut wrong, &homes, Outcome::Accepted(ada())),
            Decision::Refused
        );
    }
}
