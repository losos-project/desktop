//! A page's file upload (docs/danube.md, "Uploading files"): WebKit's
//! file chooser request answered through the desktop's file chooser
//! portal, `org.freedesktop.portal.FileChooser.OpenFile`, which the
//! sandbox's bus proxy lets Danube call. The portal shows the desktop's
//! own dialog outside the sandbox, and hands back the chosen files
//! through the document portal: the sandbox sees only those, under
//! `$XDG_RUNTIME_DIR/doc` (main.rs, "the document portal").
//!
//! On WebKit's thread, over GDBus. A portal call answers with the path of
//! a request object, and the person's choice comes later as that object's
//! `Response` signal, so each WebKit request is kept by the path it will
//! be answered on until then.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use tracing::{debug, warn};

use crate::engine::emit;
use crate::shared::Event;
use crate::webkit::{Arg, Bus, FileChooserRequest, Var};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const REQUEST: &str = "org.freedesktop.portal.Request";

pub struct FileChooser {
    /// `None` until the first upload, and for good once there is no bus.
    bus: Option<Bus>,
    /// Requests waiting for the person, by the request object's path.
    waiting: HashMap<String, FileChooserRequest>,
    next: u64,
    me: Weak<RefCell<FileChooser>>,
}

impl FileChooser {
    pub fn new() -> Rc<RefCell<Self>> {
        Rc::new_cyclic(|me| {
            RefCell::new(Self {
                bus: None,
                waiting: HashMap::new(),
                next: 1,
                me: me.clone(),
            })
        })
    }

    /// Asks the portal for files to answer `request` with; `false` when
    /// it cannot be asked, for WebKit to cancel the request.
    pub fn open(&mut self, request: FileChooserRequest) -> bool {
        if self.bus.is_none() && !self.connect() {
            emit(Event::Notice("Uploading needs the desktop's file chooser, and there is no session bus".into()));
            return false;
        }
        let Some(bus) = &self.bus else { return false };
        let token = format!("danube{}", self.next);
        self.next += 1;
        // Where the portal will answer: the request path is made from the
        // caller's bus name and the token, so the answer is listened for
        // before the call, and cannot arrive first.
        let path = request_path(&bus.unique_name(), &token);
        let options = options(&token, request.select_multiple(), &request.mime_types());
        self.waiting.insert(path.clone(), request);
        let me = self.me.clone();
        let title = Var::Str(String::new());
        let args = Var::Tuple(vec![title, Var::Str("Upload".into()), options]);
        // The portal is often not running yet, and D-Bus starts it (and
        // its backends) for this call, which takes a phone or a machine
        // without a GPU most of a minute.
        bus.call_waiting(PORTAL, PATH, CHOOSER, "OpenFile", &args, 120_000, move |reply| {
            let Some(me) = me.upgrade() else { return };
            let Ok(mut me) = me.try_borrow_mut() else { return };
            match reply {
                // An older portal names its own path: wait there instead.
                Ok(args) => match args.first() {
                    Some(Arg::Str(handle)) if *handle != path => {
                        if let Some(request) = me.waiting.remove(&path) {
                            me.waiting.insert(handle.clone(), request);
                        }
                    }
                    _ => {}
                },
                Err(error) => {
                    warn!(error, "the file chooser portal is not there");
                    emit(Event::Notice("The desktop's file chooser did not open".into()));
                    if let Some(request) = me.waiting.remove(&path) {
                        request.cancel();
                    }
                }
            }
        });
        true
    }

    fn connect(&mut self) -> bool {
        match Bus::session() {
            Ok(bus) => {
                let me = self.me.clone();
                bus.on_signal_anywhere(PORTAL, REQUEST, move |path, signal, args| {
                    if signal != "Response" {
                        return;
                    }
                    let Some(me) = me.upgrade() else { return };
                    let Ok(mut me) = me.try_borrow_mut() else { return };
                    if let Some(request) = me.waiting.remove(path) {
                        answer(&request, &args);
                    }
                });
                self.bus = Some(bus);
                true
            }
            Err(error) => {
                warn!(error, "no session bus for the file chooser");
                false
            }
        }
    }
}

/// Answers `request` with the portal's `Response`: (code, results), code 0
/// when the person chose, 1 when they cancelled.
fn answer(request: &FileChooserRequest, args: &[Arg]) {
    let chosen = match (args.first(), args.get(1).and_then(|r| r.get("uris"))) {
        (Some(Arg::U32(0)), Some(Arg::Strings(uris))) => {
            uris.iter().filter_map(|uri| file_path(uri)).collect::<Vec<_>>()
        }
        _ => Vec::new(),
    };
    debug!(files = chosen.len(), "the file chooser answered");
    if chosen.is_empty() {
        request.cancel();
    } else {
        request.select_files(&chosen);
    }
}

/// `/org/freedesktop/portal/desktop/request/SENDER/TOKEN`, SENDER being
/// the caller's unique name without its colon and with `_` for each dot.
fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{PATH}/request/{sender}/{token}")
}

/// `OpenFile`'s options: the token, whether several files may be chosen,
/// and a filter for the types the page accepts, with every file as the
/// other choice, since a page's list is only a hint.
fn options(token: &str, multiple: bool, mime_types: &[String]) -> Var {
    let mut options = vec![
        ("handle_token".to_owned(), Var::Str(token.to_owned())),
        ("multiple".to_owned(), Var::Bool(multiple)),
        ("modal".to_owned(), Var::Bool(true)),
    ];
    if !mime_types.is_empty() {
        // a(sa(us)): a name, and (0 for a glob, 1 for a MIME type, pattern).
        let accepted = mime_types
            .iter()
            .map(|m| Var::Tuple(vec![Var::U32(1), Var::Str(m.clone())]))
            .collect();
        let all = vec![Var::Tuple(vec![Var::U32(0), Var::Str("*".into())])];
        let filter = |name: &str, rules| Var::Tuple(vec![Var::Str(name.into()), Var::Array("(us)", rules)]);
        options.push((
            "filters".to_owned(),
            Var::Array("(sa(us))", vec![filter("Accepted files", accepted), filter("All files", all)]),
        ));
    }
    Var::Dict(options)
}

/// The local path a `file://` URI names, with its %-escapes undone.
fn file_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // An empty host or localhost, then the absolute path.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_paths_follow_the_portal() {
        assert_eq!(
            request_path(":1.42", "danube3"),
            "/org/freedesktop/portal/desktop/request/1_42/danube3"
        );
    }

    #[test]
    fn uris_become_paths() {
        assert_eq!(
            file_path("file:///run/user/1000/doc/ab12/My%20CV.pdf").as_deref(),
            Some("/run/user/1000/doc/ab12/My CV.pdf")
        );
        assert_eq!(file_path("file://localhost/tmp/a").as_deref(), Some("/tmp/a"));
        assert_eq!(file_path("https://example.org/a"), None);
        assert_eq!(file_path("file:///bad%2"), None);
    }

    #[test]
    fn filters_offer_the_page_types_then_everything() {
        let Var::Dict(set) = options("t", true, &["image/png".into()]) else { panic!() };
        assert!(set.contains(&("multiple".into(), Var::Bool(true))));
        let (_, Var::Array(kind, filters)) = set.iter().find(|(k, _)| k == "filters").unwrap() else {
            panic!()
        };
        assert_eq!(*kind, "(sa(us))");
        assert_eq!(filters.len(), 2);
        let Var::Dict(none) = options("t", false, &[]) else { panic!() };
        assert!(!none.iter().any(|(k, _)| k == "filters"));
    }
}
