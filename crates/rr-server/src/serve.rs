//! `rein-remote serve`: one port for the web client (GET /) and the stream (a WebSocket on /stream).
//! Phase 1: WebSocket transport (tier 3, works through any HTTPS path), software H.264, keyframe-and-
//! skip when the connection backs up, and latency measured from the client's feedback.

use crate::capture::Source;
use crate::encode::Video as Encoder;
use rr_proto::{Codec, CodecCap, Config, Cursor, Hello, Message, Video, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH, VERSION_MAX, VERSION_MIN};
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
    /// When set, /stream and /stats need `?token=` this (another program on the machine, a proxy
    /// that didn't check who's asking, can't reach the screen).
    pub token: Option<String>,
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
    /// Ticks with nothing to read or encode (the screen didn't change).
    pub still: u64,
    /// What the link takes now (the adaptive controller's target) and its queuing.
    pub kbps: u32,
    pub fps: u32,
    pub queuing_ms: f64,
}

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

pub fn run(source: Box<dyn Source>, opts: Options) -> Result<(), String> {
    let listener = TcpListener::bind(&opts.listen).map_err(|e| format!("can't listen on {}: {e}", opts.listen))?;
    let (w, h) = source.size();
    // The address it got (a port of 0 picks a free one): a program that started it reads this line.
    let addr = listener.local_addr().map_or(opts.listen.clone(), |a| a.to_string());
    eprintln!("rein-remote: streaming a {w}×{h} screen on http://{addr} (open it in a browser)");
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
    // The path without its query string ("/?measure" is the page).
    let target = req.split_whitespace().nth(1).unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let upgrade = req.to_ascii_lowercase().contains("upgrade: websocket");
    let token_ok = opts.token.as_deref().is_none_or(|t| {
        let given = target
            .split_once('?')
            .map_or("", |(_, q)| q)
            .split('&')
            .find_map(|kv| kv.strip_prefix("token="))
            .unwrap_or("");
        // Compared in full, not stopping at the first difference.
        given.len() == t.len() && given.bytes().zip(t.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
    });
    if (path.starts_with("/stream") || path == "/stats") && !token_ok {
        let _ = stream.read(&mut head);
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(());
    }
    if path.starts_with("/stream") && upgrade {
        // Frames go at once, and the kernel holds only ~50 ms of them at the top bitrate: a slow link
        // shows up as a connection that backs up within a frame or two (keyframe-and-skip, and the
        // controller backs off), instead of a second of video queued where neither can see it.
        let _ = stream.set_nodelay(true);
        let _ = socket2::SockRef::from(&stream).set_send_buffer_size((opts.bitrate_kbps as usize * 1000 / 8 / 20).max(64 * 1024));
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
        r#"{{"frames":{},"keyframes":{},"skipped_backed_up":{},"bytes":{},"rtt_ms":{:.1},"latency_ms":{{"p50":{:.1},"p95":{:.1},"samples":{}}},"encode_ms":{{"p50":{:.1},"p95":{:.1}}},"grab_ms":{{"p50":{:.1},"p95":{:.1}}},"still":{},"kbps":{},"fps":{},"queuing_ms":{:.1}}}"#,
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
        s.still,
        s.kbps,
        s.fps,
        s.queuing_ms,
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
    let mut enc = Encoder::new(w, h, opts.fps as f32, opts.bitrate_kbps)?;
    eprintln!("rein-remote: encoding {}", enc.name());
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

    let mut frame_time = Duration::from_secs_f64(1.0 / opts.fps.max(1) as f64);
    let mut adapt = crate::adapt::Adapt::new(opts.bitrate_kbps, opts.fps);
    let mut last_adapt = Instant::now();
    let mut frame_no: u32 = 0;
    // Frames sent: number, capture time (µs, for latency) and when they went out (for the round trip).
    let mut captured: std::collections::VecDeque<(u32, u64, Instant)> = Default::default();
    let mut next_frame = Instant::now();
    let mut backed_up = false;
    let mut last_ping = Instant::now() - Duration::from_secs(10);
    let mut pings: std::collections::HashMap<u32, Instant> = Default::default();
    let mut last_report = Instant::now();
    // The pointer as last sent: its place, and its image's serial (the image goes only when it changes).
    let mut cursor_sent: Option<(u16, u16, u32, bool)> = None;
    let mut seen_generation: Option<u64> = None;
    let mut last_frame = Instant::now();
    const REFRESH: Duration = Duration::from_secs(1);
    loop {
        // Everything the client sent (as soon as it arrives: see the wait at the end).
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
                                if let Some(&(_, at_us, sent)) = captured.iter().find(|(n, _, _)| *n == f.frame) {
                                    // The frame's round trip on this machine's clock alone (the client's can
                                    // be offset and drift, as a VM's does): queuing shows as it growing.
                                    adapt.feedback(sent.elapsed().as_secs_f64() * 1000.0, Instant::now());
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
        let pointer = source.lock().unwrap().cursor();
        if let Some(p) = pointer {
            let new_shape = cursor_sent.is_none_or(|(_, _, s, _)| s != p.serial);
            if new_shape || cursor_sent.is_none_or(|(x, y, _, v)| (x, y, v) != (p.x, p.y, p.visible)) {
                cursor_sent = Some((p.x, p.y, p.serial, p.visible));
                let shape = new_shape.then_some(p.shape);
                send(
                    &mut ws,
                    &Message::Cursor(Cursor {
                        x: p.x,
                        y: p.y,
                        visible: p.visible,
                        shape,
                    }),
                )?;
            }
        }
        if last_ping.elapsed() > Duration::from_secs(1) {
            last_ping = Instant::now();
            let id = frame_no;
            pings.insert(id, Instant::now());
            send(&mut ws, &Message::Ping { id, sent_us: now_us() })?;
        }
        // A frame when one is due (input is handled between frames, the moment it comes).
        if Instant::now() >= next_frame {
            next_frame = (next_frame + frame_time).max(Instant::now());
            // Backed up: encode nothing new until what's queued is out. No keyframe after: nothing was
            // encoded meanwhile, so the next frame refers to the last one sent, which the client has (a
            // keyframe here would be the biggest frame at the worst moment).
            if !flushed(&mut ws)? {
                if !backed_up {
                    backed_up = true;
                }
                adapt.backed_up();
                stats.lock().unwrap().skipped_backed_up += 1;
            } else {
                backed_up = false;
                // A still screen (the source says nothing changed) is neither read nor encoded, except for a
                // keyframe someone needs and a refresh each second.
                let generation = source.lock().unwrap().generation();
                if generation.is_some() && generation == seen_generation && !enc.wants_keyframe() && last_frame.elapsed() < REFRESH {
                    stats.lock().unwrap().still += 1;
                } else {
                    seen_generation = generation;
                    last_frame = Instant::now();
                    let t0 = Instant::now();
                    let captured_at = now_us();
                    let (encoded, grab_ms) = {
                        let mut src = source.lock().unwrap();
                        // Zero copy where both ends can: the system's own frame straight into the hardware encoder.
                        let surface = if enc.takes_surfaces() { src.surface() } else { None };
                        if let Some(surface) = surface {
                            let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            (enc.encode_surface(&surface)?, grab_ms)
                        } else {
                            let frame = src.grab()?;
                            let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            (enc.encode(frame.bgra, frame.stride)?, grab_ms)
                        }
                    };
                    let encode_ms = t0.elapsed().as_secs_f64() * 1000.0 - grab_ms;
                    if let Some(e) = encoded {
                        frame_no = frame_no.wrapping_add(1);
                        captured.push_back((frame_no, captured_at, Instant::now()));
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
            }
        }
        if last_report.elapsed() > Duration::from_secs(5) {
            last_report = Instant::now();
            eprintln!("rein-remote: {}", stats_json(&stats.lock().unwrap()));
        }
        // A few times a second: what the link takes now.
        if last_adapt.elapsed() > Duration::from_millis(250) {
            last_adapt = Instant::now();
            let off = std::env::var("RR_ADAPT").as_deref() == Ok("off");
            if let Some(t) = adapt.tick(last_adapt).filter(|_| !off) {
                enc.set_bitrate(t.kbps)?;
                frame_time = Duration::from_secs_f64(1.0 / t.fps.max(1) as f64);
            }
            let t = adapt.target();
            if std::env::var_os("RR_DEBUG_ADAPT").is_some() {
                eprintln!("adapt: queuing {:.0} ms, target {:?}", adapt.queuing_ms(), t);
            }
            let mut s = stats.lock().unwrap();
            (s.kbps, s.fps, s.queuing_ms) = (t.kbps, t.fps, adapt.queuing_ms());
        }
        // Until the next frame is due, wait for the client: a message wakes this at once (input
        // reaches the source and feedback is timed without waiting for the next frame).
        let rest = next_frame.saturating_duration_since(Instant::now());
        if !rest.is_zero() {
            let sock = ws.get_mut();
            let _ = sock.set_nonblocking(false);
            let _ = sock.set_read_timeout(Some(rest));
            let _ = sock.peek(&mut [0u8; 1]);
            let _ = sock.set_nonblocking(true);
        }
    }
}
