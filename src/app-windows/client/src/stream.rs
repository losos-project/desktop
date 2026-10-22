//! What comes from the guest, turned into events: control messages in the
//! order they came, and each window's latest picture.
//!
//! One thread reads the connection. Each window's video goes to a decoding
//! thread of its own, so a big window does not hold up a small one, and its
//! pictures land in a mailbox that keeps only the newest: whoever shows them
//! takes one when it can draw, and pictures it had no time for are skipped
//! rather than queued behind.
//!
//! Everything read can also be written to a recording, which `replay` plays
//! back without a guest, timed as it came.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use app_windows_protocol::{self as protocol, Message, Video, WindowRef};
use crossbeam_channel::{bounded, Sender, TrySendError};

use crate::decode::{Decoder, Picture};
use crate::link::Link;

pub enum Event {
    /// Anything but video, as it came.
    Message(Message),
    /// The window has a new picture in [`Frames`].
    Picture(u32),
    /// The guest hung up, or the recording ended.
    Lost(String),
}

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

/// Pictures to show, the newest for each window.
#[derive(Default)]
pub struct Frames(Mutex<HashMap<u32, Picture>>);

impl Frames {
    /// The window's newest picture, if one came since the last take.
    pub fn take(&self, window: u32) -> Option<Picture> {
        self.0.lock().unwrap().remove(&window)
    }

    /// Whether the mailbox was empty, so whoever shows it needs telling.
    fn put(&self, window: u32, picture: Picture) -> bool {
        self.0.lock().unwrap().insert(window, picture).is_none()
    }
}

/// What a window's video has cost, for `record`'s report.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct WindowStats {
    pub pictures: u64,
    pub keyframes: u64,
    pub bytes: u64,
    /// Pictures skipped because decoding fell behind or failed.
    pub skipped: u64,
    pub decode_micros: u64,
    pub on_gpu: bool,
    /// Guest time of the first and last picture, for the frame rate.
    pub first_micros: u64,
    pub last_micros: u64,
}

pub type Stats = Arc<Mutex<HashMap<u32, WindowStats>>>;

/// Pictures waiting to be decoded per window. Two is enough to keep a
/// decoder busy; more is only latency.
const DECODE_QUEUE: usize = 2;

pub struct Options {
    pub hardware: bool,
    pub recording: Option<File>,
    /// Decode every picture, waiting for the decoder rather than skipping
    /// ahead. A recording played back has no guest to send a keyframe
    /// after a skip, so it waits instead; it can, being a file.
    pub every_picture: bool,
}

/// Reads the guest until it hangs up. `link` asks for keyframes when a
/// decoder loses its place.
pub fn run(
    mut reader: impl Read,
    link: Link,
    options: Options,
    frames: Arc<Frames>,
    stats: Stats,
    sink: Sink,
) {
    let mut recording = options.recording.map(BufWriter::new);
    let started = Instant::now();
    let mut decoders: HashMap<u32, (Sender<Video>, Arc<AtomicBool>)> = HashMap::new();
    loop {
        let message = match protocol::read(&mut reader) {
            Ok(m) => m,
            Err(e) => {
                sink(Event::Lost(if e.kind() == io::ErrorKind::UnexpectedEof {
                    "the guest hung up".into()
                } else {
                    e.to_string()
                }));
                return;
            }
        };
        if let Some(out) = recording.as_mut() {
            let at = started.elapsed().as_micros() as u64;
            // A full disk ends the recording, not the session.
            if out
                .write_all(&at.to_le_bytes())
                .and_then(|()| out.write_all(&message.encode()))
                .and_then(|()| out.flush())
                .is_err()
            {
                eprintln!("app-windows: the recording stopped: could not write it");
                recording = None;
            }
        }
        match message {
            Message::Video(video) => {
                let window = video.window;
                let (tx, lost) = decoders.entry(window).or_insert_with(|| {
                    decoder(
                        window,
                        options.hardware,
                        link.clone(),
                        frames.clone(),
                        stats.clone(),
                        sink.clone(),
                    )
                });
                if options.every_picture {
                    // Only fails once the decoder has given up.
                    let _ = tx.send(video);
                } else if let Err(TrySendError::Full(video)) = tx.try_send(video) {
                    // Every picture after this one depends on it, so the
                    // decoder waits for the keyframe asked for here.
                    if !video.keyframe {
                        lost.store(true, Ordering::Relaxed);
                        link.send(Message::Keyframe(WindowRef { window }));
                    }
                    stats.lock().unwrap().entry(window).or_default().skipped += 1;
                }
            }
            Message::WindowClosed(WindowRef { window }) => {
                // Dropping its sender ends the window's decoding thread.
                decoders.remove(&window);
                frames.take(window);
                sink(Event::Message(message));
            }
            other => sink(Event::Message(other)),
        }
    }
}

fn decoder(
    window: u32,
    hardware: bool,
    link: Link,
    frames: Arc<Frames>,
    stats: Stats,
    sink: Sink,
) -> (Sender<Video>, Arc<AtomicBool>) {
    let (tx, rx) = bounded::<Video>(DECODE_QUEUE);
    // Until a keyframe comes there is nothing to decode against.
    let lost = Arc::new(AtomicBool::new(true));
    let waiting = lost.clone();
    thread::Builder::new()
        .name(format!("decode {window}"))
        .spawn(move || {
            let mut decoder = match Decoder::new(hardware) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("app-windows: window {window}: {e}");
                    return;
                }
            };
            stats.lock().unwrap().entry(window).or_default().on_gpu = decoder.on_gpu();
            for video in rx {
                if video.keyframe {
                    waiting.store(false, Ordering::Relaxed);
                } else if waiting.load(Ordering::Relaxed) {
                    stats.lock().unwrap().entry(window).or_default().skipped += 1;
                    continue;
                }
                let started = Instant::now();
                let picture = decoder.decode(&video.data);
                let took = started.elapsed().as_micros() as u64;
                let mut all = stats.lock().unwrap();
                let s = all.entry(window).or_default();
                s.bytes += video.data.len() as u64;
                s.decode_micros += took;
                match picture {
                    Ok(Some(picture)) => {
                        if s.pictures == 0 {
                            s.first_micros = video.micros;
                        }
                        s.pictures += 1;
                        s.keyframes += u64::from(video.keyframe);
                        s.last_micros = video.micros;
                        drop(all);
                        if frames.put(window, picture) {
                            sink(Event::Picture(window));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        s.skipped += 1;
                        drop(all);
                        eprintln!("app-windows: window {window}: {e}; asking for a keyframe");
                        waiting.store(true, Ordering::Relaxed);
                        link.send(Message::Keyframe(WindowRef { window }));
                    }
                }
            }
        })
        .expect("spawning a decoder thread");
    (tx, lost)
}

/// A recording read back at the pace it was made, as if from the guest.
pub struct Replay {
    file: BufReader<File>,
    started: Instant,
    /// The rest of the current message, once its time has come.
    pending: Vec<u8>,
    at: usize,
    speed: f64,
}

impl Replay {
    pub fn open(path: &Path, speed: f64) -> io::Result<Self> {
        Ok(Replay {
            file: BufReader::new(File::open(path)?),
            started: Instant::now(),
            pending: Vec::new(),
            at: 0,
            speed,
        })
    }
}

impl Read for Replay {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at == self.pending.len() {
            let mut header = [0u8; 12];
            self.file.read_exact(&mut header)?;
            let micros = u64::from_le_bytes(header[..8].try_into().unwrap());
            let length = u32::from_le_bytes(header[8..].try_into().unwrap()) as usize;
            if length == 0 || length > protocol::MAX_MESSAGE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not a recording",
                ));
            }
            let due = Duration::from_micros((micros as f64 / self.speed) as u64);
            if let Some(wait) = due.checked_sub(self.started.elapsed()) {
                thread::sleep(wait);
            }
            self.pending.clear();
            self.pending.extend_from_slice(&header[8..]);
            self.pending.resize(4 + length, 0);
            self.file.read_exact(&mut self.pending[4..])?;
            self.at = 0;
        }
        let n = buf.len().min(self.pending.len() - self.at);
        buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_windows_protocol::{WindowInfo, WindowKind};

    #[test]
    fn replays_what_was_recorded() {
        let path = std::env::temp_dir().join(format!("app-windows-replay-{}", std::process::id()));
        let opened = Message::WindowOpened(WindowInfo {
            window: 3,
            app: "com.apple.TextEdit".into(),
            title: "Untitled".into(),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
            scale: 2.0,
            kind: WindowKind::Normal,
            parent: None,
        });
        let closed = Message::WindowClosed(WindowRef { window: 3 });
        let mut file = Vec::new();
        for (at, m) in [(0u64, &opened), (20_000, &closed)] {
            file.extend_from_slice(&at.to_le_bytes());
            file.extend_from_slice(&m.encode());
        }
        std::fs::write(&path, file).unwrap();
        let mut replay = Replay::open(&path, 1.0).unwrap();
        let started = Instant::now();
        assert_eq!(protocol::read(&mut replay).unwrap(), opened);
        assert_eq!(protocol::read(&mut replay).unwrap(), closed);
        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(protocol::read(&mut replay).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
