//! What `losos-tokens` decides, apart from the sockets and devices it
//! decides it for: the frames it sends machines, the CTAPHID it reads to
//! keep machines' conversations with a key apart, the uhid events a machine
//! makes its copy of a key with, and what the host's sysfs and libvirt say.

use std::collections::{BTreeMap, BTreeSet};

/// The vsock port machines reach the host's security keys on.
pub const KEYS_PORT: u32 = 7370;
/// The vsock port machines reach the host's pcscd on.
pub const SMART_CARDS_PORT: u32 = 7371;

/// The size of every CTAPHID report, in and out, on every FIDO key.
pub const REPORT_LEN: usize = 64;
/// What a machine's copy of a key says it is connected through, in
/// `HID_PHYS`. The host never relays a device that says it, so a host that
/// is itself a machine, or a test that runs both ends on one kernel, does
/// not hand a key back to where it came from.
pub const RELAYED_PHYS: &str = "losos-tokens";

/// One frame between the host and a machine on [`KEYS_PORT`]: a kind, the
/// key it is about, and what it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// The host has key `key`; the machine makes a copy of it.
    Add {
        key: u8,
        vendor: u32,
        product: u32,
        name: String,
    },
    /// Key `key` is gone.
    Remove { key: u8 },
    /// One CTAPHID report: from the key to the machine, or from the machine
    /// to the key.
    Report { key: u8, report: [u8; REPORT_LEN] },
}

const ADD: u8 = 1;
const REMOVE: u8 = 2;
const REPORT: u8 = 3;
/// The longest key name a frame carries, which is uhid's limit too.
const NAME_MAX: usize = 127;

impl Frame {
    /// The frame as bytes: kind, key, payload length (two bytes, little
    /// endian), payload.
    pub fn encode(&self) -> Vec<u8> {
        let (kind, key, payload) = match self {
            Frame::Add {
                key,
                vendor,
                product,
                name,
            } => {
                let mut payload = Vec::with_capacity(8 + name.len());
                payload.extend_from_slice(&vendor.to_le_bytes());
                payload.extend_from_slice(&product.to_le_bytes());
                payload.extend_from_slice(truncate(name, NAME_MAX).as_bytes());
                (ADD, *key, payload)
            }
            Frame::Remove { key } => (REMOVE, *key, Vec::new()),
            Frame::Report { key, report } => (REPORT, *key, report.to_vec()),
        };
        let mut bytes = Vec::with_capacity(4 + payload.len());
        bytes.push(kind);
        bytes.push(key);
        bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    /// Reads one frame, or `None` at the end of the stream. A frame this
    /// does not understand is an error, which ends the connection: both
    /// ends come from the same image.
    pub fn read(from: &mut impl std::io::Read) -> std::io::Result<Option<Frame>> {
        let mut head = [0u8; 4];
        match from.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u16::from_le_bytes([head[2], head[3]]) as usize;
        let mut payload = vec![0u8; len];
        from.read_exact(&mut payload)?;
        Frame::decode(head[0], head[1], &payload)
            .map(Some)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad frame"))
    }

    fn decode(kind: u8, key: u8, payload: &[u8]) -> Option<Frame> {
        match kind {
            ADD if payload.len() >= 8 => Some(Frame::Add {
                key,
                vendor: u32::from_le_bytes(payload[0..4].try_into().ok()?),
                product: u32::from_le_bytes(payload[4..8].try_into().ok()?),
                name: String::from_utf8_lossy(&payload[8..payload.len().min(8 + NAME_MAX)])
                    .into_owned(),
            }),
            REMOVE if payload.is_empty() => Some(Frame::Remove { key }),
            REPORT => Some(Frame::Report {
                key,
                report: payload.try_into().ok()?,
            }),
            _ => None,
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// CTAPHID, as much of it as keeping machines apart needs (CTAP 2.1,
/// section 11.2.4): every report starts with the channel it is on, and a
/// machine gets a channel by sending INIT with a nonce on the broadcast
/// channel, which the key answers there with the same nonce and the new
/// channel.
pub mod ctaphid {
    use super::REPORT_LEN;

    pub const BROADCAST: [u8; 4] = [0xff; 4];
    /// INIT with the bit that marks the first report of a message.
    pub const INIT: u8 = 0x86;

    pub fn channel(report: &[u8; REPORT_LEN]) -> [u8; 4] {
        report[0..4].try_into().unwrap()
    }

    /// The nonce of an INIT request or response: the first eight bytes of
    /// its payload, after channel, command and length.
    pub fn init_nonce(report: &[u8; REPORT_LEN]) -> Option<[u8; 8]> {
        (report[4] == INIT).then(|| report[7..15].try_into().unwrap())
    }

    /// The channel an INIT response gives out, after its nonce.
    pub fn init_channel(report: &[u8; REPORT_LEN]) -> Option<[u8; 4]> {
        let len = u16::from_be_bytes([report[5], report[6]]);
        (report[4] == INIT && len >= 17).then(|| report[15..19].try_into().unwrap())
    }
}

/// Where the host sends each report: to the key, to one machine, or
/// nowhere. A machine sees only the reports on channels it opened, and can
/// write only on those and on the broadcast channel to open one. The host's
/// own programs, such as a login, use the same keys beside the relay
/// through hidraw, and their channels are nobody's here, so a machine never
/// sees them either.
///
/// `M` names a machine's connection. The router holds no sockets, so the
/// rules are tested without any.
#[derive(Debug, Default)]
pub struct Router<M> {
    /// Channels a machine opened, by key.
    channels: BTreeMap<(u8, [u8; 4]), M>,
    /// INIT nonces a machine sent and the key has not answered, by key.
    pending: BTreeMap<(u8, [u8; 8]), M>,
}

impl<M: Clone + Ord> Router<M> {
    pub fn new() -> Self {
        Router {
            channels: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }

    /// Whether `machine` may write `report` to `key`, which it may on a
    /// channel it opened, or with an INIT on the broadcast channel.
    pub fn from_machine(&mut self, machine: &M, key: u8, report: &[u8; REPORT_LEN]) -> bool {
        let channel = ctaphid::channel(report);
        if channel == ctaphid::BROADCAST {
            return match ctaphid::init_nonce(report) {
                Some(nonce) => {
                    self.pending.insert((key, nonce), machine.clone());
                    true
                }
                None => false,
            };
        }
        self.channels.get(&(key, channel)) == Some(machine)
    }

    /// The machine `report` from `key` goes to, if any.
    pub fn from_key(&mut self, key: u8, report: &[u8; REPORT_LEN]) -> Option<M> {
        let channel = ctaphid::channel(report);
        if channel == ctaphid::BROADCAST {
            let nonce = ctaphid::init_nonce(report)?;
            let machine = self.pending.remove(&(key, nonce))?;
            if let Some(opened) = ctaphid::init_channel(report) {
                self.channels.insert((key, opened), machine.clone());
            }
            return Some(machine);
        }
        self.channels.get(&(key, channel)).cloned()
    }

    /// Forgets `key`'s channels, when it is pulled out.
    pub fn key_gone(&mut self, key: u8) {
        self.channels.retain(|(k, _), _| *k != key);
        self.pending.retain(|(k, _), _| *k != key);
    }

    /// Forgets `machine`'s channels, when it hangs up.
    pub fn machine_gone(&mut self, machine: &M) {
        self.channels.retain(|_, m| m != machine);
        self.pending.retain(|_, m| m != machine);
    }
}

/// The smallest key number no key has, out of 256.
pub fn free_key(used: &BTreeSet<u8>) -> Option<u8> {
    (0..=u8::MAX).find(|k| !used.contains(k))
}

/// The FIDO usage page, 0xF1D0, as the first item of a report descriptor
/// declares it. Every FIDO key's HID interface starts this way, which is how
/// libfido2 and systemd's fido_id tell one from a keyboard.
pub fn is_fido_descriptor(descriptor: &[u8]) -> bool {
    descriptor.starts_with(&[0x06, 0xd0, 0xf1])
}

/// A hidraw device's `HID_ID`, `HID_NAME` and `HID_PHYS`, from its HID
/// parent's uevent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HidInfo {
    pub vendor: u32,
    pub product: u32,
    pub name: String,
    pub phys: String,
}

pub fn parse_hid_uevent(uevent: &str) -> HidInfo {
    let mut info = HidInfo::default();
    for line in uevent.lines() {
        if let Some(id) = line.strip_prefix("HID_ID=") {
            // bus:vendor:product, in hex.
            let mut parts = id.split(':').skip(1);
            let mut hex = || {
                parts
                    .next()
                    .and_then(|p| u32::from_str_radix(p, 16).ok())
                    .unwrap_or(0)
            };
            info.vendor = hex();
            info.product = hex();
        } else if let Some(name) = line.strip_prefix("HID_NAME=") {
            info.name = name.to_owned();
        } else if let Some(phys) = line.strip_prefix("HID_PHYS=") {
            info.phys = phys.to_owned();
        }
    }
    info
}

/// uhid, the kernel's way to make a HID device from user space
/// (include/uapi/linux/uhid.h): a machine makes its copy of each key this
/// way, and hidraw, libfido2 and every browser take it for a key on USB.
pub mod uhid {
    /// `sizeof(struct uhid_event)`: a type and the largest request,
    /// CREATE2's.
    pub const EVENT_LEN: usize = 4 + 4372;

    pub const DESTROY: u32 = 1;
    pub const STOP: u32 = 3;
    pub const OUTPUT: u32 = 6;
    pub const GET_REPORT: u32 = 9;
    pub const GET_REPORT_REPLY: u32 = 10;
    pub const CREATE2: u32 = 11;
    pub const INPUT2: u32 = 12;
    pub const SET_REPORT: u32 = 13;
    pub const SET_REPORT_REPLY: u32 = 14;

    const BUS_USB: u16 = 0x03;
    /// EIO, for the report requests a FIDO key never answers.
    const EIO: u16 = 5;

    /// A FIDO key's report descriptor (CTAP 2.1, section 11.2.8.1): one
    /// 64-byte input report and one 64-byte output report, no report IDs.
    pub const FIDO_DESCRIPTOR: [u8; 34] = [
        0x06, 0xd0, 0xf1, // Usage Page (FIDO Alliance)
        0x09, 0x01, // Usage (CTAPHID)
        0xa1, 0x01, // Collection (Application)
        0x09, 0x20, //   Usage (Input Report Data)
        0x15, 0x00, //   Logical Minimum (0)
        0x26, 0xff, 0x00, //   Logical Maximum (255)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x40, //   Report Count (64)
        0x81, 0x02, //   Input (Data, Var, Abs)
        0x09, 0x21, //   Usage (Output Report Data)
        0x15, 0x00, //   Logical Minimum (0)
        0x26, 0xff, 0x00, //   Logical Maximum (255)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x40, //   Report Count (64)
        0x91, 0x02, //   Output (Data, Var, Abs)
        0xc0, // End Collection
    ];

    fn event(kind: u32) -> Vec<u8> {
        let mut event = vec![0u8; EVENT_LEN];
        event[0..4].copy_from_slice(&kind.to_ne_bytes());
        event
    }

    /// CREATE2 for a copy of a key: its name, the relay's mark as its
    /// physical path, its vendor and product, and the FIDO descriptor.
    pub fn create(name: &str, phys: &str, vendor: u32, product: u32) -> Vec<u8> {
        let mut event = event(CREATE2);
        let request = &mut event[4..];
        let name = super::truncate(name, 127).as_bytes();
        request[0..name.len()].copy_from_slice(name);
        let phys = super::truncate(phys, 63).as_bytes();
        request[128..128 + phys.len()].copy_from_slice(phys);
        request[256..258].copy_from_slice(&(FIDO_DESCRIPTOR.len() as u16).to_ne_bytes());
        request[258..260].copy_from_slice(&BUS_USB.to_ne_bytes());
        request[260..264].copy_from_slice(&vendor.to_ne_bytes());
        request[264..268].copy_from_slice(&product.to_ne_bytes());
        request[276..276 + FIDO_DESCRIPTOR.len()].copy_from_slice(&FIDO_DESCRIPTOR);
        event
    }

    pub fn destroy() -> Vec<u8> {
        event(DESTROY)
    }

    /// INPUT2: a report from the key, as the device sends it.
    pub fn input(report: &[u8]) -> Vec<u8> {
        let mut event = event(INPUT2);
        event[4..6].copy_from_slice(&(report.len() as u16).to_ne_bytes());
        event[6..6 + report.len()].copy_from_slice(report);
        event
    }

    /// The reply to a GET_REPORT or SET_REPORT request, refusing it: a FIDO
    /// key has no feature reports, and hidraw would wait five seconds for
    /// an answer that never comes.
    pub fn refuse(request: &[u8]) -> Option<Vec<u8>> {
        let kind = u32::from_ne_bytes(request[0..4].try_into().ok()?);
        let reply = match kind {
            GET_REPORT => GET_REPORT_REPLY,
            SET_REPORT => SET_REPORT_REPLY,
            _ => return None,
        };
        let mut event = event(reply);
        // The request's id, then the error.
        event[4..8].copy_from_slice(&request[4..8]);
        event[8..10].copy_from_slice(&EIO.to_ne_bytes());
        Some(event)
    }

    /// The report in an OUTPUT event: what a program wrote to hidraw, less
    /// the report number hidraw puts first, which is 0 on a device without
    /// report IDs.
    pub fn output(event: &[u8]) -> Option<[u8; super::REPORT_LEN]> {
        if u32::from_ne_bytes(event[0..4].try_into().ok()?) != OUTPUT {
            return None;
        }
        let size = u16::from_ne_bytes(event[4 + 4096..4 + 4098].try_into().ok()?) as usize;
        let data = &event[4..4 + size.min(4096)];
        let data = match data.len() {
            65 => &data[1..],
            _ => data,
        };
        let mut report = [0u8; super::REPORT_LEN];
        let n = data.len().min(super::REPORT_LEN);
        report[..n].copy_from_slice(&data[..n]);
        Some(report)
    }

    pub fn kind(event: &[u8]) -> Option<u32> {
        Some(u32::from_ne_bytes(event.get(0..4)?.try_into().ok()?))
    }
}

/// The vsock address in a libvirt domain's XML, which is the machine's for
/// as long as it runs.
pub fn domain_cid(xml: &str) -> Option<u32> {
    let start = xml.find("<cid ")? + "<cid ".len();
    let rest = &xml[start..];
    let rest = &rest[..rest.find('>')?];
    let value = &rest[rest.find("address=")? + "address=".len()..];
    let quote = value.chars().next().filter(|q| *q == '\'' || *q == '"')?;
    let value = &value[1..];
    value[..value.find(quote)?].parse().ok()
}

/// Which machine holds the smart card readers. One machine at a time: a
/// card keeps what a PIN unlocked until it is reset, so a second machine
/// sharing the reader could use a card the first one had unlocked. The
/// machine holding them keeps them while it has a connection open, and the
/// next to ask gets them once it has none.
#[derive(Debug, Default)]
pub struct Lease {
    holder: Option<(String, usize)>,
}

impl Lease {
    /// Takes a connection for `machine`, if the readers are free or it holds
    /// them already.
    pub fn take(&mut self, machine: &str) -> bool {
        match &mut self.holder {
            Some((holder, count)) if holder == machine => {
                *count += 1;
                true
            }
            Some(_) => false,
            None => {
                self.holder = Some((machine.to_owned(), 1));
                true
            }
        }
    }

    /// Gives back one of `machine`'s connections.
    pub fn give_back(&mut self, machine: &str) {
        if let Some((holder, count)) = &mut self.holder {
            if holder == machine {
                *count -= 1;
                if *count == 0 {
                    self.holder = None;
                }
            }
        }
    }

    pub fn holder(&self) -> Option<&str> {
        self.holder.as_ref().map(|(m, _)| m.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(channel: [u8; 4], cmd: u8, payload: &[u8]) -> [u8; REPORT_LEN] {
        let mut r = [0u8; REPORT_LEN];
        r[0..4].copy_from_slice(&channel);
        r[4] = cmd;
        r[5..7].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        r[7..7 + payload.len()].copy_from_slice(payload);
        r
    }

    #[test]
    fn frames_round_trip() {
        let frames = [
            Frame::Add {
                key: 3,
                vendor: 0x1050,
                product: 0x0407,
                name: "Yubico YubiKey OTP+FIDO+CCID".into(),
            },
            Frame::Remove { key: 3 },
            Frame::Report {
                key: 0,
                report: [7; REPORT_LEN],
            },
        ];
        let mut bytes = Vec::new();
        for f in &frames {
            bytes.extend(f.encode());
        }
        let mut reader = bytes.as_slice();
        for f in &frames {
            assert_eq!(Frame::read(&mut reader).unwrap().as_ref(), Some(f));
        }
        assert_eq!(Frame::read(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_short_report_is_not_a_frame() {
        let mut bytes = Frame::Report {
            key: 0,
            report: [0; REPORT_LEN],
        }
        .encode();
        bytes[2] = 10;
        bytes.truncate(14);
        assert!(Frame::read(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn long_names_are_cut_on_a_character() {
        let name = "é".repeat(100);
        let Frame::Add { name: back, .. } = Frame::read(
            &mut Frame::Add {
                key: 0,
                vendor: 0,
                product: 0,
                name,
            }
            .encode()
            .as_slice(),
        )
        .unwrap()
        .unwrap() else {
            panic!()
        };
        assert_eq!(back, "é".repeat(63));
    }

    #[test]
    fn machines_see_only_their_own_channels() {
        let mut router = Router::new();
        let nonce_a = [1, 2, 3, 4, 5, 6, 7, 8];
        let nonce_b = [8, 7, 6, 5, 4, 3, 2, 1];
        assert!(router.from_machine(&"work", 0, &report(ctaphid::BROADCAST, 0x86, &nonce_a)));
        assert!(router.from_machine(&"play", 0, &report(ctaphid::BROADCAST, 0x86, &nonce_b)));

        let answer = |nonce: [u8; 8], channel: [u8; 4]| {
            let mut payload = nonce.to_vec();
            payload.extend_from_slice(&channel);
            payload.extend_from_slice(&[2, 5, 4, 0, 0x05]);
            report(ctaphid::BROADCAST, 0x86, &payload)
        };
        // The key answers play first; each answer goes to whoever asked.
        assert_eq!(
            router.from_key(0, &answer(nonce_b, [0, 0, 0, 2])),
            Some("play")
        );
        assert_eq!(
            router.from_key(0, &answer(nonce_a, [0, 0, 0, 1])),
            Some("work")
        );
        // An answer nobody asked for, such as one to the host's own login.
        assert_eq!(router.from_key(0, &answer([9; 8], [0, 0, 0, 3])), None);

        let msg = |channel| report(channel, 0x90, &[0x04]);
        assert_eq!(router.from_key(0, &msg([0, 0, 0, 1])), Some("work"));
        assert_eq!(router.from_key(0, &msg([0, 0, 0, 2])), Some("play"));
        assert_eq!(router.from_key(0, &msg([0, 0, 0, 3])), None);
        // The same channel number on another key is another channel.
        assert_eq!(router.from_key(1, &msg([0, 0, 0, 1])), None);

        // Nobody writes on another's channel, or anything but INIT on the
        // broadcast one.
        assert!(router.from_machine(&"work", 0, &msg([0, 0, 0, 1])));
        assert!(!router.from_machine(&"work", 0, &msg([0, 0, 0, 2])));
        assert!(!router.from_machine(&"work", 0, &msg([0, 0, 0, 3])));
        assert!(!router.from_machine(&"work", 0, &report(ctaphid::BROADCAST, 0x90, &[4])));

        router.machine_gone(&"work");
        assert_eq!(router.from_key(0, &msg([0, 0, 0, 1])), None);
        assert_eq!(router.from_key(0, &msg([0, 0, 0, 2])), Some("play"));
        router.key_gone(0);
        assert_eq!(router.from_key(0, &msg([0, 0, 0, 2])), None);
    }

    #[test]
    fn keys_take_the_smallest_free_number() {
        assert_eq!(free_key(&BTreeSet::new()), Some(0));
        assert_eq!(free_key(&[0, 1, 3].into()), Some(2));
        assert_eq!(free_key(&(0..=u8::MAX).collect()), None);
    }

    #[test]
    fn fido_keys_are_told_by_their_descriptor() {
        assert!(is_fido_descriptor(&uhid::FIDO_DESCRIPTOR));
        // A keyboard's: Generic Desktop, Keyboard.
        assert!(!is_fido_descriptor(&[0x05, 0x01, 0x09, 0x06, 0xa1, 0x01]));
    }

    #[test]
    fn hid_uevents() {
        let info = parse_hid_uevent(
            "DRIVER=hid-generic\nHID_ID=0003:00001050:00000407\n\
             HID_NAME=Yubico YubiKey OTP+FIDO+CCID\nHID_PHYS=usb-0000:00:14.0-1/input1\n",
        );
        assert_eq!(
            info,
            HidInfo {
                vendor: 0x1050,
                product: 0x0407,
                name: "Yubico YubiKey OTP+FIDO+CCID".into(),
                phys: "usb-0000:00:14.0-1/input1".into(),
            }
        );
    }

    #[test]
    fn uhid_events_have_the_kernels_layout() {
        let create = uhid::create("Key", RELAYED_PHYS, 0x1050, 0x0407);
        assert_eq!(create.len(), uhid::EVENT_LEN);
        assert_eq!(uhid::kind(&create), Some(uhid::CREATE2));
        assert_eq!(&create[4..7], b"Key");
        assert_eq!(
            &create[132..132 + RELAYED_PHYS.len()],
            RELAYED_PHYS.as_bytes()
        );
        assert_eq!(u16::from_ne_bytes([create[260], create[261]]), 34);
        assert_eq!(u16::from_ne_bytes([create[262], create[263]]), 3);
        assert_eq!(&create[280..314], &uhid::FIDO_DESCRIPTOR);

        let input = uhid::input(&[9; REPORT_LEN]);
        assert_eq!(u16::from_ne_bytes([input[4], input[5]]), 64);
        assert_eq!(&input[6..70], &[9; REPORT_LEN]);

        // hidraw's write of 65 bytes, report number first.
        let mut output = vec![0u8; uhid::EVENT_LEN];
        output[0..4].copy_from_slice(&uhid::OUTPUT.to_ne_bytes());
        output[4] = 0;
        output[5..69].copy_from_slice(&[5; REPORT_LEN]);
        output[4100..4102].copy_from_slice(&65u16.to_ne_bytes());
        assert_eq!(uhid::output(&output), Some([5; REPORT_LEN]));

        let mut get = vec![0u8; uhid::EVENT_LEN];
        get[0..4].copy_from_slice(&uhid::GET_REPORT.to_ne_bytes());
        get[4..8].copy_from_slice(&42u32.to_ne_bytes());
        let reply = uhid::refuse(&get).unwrap();
        assert_eq!(uhid::kind(&reply), Some(uhid::GET_REPORT_REPLY));
        assert_eq!(&reply[4..8], &42u32.to_ne_bytes());
        assert_eq!(uhid::refuse(&output), None);
    }

    #[test]
    fn domain_cids() {
        assert_eq!(
            domain_cid("<vsock model='virtio'>\n  <cid auto='yes' address='5'/>\n</vsock>"),
            Some(5)
        );
        assert_eq!(domain_cid("<domain/>"), None);
    }

    #[test]
    fn one_machine_holds_the_readers() {
        let mut lease = Lease::default();
        assert!(lease.take("work"));
        assert!(lease.take("work"));
        assert!(!lease.take("play"));
        lease.give_back("work");
        assert!(!lease.take("play"));
        lease.give_back("play");
        assert_eq!(lease.holder(), Some("work"));
        lease.give_back("work");
        assert_eq!(lease.holder(), None);
        assert!(lease.take("play"));
    }
}
