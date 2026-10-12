//! `rein-remote relay`: for a machine you can't reach directly (behind NAT, a firewall, no Tailscale).
//! The machine connects out to the relay (`serve --relay`), a viewer opens the relay's page with the
//! room's code, and the relay pairs them and passes their messages along. Self-hosted: run it
//! anywhere both can reach (a small VPS), behind HTTPS for wss://. What it passes is sealed end to end
//! (the key is in the viewer's link after #, which it never sees).
//!
//! It also answers on UDP, on the same port: "what address do you see me from?" (rr-proto's `punch`),
//! for a room it has open. That's how a host and a native viewer learn their addresses beyond their
//! NATs and go straight to each other (rr-client's `direct`); the relay never sees what they say.

use crate::serve::{CLIENT_JS, INDEX};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::{Message as Ws, WebSocket};

type Rooms = Arc<Mutex<HashMap<String, WebSocket<TcpStream>>>>;
/// Rooms with a viewer in them now (how many sessions each), apart from the ones waiting.
type Busy = Arc<Mutex<HashMap<String, usize>>>;

fn query<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    target.split_once('?')?.1.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Serve the relay on `listen`. `secret`: what a machine must give to host a room (else anyone who
/// can reach the relay could put a screen in one).
pub fn run(listen: &str, secret: Option<String>) -> Result<(), String> {
    let listener = TcpListener::bind(listen).map_err(|e| format!("can't listen on {listen}: {e}"))?;
    eprintln!(
        "rein-remote relay: on http://{}{}",
        listener.local_addr().map_or(listen.to_string(), |a| a.to_string()),
        if secret.is_some() {
            ""
        } else {
            " (no --secret: anyone who can reach it can host a room)"
        }
    );
    let rooms: Rooms = Arc::default();
    let busy: Busy = Arc::default();
    match listener.local_addr().and_then(std::net::UdpSocket::bind) {
        Ok(udp) => {
            let (rooms, busy) = (rooms.clone(), busy.clone());
            std::thread::spawn(move || reflect(udp, rooms, busy));
        }
        Err(e) => eprintln!("rein-remote relay: no UDP on its port ({e}): sessions stay relayed, never direct"),
    }
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (rooms, busy, secret) = (rooms.clone(), busy.clone(), secret.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, rooms, busy, secret) {
                eprintln!("rein-remote relay: {e}");
            }
        });
    }
    Ok(())
}

/// Answers probes: the address each came from, for a room that's open (waiting, or with a viewer in
/// it). The answer is never bigger than the probe, so the relay can't be used to amplify.
fn reflect(udp: std::net::UdpSocket, rooms: Rooms, busy: Busy) {
    use rr_proto::punch::{room_hash, Packet};
    let mut buf = [0u8; 512];
    loop {
        let Ok((n, from)) = udp.recv_from(&mut buf) else { continue };
        let Some(Packet::Probe { id, room_hash: asked }) = Packet::decode(&buf[..n]) else {
            continue;
        };
        if n < rr_proto::punch::PROBE_LEN {
            continue;
        }
        let open = {
            let (r, b) = (rooms.lock().unwrap(), busy.lock().unwrap());
            r.keys().chain(b.keys()).any(|room| rr_proto::pair::confirm_eq(&room_hash(room), &asked))
        };
        if open {
            let _ = udp.send_to(&Packet::Seen { id, addr: from }.encode(), from);
        }
    }
}

fn handle(mut stream: TcpStream, rooms: Rooms, busy: Busy, secret: Option<String>) -> Result<(), String> {
    let mut head = [0u8; 2048];
    let n = stream.peek(&mut head).map_err(|e| e.to_string())?;
    let req = String::from_utf8_lossy(&head[..n]).to_string();
    let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let upgrade = req.to_ascii_lowercase().contains("upgrade: websocket");
    let room = query(&target, "room").unwrap_or("").to_string();
    let refuse = |mut s: TcpStream, status: &str| {
        let _ = s.read(&mut [0u8; 2048]);
        let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        Ok(())
    };
    if upgrade && (path == "/host" || path == "/stream") && room.len() < 16 {
        return refuse(stream, "400 Bad Request");
    }
    if upgrade && path == "/host" {
        if let Some(s) = &secret {
            if !same(query(&target, "secret").unwrap_or(""), s) {
                return refuse(stream, "403 Forbidden");
            }
        }
        let ws = tungstenite::accept(stream).map_err(|e| e.to_string())?;
        eprintln!("rein-remote relay: a machine is waiting in room {}…", &room[..6]);
        rooms.lock().unwrap().insert(room, ws);
        return Ok(());
    }
    if upgrade && path == "/stream" {
        // The machine may be reconnecting after the last viewer: wait for it a little.
        let deadline = Instant::now() + Duration::from_secs(10);
        let host = loop {
            if let Some(h) = rooms.lock().unwrap().remove(&room) {
                break h;
            }
            if Instant::now() > deadline {
                return refuse(stream, "404 Not Found");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let viewer = tungstenite::accept(stream).map_err(|e| e.to_string())?;
        eprintln!("rein-remote relay: a viewer joined room {}", &room[..6]);
        *busy.lock().unwrap().entry(room.clone()).or_default() += 1;
        pipe(host, viewer);
        let mut b = busy.lock().unwrap();
        if let Some(n) = b.get_mut(&room) {
            *n -= 1;
            if *n == 0 {
                b.remove(&room);
            }
        }
        return Ok(());
    }
    // The web client, as the machine itself serves it (it reads the room from the page's address).
    let _ = stream.read(&mut head);
    let (status, kind, body) = match path.as_str() {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", INDEX.to_string()),
        "/rein-remote.js" => ("200 OK", "text/javascript; charset=utf-8", CLIENT_JS.to_string()),
        // No WebTransport through the relay (yet): the page uses its WebSocket.
        "/info" => ("200 OK", "application/json", r#"{"webtransport":null}"#.to_string()),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).map_err(|e| e.to_string())
}

/// Pass messages both ways until either side goes.
fn pipe(mut a: WebSocket<TcpStream>, mut b: WebSocket<TcpStream>) {
    for ws in [&mut a, &mut b] {
        let _ = ws.get_mut().set_nonblocking(true);
        let _ = ws.get_mut().set_nodelay(true);
    }
    // Moves what one side sent to the other: Some(true) if anything moved, None when it closed.
    fn step(from: &mut WebSocket<TcpStream>, to: &mut WebSocket<TcpStream>) -> Option<bool> {
        let mut moved = false;
        loop {
            match from.read() {
                Ok(m @ (Ws::Binary(_) | Ws::Text(_))) => {
                    moved = true;
                    match to.write(m) {
                        Ok(()) => {}
                        Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(_) => return None,
                    }
                }
                Ok(Ws::Close(_)) => return None,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => return None,
            }
        }
        match to.flush() {
            Ok(()) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return None,
        }
        Some(moved)
    }
    while let (Some(x), Some(y)) = (step(&mut a, &mut b), step(&mut b, &mut a)) {
        if !x && !y {
            // Nothing moving: wait for the machine's next frame (it sends most), at most 2 ms.
            let _ = a.get_mut().set_nonblocking(false);
            let _ = a.get_mut().set_read_timeout(Some(Duration::from_millis(2)));
            let _ = a.get_mut().peek(&mut [0u8; 1]);
            let _ = a.get_mut().set_nonblocking(true);
        }
    }
    let _ = a.close(None);
    let _ = b.close(None);
    eprintln!("rein-remote relay: a viewer left");
}
