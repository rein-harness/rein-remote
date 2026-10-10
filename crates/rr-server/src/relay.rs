//! `rein-remote relay`: for a machine you can't reach directly (behind NAT, a firewall, no Tailscale).
//! The machine connects out to the relay (`serve --relay`), a viewer opens the relay's page with the
//! room's code, and the relay pairs them and passes their messages along. Self-hosted: run it
//! anywhere both can reach (a small VPS), behind HTTPS for wss://. It sees the stream as it passes
//! (TLS protects it on the way, not from the relay itself); end-to-end encryption is on the roadmap.

use crate::serve::{CLIENT_JS, INDEX};
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::{Message as Ws, WebSocket};

type Rooms = Arc<Mutex<HashMap<String, WebSocket<TcpStream>>>>;

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
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (rooms, secret) = (rooms.clone(), secret.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, rooms, secret) {
                eprintln!("rein-remote relay: {e}");
            }
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, rooms: Rooms, secret: Option<String>) -> Result<(), String> {
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
        pipe(host, viewer);
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
