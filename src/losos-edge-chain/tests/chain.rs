//! The two handshakes against a stand-in for Tor and the edge's exit on one
//! stream, as they meet over a real onion circuit, and the bytes after them.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::thread;

use losos_edge_chain::{open, splice, Edge};

const ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";

fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut b = vec![0; n];
    s.read_exact(&mut b).unwrap();
    b
}

/// Plays a SOCKS5 server's side of a password greeting and a CONNECT, and
/// returns the credentials and the request's address bytes.
fn accept(s: &mut TcpStream) -> (String, String, Vec<u8>) {
    assert_eq!(read_n(s, 3), [5, 1, 2]);
    s.write_all(&[5, 2]).unwrap();
    assert_eq!(read_n(s, 1), [1]);
    let n = read_n(s, 1)[0] as usize;
    let user = String::from_utf8(read_n(s, n)).unwrap();
    let n = read_n(s, 1)[0] as usize;
    let pass = String::from_utf8(read_n(s, n)).unwrap();
    s.write_all(&[1, 0]).unwrap();
    assert_eq!(read_n(s, 3), [5, 1, 0]);
    let atyp = read_n(s, 1)[0];
    let addr = match atyp {
        1 => read_n(s, 4 + 2),
        4 => read_n(s, 16 + 2),
        3 => {
            let n = read_n(s, 1)[0] as usize;
            read_n(s, n + 2)
        }
        _ => panic!("address type {atyp}"),
    };
    s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
    (user, pass, addr)
}

#[test]
fn a_connection_reaches_its_address_through_tor_and_the_edge() {
    let tor = TcpListener::bind("127.0.0.1:0").unwrap();
    let tor_addr = tor.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut s, _) = tor.accept().unwrap();
        // Tor: isolated by the client and the port, asked for the onion.
        let (user, pass, addr) = accept(&mut s);
        assert_eq!((user.as_str(), pass.as_str()), ("10.152.152.11", "443"));
        assert_eq!(addr, [ONION.as_bytes(), &[4, 56]].concat());
        // The edge's exit, at the far end of the circuit: its own
        // credentials, asked for the real address.
        let (user, pass, addr) = accept(&mut s);
        assert_eq!((user.as_str(), pass.as_str()), ("desk-1", "s3cret"));
        assert_eq!(addr, [93, 184, 215, 14, 1, 187]);
        // The site: echoes what it gets, upper-cased.
        let mut got = [0u8; 5];
        s.read_exact(&mut got).unwrap();
        s.write_all(&got.to_ascii_uppercase()).unwrap();
    });

    let edge = Edge {
        onion: ONION.into(),
        port: 1080,
        username: "desk-1".into(),
        password: "s3cret".into(),
    };
    let client: IpAddr = "10.152.152.11".parse().unwrap();
    let dst: SocketAddr = "93.184.215.14:443".parse().unwrap();
    let out = open(tor_addr, &edge, client, dst).unwrap();

    // The app's side of the gateway, spliced to the way out.
    let app = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut app_end = TcpStream::connect(app.local_addr().unwrap()).unwrap();
    let (gateway_end, _) = app.accept().unwrap();
    let carried = thread::spawn(move || splice(&gateway_end, &out));

    app_end.write_all(b"hello").unwrap();
    let mut answer = [0u8; 5];
    app_end.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"HELLO");
    server.join().unwrap();
    drop(app_end);
    carried.join().unwrap();
}

#[test]
fn an_edge_that_refuses_closes_the_connection() {
    let tor = TcpListener::bind("127.0.0.1:0").unwrap();
    let tor_addr = tor.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut s, _) = tor.accept().unwrap();
        accept(&mut s);
        // The edge's exit turns the credentials down.
        assert_eq!(read_n(&mut s, 3), [5, 1, 2]);
        s.write_all(&[5, 2]).unwrap();
        let n = read_n(&mut s, 2)[1] as usize;
        read_n(&mut s, n);
        let n = read_n(&mut s, 1)[0] as usize;
        read_n(&mut s, n);
        s.write_all(&[1, 1]).unwrap();
    });
    let edge = Edge {
        onion: ONION.into(),
        port: 1080,
        username: "old".into(),
        password: "revoked".into(),
    };
    let e = open(
        tor_addr,
        &edge,
        "10.152.152.11".parse().unwrap(),
        "1.1.1.1:443".parse().unwrap(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("refused"), "{e}");
    server.join().unwrap();
}
