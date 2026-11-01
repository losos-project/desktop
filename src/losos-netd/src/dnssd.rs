//! A DNS-SD browse in one mDNS query, for the edges on the networks this
//! machine joins (docs/network-machine.md, "Through an edge"). LosOS boxes
//! ask resolved for `_losos-edge._tcp`; this machine has no resolved, so it
//! asks the network itself: one question for the service's PTR, sent from a
//! port of its own so every responder answers it directly (RFC 6762, 6.7),
//! and every answer's records read into the adverts the path rule takes.
//!
//! Only the packets are here, and tested as such; the sockets are the
//! daemon's.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use losos_edge_path::Advert;

/// The service LosOS edges advertise.
pub const SERVICE: &str = "_losos-edge._tcp.local";

const PTR: u16 = 12;
const TXT: u16 = 16;
const SRV: u16 = 33;
const A: u16 = 1;
const AAAA: u16 = 28;

/// The question: PTR for [`SERVICE`], class IN with the unicast-response
/// bit, which a query from a port other than 5353 implies anyway.
#[must_use]
pub fn query() -> Vec<u8> {
    let mut packet = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in SERVICE.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&PTR.to_be_bytes());
    packet.extend_from_slice(&0x8001u16.to_be_bytes());
    packet
}

/// What the answers said so far, across every packet of one browse.
#[derive(Debug, Default)]
pub struct Answers {
    instances: Vec<String>,
    srv: BTreeMap<String, (String, u16)>,
    txt: BTreeMap<String, Vec<String>>,
    hosts: BTreeMap<String, Vec<IpAddr>>,
}

impl Answers {
    /// Reads one response's records, from every section; anything that is
    /// not a well-formed response is ignored.
    pub fn read(&mut self, packet: &[u8]) {
        let _ = self.try_read(packet);
    }

    fn try_read(&mut self, packet: &[u8]) -> Option<()> {
        let header = packet.get(..12)?;
        // A response, not someone's question.
        if header[2] & 0x80 == 0 {
            return None;
        }
        let count = |at: usize| usize::from(u16::from_be_bytes([header[at], header[at + 1]]));
        let questions = count(4);
        let records = count(6) + count(8) + count(10);
        let mut at = 12;
        for _ in 0..questions {
            let (_, next) = name(packet, at)?;
            at = next + 4;
        }
        for _ in 0..records {
            let (owner, next) = name(packet, at)?;
            let fixed = packet.get(next..next + 10)?;
            let kind = u16::from_be_bytes([fixed[0], fixed[1]]);
            let length = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
            let start = next + 10;
            let data = packet.get(start..start + length)?;
            let owner = owner.to_ascii_lowercase();
            match kind {
                PTR if owner == SERVICE => {
                    let (instance, _) = name(packet, start)?;
                    if !self.instances.contains(&instance) {
                        self.instances.push(instance);
                    }
                }
                SRV if data.len() > 6 => {
                    let port = u16::from_be_bytes([data[4], data[5]]);
                    let (target, _) = name(packet, start + 6)?;
                    self.srv.insert(owner, (target.to_ascii_lowercase(), port));
                }
                TXT => {
                    let mut strings = Vec::new();
                    let mut i = 0;
                    while let Some(&len) = data.get(i) {
                        let s = data.get(i + 1..i + 1 + usize::from(len))?;
                        strings.push(String::from_utf8_lossy(s).into_owned());
                        i += 1 + usize::from(len);
                    }
                    self.txt.insert(owner, strings);
                }
                A if length == 4 => {
                    let ip = IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                    self.hosts.entry(owner).or_default().push(ip);
                }
                AAAA if length == 16 => {
                    let bytes: [u8; 16] = data.try_into().ok()?;
                    self.hosts
                        .entry(owner)
                        .or_default()
                        .push(IpAddr::V6(Ipv6Addr::from(bytes)));
                }
                _ => {}
            }
            at = start + length;
        }
        Some(())
    }

    /// One advert per instance whose SRV and host address arrived, IPv4
    /// first, as lososd's browse hands them to the path rule.
    #[must_use]
    pub fn adverts(&self) -> Vec<Advert> {
        self.instances
            .iter()
            .filter_map(|instance| {
                let key = instance.to_ascii_lowercase();
                let (host, port) = self.srv.get(&key)?;
                let addresses = self.hosts.get(host)?;
                let address = addresses
                    .iter()
                    .find(|a| a.is_ipv4())
                    .or_else(|| addresses.first())?;
                let txt = self.txt.get(&key);
                let record = |k: &str| {
                    txt?.iter()
                        .find_map(|s| s.strip_prefix(k)?.strip_prefix('=').map(str::to_owned))
                };
                let name = instance
                    .strip_suffix(&format!(".{SERVICE}"))
                    .unwrap_or(instance)
                    .to_owned();
                Some(Advert {
                    name,
                    host: host.trim_end_matches('.').to_owned(),
                    address: address.to_string(),
                    port: *port,
                    url: record("url"),
                    tunnel: record("tunnel"),
                })
            })
            .collect()
    }
}

/// A name at `at`, following compression pointers, and where the record
/// goes on after it. Labels are joined with dots, without a final one.
fn name(packet: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    // A pointer only ever goes back, but a hostile packet could loop.
    for _ in 0..64 {
        let len = *packet.get(at)?;
        match len {
            0 => {
                return Some((labels.join("."), end.unwrap_or(at + 1)));
            }
            l if l & 0xc0 == 0xc0 => {
                let low = *packet.get(at + 1)?;
                end.get_or_insert(at + 2);
                at = usize::from(u16::from_be_bytes([l & 0x3f, low]));
            }
            l if l < 64 => {
                let label = packet.get(at + 1..at + 1 + usize::from(l))?;
                labels.push(String::from_utf8_lossy(label).into_owned());
                at += 1 + usize::from(l);
            }
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(out: &mut Vec<u8>, name: &str) {
        for label in name.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
    }

    fn record(out: &mut Vec<u8>, owner: &[u8], kind: u16, data: &[u8]) {
        out.extend_from_slice(owner);
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&[0x80, 1, 0, 0, 0, 120]);
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }

    /// What an edge's resolved answers: the PTR, then the SRV, TXT and A
    /// as additional records, the instance by a compression pointer.
    fn response() -> Vec<u8> {
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        let service_at = p.len();
        let mut service = Vec::new();
        labels(&mut service, SERVICE);
        let mut instance = vec![4];
        instance.extend_from_slice(b"Gate");
        instance.extend_from_slice(&[0xc0, service_at as u8]);
        // The PTR's owner is written out, the rest point back at it.
        p.extend_from_slice(&service);
        p.extend_from_slice(&PTR.to_be_bytes());
        p.extend_from_slice(&[0, 1, 0, 0, 0, 120]);
        p.extend_from_slice(&(instance.len() as u16).to_be_bytes());
        let instance_at = p.len();
        p.extend_from_slice(&instance);
        let owner = [0xc0, instance_at as u8];
        let mut srv = vec![0, 0, 0, 0, 0x20, 0xfb];
        labels(&mut srv, "gate.local");
        record(&mut p, &owner, SRV, &srv);
        let mut txt = Vec::new();
        for s in ["url=https://gate.example", "tunnel=gate.example:7843"] {
            txt.push(s.len() as u8);
            txt.extend_from_slice(s.as_bytes());
        }
        record(&mut p, &owner, TXT, &txt);
        let mut host = Vec::new();
        labels(&mut host, "gate.local");
        record(&mut p, &host, A, &[192, 168, 1, 7]);
        p
    }

    #[test]
    fn asks_for_the_service() {
        let q = query();
        assert_eq!(&q[4..6], &[0, 1]);
        assert_eq!(&q[13..24], b"_losos-edge");
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0x80, 1]);
    }

    #[test]
    fn reads_an_edge_out_of_its_answer() {
        let mut answers = Answers::default();
        answers.read(&response());
        let adverts = answers.adverts();
        assert_eq!(adverts.len(), 1);
        let a = &adverts[0];
        assert_eq!(a.name, "Gate");
        assert_eq!(a.host, "gate.local");
        assert_eq!(a.address, "192.168.1.7");
        assert_eq!(a.port, 8443);
        assert_eq!(a.url.as_deref(), Some("https://gate.example"));
        assert_eq!(a.tunnel.as_deref(), Some("gate.example:7843"));
    }

    #[test]
    fn ignores_questions_and_garbage() {
        let mut answers = Answers::default();
        answers.read(&query());
        answers.read(&[0xff; 40]);
        let mut cut = response();
        cut.truncate(cut.len() - 3);
        answers.read(&cut);
        assert!(answers.adverts().is_empty());
    }

    #[test]
    fn a_pointer_loop_ends() {
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        p.extend_from_slice(&[0xc0, 12]);
        assert!(name(&p, 12).is_none());
    }
}
