//! The wire between a guest's app server and the host's `app-windows`
//! (docs/app-windows.md).
//!
//! A guest system, macOS in a machine or on a Mac nearby, runs a server that
//! lists its apps, starts them, sends each of its windows as a video stream
//! and takes input back for it. The host shows each of those windows as a
//! window of its own. Nothing in here is particular to macOS: a guest maps
//! keys from the Linux key codes the host sends to its own, as Android's
//! would without mapping at all, and says in [`Welcome::missing`] what it
//! still needs someone to allow before it can see and drive its windows.
//!
//! Every message is a little-endian `u32` length, a type byte, then
//! `length - 1` bytes of payload. Control messages carry JSON; video carries
//! a fixed binary header and H.264 in Annex B, the framing any decoder takes
//! without knowing the encoder's own.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// The version both ends speak. A client with another is told so and let
/// go rather than misreading what follows.
pub const VERSION: u32 = 1;

/// The port servers listen on when nobody says otherwise.
pub const DEFAULT_PORT: u16 = 7180;

/// The largest message either end takes. A keyframe of a 6K window is a
/// few megabytes; a length beyond this is a stream out of step, not video.
pub const MAX_MESSAGE: usize = 64 << 20;

/// Modifier bits in [`Key::modifiers`].
pub mod modifiers {
    pub const SHIFT: u32 = 1;
    pub const CONTROL: u32 = 2;
    pub const ALT: u32 = 4;
    /// The logo key, which macOS takes as Command.
    pub const SUPER: u32 = 8;
    pub const CAPS_LOCK: u32 = 16;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// The secret the server was set up with, which is all that stands
    /// between anyone who can reach its port and its desktop.
    pub token: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub version: u32,
    /// The guest's own name for itself.
    pub name: String,
    /// "macOS 15.1" and the like.
    pub system: String,
    /// What someone still has to allow on the guest before its windows
    /// show or take input, in the guest's own words ("Screen Recording").
    /// Empty when nothing is missing.
    #[serde(default)]
    pub missing: Vec<String>,
    /// How the guest encodes video: "hardware" or "software".
    #[serde(default)]
    pub encoder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    /// The guest's identifier for the app (a bundle identifier on macOS),
    /// which is how the host names it back in [`Launch`].
    pub id: String,
    pub name: String,
    /// A square PNG, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// A window of its own.
    Normal,
    /// A menu, tooltip or panel that belongs over [`WindowInfo::parent`],
    /// at its offset from it.
    Popup,
}

/// A window as the host sees it: where it is on the guest's screen and how
/// big, in the guest's logical units (points on macOS), and how many video
/// pixels each unit is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window: u32,
    /// The [`AppInfo::id`] of the app it belongs to.
    pub app: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub kind: WindowKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRef {
    pub window: u32,
}

/// A picture of one window: H.264 in Annex B, `width` by `height` pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    pub window: u32,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub micros: u64,
    pub data: Vec<u8>,
}

/// The pointer, at a position in a window's own logical units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pointer {
    pub window: u32,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Button {
    pub window: u32,
    /// 0 primary, 1 secondary, 2 middle.
    pub button: u8,
    pub pressed: bool,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scroll {
    pub window: u32,
    pub x: f64,
    pub y: f64,
    /// Logical units, positive down and right, as Wayland counts them.
    pub dx: f64,
    pub dy: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub window: u32,
    /// A Linux key code (linux/input-event-codes.h), which names the key's
    /// place on the keyboard, not what the host's layout prints on it.
    pub code: u32,
    pub pressed: bool,
    /// [`modifiers`] held while it was pressed.
    pub modifiers: u32,
    /// What the host's layout types with it, when it types anything: a
    /// guest whose layout differs still gets the character the person
    /// meant, while shortcuts keep going by position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Text to type as it is, for what the host's layout or input method
/// composes and a key code would get wrong on the guest's layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Text {
    pub window: u32,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resize {
    pub window: u32,
    pub width: f64,
    pub height: f64,
}

/// One entry in an app's menus, in the shape derisk's global menu takes
/// (derisk's crates/derisk/src/menu.rs), so the host can hand it on as is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuEntry {
    Item {
        id: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shortcut: Option<String>,
        #[serde(default = "enabled")]
        enabled: bool,
    },
    Submenu {
        label: String,
        entries: Vec<MenuEntry>,
    },
    Separator,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

/// The menus of the app a window belongs to, sent when that window is
/// focused and whenever they change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Menus {
    pub window: u32,
    pub menus: Vec<Menu>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActivateMenu {
    pub window: u32,
    pub item: String,
}

/// The clipboard's text, sent by either end when its own changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clipboard {
    pub text: String,
}

/// Host: a file for the guest, such as an installer downloaded here, whose
/// bytes follow in [`Message::FileData`] pieces and then
/// [`Message::FileEnd`]. The guest keeps it in its own downloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileStart {
    /// The host's number for it, which the pieces and the answer carry.
    pub file: u32,
    /// Its name without a folder, which the guest may change to keep it
    /// from replacing another file.
    pub name: String,
    pub size: u64,
    /// Whether the guest should open it once it is all there, as a double
    /// click would: a disk image mounts, an installer package starts.
    pub open: bool,
}

/// The next piece of a file, at most [`FILE_PIECE`] bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct FileData {
    pub file: u32,
    pub data: Vec<u8>,
}

/// Guest: a file is all there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileSaved {
    pub file: u32,
    /// Where it is on the guest.
    pub path: String,
    /// Whether it was opened as asked; an [`Message::Error`] says why not.
    pub opened: bool,
}

/// How much of a file one [`Message::FileData`] carries.
pub const FILE_PIECE: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// Host: the first message, before anything else is answered.
    Hello(Hello),
    /// Guest: the answer to a [`Message::Hello`] it accepts.
    Welcome(Welcome),
    /// Guest: why it is about to hang up, or why a request failed.
    Error(ErrorMessage),

    /// Host: asks for [`Message::Apps`].
    ListApps,
    Apps(Vec<AppInfo>),
    /// Host: starts an app, or brings it forward when it runs.
    Launch(Launch),

    /// Host: send me windows. Until then a connection gets none, so one
    /// that only lists or launches apps costs the guest no capture.
    Watch,
    WindowOpened(WindowInfo),
    WindowChanged(WindowInfo),
    WindowClosed(WindowRef),
    Video(Video),

    Pointer(Pointer),
    Button(Button),
    Scroll(Scroll),
    Key(Key),
    Text(Text),
    Resize(Resize),
    Close(WindowRef),
    /// Host: this window has the host's focus, so the guest should raise it
    /// and give it its keyboard.
    Focus(WindowRef),
    /// Host: the next picture of this window should be a keyframe, for a
    /// decoder that lost its place.
    Keyframe(WindowRef),

    Menus(Menus),
    ActivateMenu(ActivateMenu),

    Clipboard(Clipboard),

    FileStart(FileStart),
    FileData(FileData),
    /// Host: the last piece of the file has been sent.
    FileEnd(FileRef),
    FileSaved(FileSaved),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRef {
    pub file: u32,
}

mod kind {
    pub const HELLO: u8 = 0x01;
    pub const WELCOME: u8 = 0x02;
    pub const ERROR: u8 = 0x03;
    pub const LIST_APPS: u8 = 0x10;
    pub const APPS: u8 = 0x11;
    pub const LAUNCH: u8 = 0x12;
    pub const WATCH: u8 = 0x20;
    pub const WINDOW_OPENED: u8 = 0x21;
    pub const WINDOW_CHANGED: u8 = 0x22;
    pub const WINDOW_CLOSED: u8 = 0x23;
    pub const VIDEO: u8 = 0x24;
    pub const POINTER: u8 = 0x30;
    pub const BUTTON: u8 = 0x31;
    pub const SCROLL: u8 = 0x32;
    pub const KEY: u8 = 0x33;
    pub const RESIZE: u8 = 0x34;
    pub const CLOSE: u8 = 0x35;
    pub const FOCUS: u8 = 0x36;
    pub const KEYFRAME: u8 = 0x37;
    pub const TEXT: u8 = 0x38;
    pub const MENUS: u8 = 0x40;
    pub const ACTIVATE_MENU: u8 = 0x41;
    pub const CLIPBOARD: u8 = 0x50;
    pub const FILE_START: u8 = 0x60;
    pub const FILE_DATA: u8 = 0x61;
    pub const FILE_END: u8 = 0x62;
    pub const FILE_SAVED: u8 = 0x63;
}

/// The fixed part of a video message: window, width, height, flags, time.
const VIDEO_HEADER: usize = 4 + 4 + 4 + 1 + 8;

impl Message {
    /// Whether this is a picture, which a sender may drop when the other
    /// end falls behind, as long as the next one it sends is a keyframe.
    pub fn is_video(&self) -> bool {
        matches!(self, Message::Video(_))
    }

    /// The whole message as it goes on the wire, length first.
    pub fn encode(&self) -> Vec<u8> {
        fn json(kind: u8, value: &impl Serialize) -> Vec<u8> {
            // Plain structs of strings and numbers always serialize.
            let body = serde_json::to_vec(value).expect("message serializes");
            frame(kind, &body)
        }
        match self {
            Message::Hello(m) => json(kind::HELLO, m),
            Message::Welcome(m) => json(kind::WELCOME, m),
            Message::Error(m) => json(kind::ERROR, m),
            Message::ListApps => frame(kind::LIST_APPS, b"{}"),
            Message::Apps(m) => json(kind::APPS, m),
            Message::Launch(m) => json(kind::LAUNCH, m),
            Message::Watch => frame(kind::WATCH, b"{}"),
            Message::WindowOpened(m) => json(kind::WINDOW_OPENED, m),
            Message::WindowChanged(m) => json(kind::WINDOW_CHANGED, m),
            Message::WindowClosed(m) => json(kind::WINDOW_CLOSED, m),
            Message::Video(v) => {
                let mut body = Vec::with_capacity(VIDEO_HEADER + v.data.len());
                body.extend_from_slice(&v.window.to_le_bytes());
                body.extend_from_slice(&v.width.to_le_bytes());
                body.extend_from_slice(&v.height.to_le_bytes());
                body.push(u8::from(v.keyframe));
                body.extend_from_slice(&v.micros.to_le_bytes());
                body.extend_from_slice(&v.data);
                frame(kind::VIDEO, &body)
            }
            Message::Pointer(m) => json(kind::POINTER, m),
            Message::Button(m) => json(kind::BUTTON, m),
            Message::Scroll(m) => json(kind::SCROLL, m),
            Message::Key(m) => json(kind::KEY, m),
            Message::Text(m) => json(kind::TEXT, m),
            Message::Resize(m) => json(kind::RESIZE, m),
            Message::Close(m) => json(kind::CLOSE, m),
            Message::Focus(m) => json(kind::FOCUS, m),
            Message::Keyframe(m) => json(kind::KEYFRAME, m),
            Message::Menus(m) => json(kind::MENUS, m),
            Message::ActivateMenu(m) => json(kind::ACTIVATE_MENU, m),
            Message::Clipboard(m) => json(kind::CLIPBOARD, m),
            Message::FileStart(m) => json(kind::FILE_START, m),
            Message::FileData(m) => {
                let mut body = Vec::with_capacity(4 + m.data.len());
                body.extend_from_slice(&m.file.to_le_bytes());
                body.extend_from_slice(&m.data);
                frame(kind::FILE_DATA, &body)
            }
            Message::FileEnd(m) => json(kind::FILE_END, m),
            Message::FileSaved(m) => json(kind::FILE_SAVED, m),
        }
    }

    /// A message from its type byte and payload.
    pub fn decode(kind: u8, body: &[u8]) -> io::Result<Message> {
        fn json<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
            serde_json::from_slice(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Ok(match kind {
            kind::HELLO => Message::Hello(json(body)?),
            kind::WELCOME => Message::Welcome(json(body)?),
            kind::ERROR => Message::Error(json(body)?),
            kind::LIST_APPS => Message::ListApps,
            kind::APPS => Message::Apps(json(body)?),
            kind::LAUNCH => Message::Launch(json(body)?),
            kind::WATCH => Message::Watch,
            kind::WINDOW_OPENED => Message::WindowOpened(json(body)?),
            kind::WINDOW_CHANGED => Message::WindowChanged(json(body)?),
            kind::WINDOW_CLOSED => Message::WindowClosed(json(body)?),
            kind::VIDEO => {
                if body.len() < VIDEO_HEADER {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "short video message",
                    ));
                }
                let u32_at = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
                Message::Video(Video {
                    window: u32_at(0),
                    width: u32_at(4),
                    height: u32_at(8),
                    keyframe: body[12] & 1 != 0,
                    micros: u64::from_le_bytes(body[13..21].try_into().unwrap()),
                    data: body[VIDEO_HEADER..].to_vec(),
                })
            }
            kind::POINTER => Message::Pointer(json(body)?),
            kind::BUTTON => Message::Button(json(body)?),
            kind::SCROLL => Message::Scroll(json(body)?),
            kind::KEY => Message::Key(json(body)?),
            kind::TEXT => Message::Text(json(body)?),
            kind::RESIZE => Message::Resize(json(body)?),
            kind::CLOSE => Message::Close(json(body)?),
            kind::FOCUS => Message::Focus(json(body)?),
            kind::KEYFRAME => Message::Keyframe(json(body)?),
            kind::MENUS => Message::Menus(json(body)?),
            kind::ACTIVATE_MENU => Message::ActivateMenu(json(body)?),
            kind::CLIPBOARD => Message::Clipboard(json(body)?),
            kind::FILE_START => Message::FileStart(json(body)?),
            kind::FILE_DATA => {
                if body.len() < 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "short file message",
                    ));
                }
                Message::FileData(FileData {
                    file: u32::from_le_bytes(body[..4].try_into().unwrap()),
                    data: body[4..].to_vec(),
                })
            }
            kind::FILE_END => Message::FileEnd(json(body)?),
            kind::FILE_SAVED => Message::FileSaved(json(body)?),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown message type {other:#04x}"),
                ))
            }
        })
    }
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.extend_from_slice(&(body.len() as u32 + 1).to_le_bytes());
    out.push(kind);
    out.extend_from_slice(body);
    out
}

/// Writes one message.
pub fn write(w: &mut impl Write, message: &Message) -> io::Result<()> {
    w.write_all(&message.encode())
}

/// Reads one message, or fails at the end of the stream.
pub fn read(r: &mut impl Read) -> io::Result<Message> {
    let mut length = [0; 4];
    r.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message of {length} bytes"),
        ));
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body)?;
    Message::decode(body[0], &body[1..])
}

/// Compares a token in time that does not depend on where they differ, so
/// the answer's timing tells a guesser nothing about how close they came.
pub fn token_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8 | u8::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0 && !expected.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(m: Message) {
        let bytes = m.encode();
        let back = read(&mut &bytes[..]).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn every_kind_round_trips() {
        round_trip(Message::Hello(Hello {
            version: VERSION,
            token: "t".into(),
        }));
        round_trip(Message::Welcome(Welcome {
            version: VERSION,
            name: "Mac".into(),
            system: "macOS 15.1".into(),
            missing: vec!["Screen Recording".into()],
            encoder: "hardware".into(),
        }));
        round_trip(Message::ListApps);
        round_trip(Message::Watch);
        round_trip(Message::Apps(vec![AppInfo {
            id: "com.apple.TextEdit".into(),
            name: "TextEdit".into(),
            icon: None,
        }]));
        round_trip(Message::WindowOpened(WindowInfo {
            window: 7,
            app: "com.apple.TextEdit".into(),
            title: "Untitled".into(),
            x: 10.0,
            y: 20.5,
            width: 600.0,
            height: 400.0,
            scale: 2.0,
            kind: WindowKind::Popup,
            parent: Some(3),
        }));
        round_trip(Message::Video(Video {
            window: 7,
            width: 1200,
            height: 800,
            keyframe: true,
            micros: 1 << 40,
            data: vec![0, 0, 0, 1, 0x67],
        }));
        round_trip(Message::FileStart(FileStart {
            file: 1,
            name: "Rectangle.dmg".into(),
            size: 3 << 20,
            open: true,
        }));
        round_trip(Message::FileData(FileData {
            file: 1,
            data: vec![0x78, 0x01, 0x73],
        }));
        round_trip(Message::FileEnd(FileRef { file: 1 }));
        round_trip(Message::FileSaved(FileSaved {
            file: 1,
            path: "/Users/admin/Downloads/Rectangle.dmg".into(),
            opened: true,
        }));
        round_trip(Message::Key(Key {
            window: 7,
            code: 30,
            pressed: true,
            modifiers: modifiers::SUPER,
            text: Some("a".into()),
        }));
        round_trip(Message::Menus(Menus {
            window: 7,
            menus: vec![Menu {
                title: "File".into(),
                entries: vec![
                    MenuEntry::Item {
                        id: "0/1".into(),
                        label: "New".into(),
                        shortcut: Some("Cmd+N".into()),
                        enabled: true,
                    },
                    MenuEntry::Separator,
                    MenuEntry::Submenu {
                        label: "Open Recent".into(),
                        entries: vec![],
                    },
                ],
            }],
        }));
        round_trip(Message::Clipboard(Clipboard {
            text: "héllo".into(),
        }));
    }

    #[test]
    fn menus_are_derisks_shape() {
        let entry = MenuEntry::Item {
            id: "1".into(),
            label: "Quit".into(),
            shortcut: None,
            enabled: false,
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({"kind": "item", "id": "1", "label": "Quit", "enabled": false})
        );
    }

    #[test]
    fn refuses_nonsense_lengths() {
        assert!(read(&mut &[0u8, 0, 0, 0][..]).is_err());
        assert!(read(&mut &u32::MAX.to_le_bytes()[..]).is_err());
        assert!(read(&mut &[2u8, 0, 0, 0, 0x7f, 0][..]).is_err());
    }

    #[test]
    fn tokens() {
        assert!(token_matches("secret", "secret"));
        assert!(!token_matches("secreT", "secret"));
        assert!(!token_matches("secret!", "secret"));
        assert!(!token_matches("", ""));
    }
}
