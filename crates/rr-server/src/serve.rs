//! `rein-remote serve`: one port for the web client (GET /) and the stream (a WebSocket on /stream).
//! Phase 1: WebSocket transport (tier 3, works through any HTTPS path), software H.264, keyframe-and-
//! skip when the connection backs up, and latency measured from the client's feedback.

use crate::capture::Source;
use crate::encode::H264;
use rr_proto::{Codec, CodecCap, Config, Hello, Message, Video, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH, VERSION_MAX, VERSION_MIN};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::{Message as Ws, WebSocket};

const INDEX: &str = include_str!("../../../clients/web/index.html");
const CLIENT_JS: &str = include_str!("../../../clients/web/rein-remote.js");

pub struct Options {
    pub listen: String,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// Latency and throughput, kept per session and shown by /stats.
#[derive(Default, Clone)]
pub struct Stats {
    pub frames: u64,
    pub keyframes: u64,
    pub skipped_backed_up: u64,
    pub bytes: u64,
    pub rtt_ms: f64,
    /// Capture to decoded on the client, estimated: feedback arrival minus capture, less half the RTT.
    pub latency_ms: Vec<f64>,
    pub encode_ms: Vec<f64>,
    /// Reading the screen, apart from encoding it.
    pub grab_ms: Vec<f64>,
}

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

pub fn run(source: Box<dyn Source>, opts: Options) -> Result<(), String> {
    let listener = TcpListener::bind(&opts.listen).map_err(|e| format!("can't listen on {}: {e}", opts.listen))?;
    let (w, h) = source.size();
    eprintln!("rein-remote: streaming a {w}×{h} screen on http://{} (open it in a browser)", opts.listen);
    let source = Arc::new(Mutex::new(source));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let opts = Arc::new(opts);
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (source, stats, opts) = (source.clone(), stats.clone(), opts.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, source, stats, &opts) {
                eprintln!("rein-remote: {e}");
            }
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, source: Arc<Mutex<Box<dyn Source>>>, stats: Arc<Mutex<Stats>>, opts: &Options) -> Result<(), String> {
    // Look at the request line without consuming it (the WebSocket handshake needs all of it).
    let mut head = [0u8; 2048];
    let n = stream.peek(&mut head).map_err(|e| e.to_string())?;
    let req = String::from_utf8_lossy(&head[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let upgrade = req.to_ascii_lowercase().contains("upgrade: websocket");
    if path.starts_with("/stream") && upgrade {
        let ws = tungstenite::accept(stream).map_err(|e| format!("websocket: {e}"))?;
        return session(ws, source, stats, opts);
    }
    // Plain HTTP: read the request, answer with a file.
    let _ = stream.read(&mut head);
    let (status, kind, body) = match path.as_str() {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", INDEX.to_string()),
        "/rein-remote.js" => ("200 OK", "text/javascript; charset=utf-8", CLIENT_JS.to_string()),
        "/stats" => ("200 OK", "application/json", stats_json(&stats.lock().unwrap())),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).map_err(|e| e.to_string())
}

fn stats_json(s: &Stats) -> String {
    let pct = |v: &[f64], p: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        let mut v = v.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((v.len() - 1) as f64 * p) as usize]
    };
    format!(
        r#"{{"frames":{},"keyframes":{},"skipped_backed_up":{},"bytes":{},"rtt_ms":{:.1},"latency_ms":{{"p50":{:.1},"p95":{:.1},"samples":{}}},"encode_ms":{{"p50":{:.1},"p95":{:.1}}},"grab_ms":{{"p50":{:.1},"p95":{:.1}}}}}"#,
        s.frames,
        s.keyframes,
        s.skipped_backed_up,
        s.bytes,
        s.rtt_ms,
        pct(&s.latency_ms, 0.5),
        pct(&s.latency_ms, 0.95),
        s.latency_ms.len(),
        pct(&s.encode_ms, 0.5),
        pct(&s.encode_ms, 0.95),
        pct(&s.grab_ms, 0.5),
        pct(&s.grab_ms, 0.95),
    )
}

fn send(ws: &mut WebSocket<TcpStream>, m: &Message) -> Result<(), String> {
    match ws.write(Ws::Binary(m.encode().into())) {
        Ok(()) => Ok(()),
        Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(()), // buffered
        Err(e) => Err(e.to_string()),
    }
}

/// Try to push out what's buffered: false when the connection is backed up.
fn flushed(ws: &mut WebSocket<TcpStream>) -> Result<bool, String> {
    match ws.flush() {
        Ok(()) => Ok(true),
        Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

fn session(mut ws: WebSocket<TcpStream>, source: Arc<Mutex<Box<dyn Source>>>, stats: Arc<Mutex<Stats>>, opts: &Options) -> Result<(), String> {
    let (w, h) = source.lock().unwrap().size();
    let hello = Hello {
        version_min: VERSION_MIN,
        version_max: VERSION_MAX,
        name: std::env::var("HOSTNAME").unwrap_or_else(|_| "rein-remote".into()),
        codecs: vec![CodecCap {
            codec: Codec::H264,
            hardware: false,
        }],
        max_width: w as u16,
        max_height: h as u16,
        max_fps: opts.fps as u16,
        audio: false,
        input: INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH,
    };
    send(&mut ws, &Message::Hello(hello.clone()))?;
    ws.flush().map_err(|e| e.to_string())?;
    // The client's Hello (blocking, with a deadline), then the stream's config.
    ws.get_mut().set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let client = loop {
        match ws.read().map_err(|e| e.to_string())? {
            Ws::Binary(b) => {
                if let Ok((Some(Message::Hello(c)), _)) = Message::decode(&b) {
                    break c;
                }
            }
            Ws::Close(_) => return Ok(()),
            _ => {}
        }
    };
    if rr_proto::negotiate(&hello, &client).is_none() {
        return Err(format!("{}: no protocol version in common", client.name));
    }
    let codec = rr_proto::pick_codec(&hello, &client).ok_or_else(|| format!("{}: decodes none of this server's codecs", client.name))?;
    let mut enc = H264::new(w, h, opts.fps as f32, opts.bitrate_kbps)?;
    let (ew, eh) = enc.size();
    send(
        &mut ws,
        &Message::Config(Config {
            codec,
            width: ew as u16,
            height: eh as u16,
            fps: opts.fps as u16,
            bitrate_kbps: opts.bitrate_kbps,
        }),
    )?;
    ws.get_mut().set_nonblocking(true).map_err(|e| e.to_string())?;
    eprintln!("rein-remote: {} connected ({:?} {ew}×{eh} at {} fps)", client.name, codec, opts.fps);

    let frame_time = Duration::from_secs_f64(1.0 / opts.fps.max(1) as f64);
    let mut frame_no: u32 = 0;
    let mut captured: std::collections::VecDeque<(u32, u64)> = Default::default();
    let mut backed_up = false;
    let mut last_ping = Instant::now() - Duration::from_secs(10);
    let mut pings: std::collections::HashMap<u32, Instant> = Default::default();
    let mut last_report = Instant::now();
    loop {
        let tick = Instant::now();
        // Everything the client sent since the last frame.
        loop {
            match ws.read() {
                Ok(Ws::Binary(b)) => {
                    let mut at = 0;
                    while at < b.len() {
                        let Ok((m, used)) = Message::decode(&b[at..]) else { break };
                        at += used;
                        match m {
                            Some(Message::Input(ev)) => {
                                let _ = source.lock().unwrap().input(&ev);
                            }
                            Some(Message::KeyframeRequest) => enc.keyframe(),
                            Some(Message::Feedback(f)) => {
                                if let Some(&(_, at_us)) = captured.iter().find(|(n, _)| *n == f.frame) {
                                    let mut s = stats.lock().unwrap();
                                    let l = (now_us().saturating_sub(at_us)) as f64 / 1000.0 - s.rtt_ms / 2.0;
                                    s.latency_ms.push(l.max(0.0));
                                    if s.latency_ms.len() > 2000 {
                                        s.latency_ms.drain(..1000);
                                    }
                                }
                            }
                            Some(Message::Pong { id, .. }) => {
                                if let Some(t) = pings.remove(&id) {
                                    stats.lock().unwrap().rtt_ms = t.elapsed().as_secs_f64() * 1000.0;
                                }
                            }
                            Some(Message::Ping { id, sent_us }) => send(&mut ws, &Message::Pong { id, sent_us })?,
                            _ => {}
                        }
                    }
                }
                Ok(Ws::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
                Err(e) => return Err(e.to_string()),
            }
        }
        if last_ping.elapsed() > Duration::from_secs(1) {
            last_ping = Instant::now();
            let id = frame_no;
            pings.insert(id, Instant::now());
            send(&mut ws, &Message::Ping { id, sent_us: now_us() })?;
        }
        // Backed up: send nothing new until what's queued is out, then start again on a keyframe
        // (the client never decodes a frame whose reference it didn't get, and never falls behind).
        if !flushed(&mut ws)? {
            if !backed_up {
                backed_up = true;
            }
            stats.lock().unwrap().skipped_backed_up += 1;
        } else {
            if backed_up {
                backed_up = false;
                enc.keyframe();
            }
            let t0 = Instant::now();
            let captured_at = now_us();
            let (encoded, grab_ms) = {
                let mut src = source.lock().unwrap();
                let frame = src.grab()?;
                let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                (enc.encode(frame.bgra, frame.stride)?, grab_ms)
            };
            let encode_ms = t0.elapsed().as_secs_f64() * 1000.0 - grab_ms;
            if let Some(e) = encoded {
                frame_no = frame_no.wrapping_add(1);
                captured.push_back((frame_no, captured_at));
                if captured.len() > 600 {
                    captured.pop_front();
                }
                let len = e.data.len() as u64;
                send(
                    &mut ws,
                    &Message::Video(Video {
                        keyframe: e.keyframe,
                        frame: frame_no,
                        timestamp_us: captured_at,
                        data: e.data,
                    }),
                )?;
                let mut s = stats.lock().unwrap();
                s.frames += 1;
                s.bytes += len;
                s.keyframes += e.keyframe as u64;
                s.encode_ms.push(encode_ms);
                s.grab_ms.push(grab_ms);
                if s.encode_ms.len() > 2000 {
                    s.encode_ms.drain(..1000);
                    s.grab_ms.drain(..1000);
                }
            }
        }
        if last_report.elapsed() > Duration::from_secs(5) {
            last_report = Instant::now();
            eprintln!("rein-remote: {}", stats_json(&stats.lock().unwrap()));
        }
        if let Some(rest) = frame_time.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}
