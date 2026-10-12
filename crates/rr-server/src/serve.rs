//! `rein-remote serve`: one port for the web client (GET /) and the stream (a WebSocket on /stream).
//! Phase 1: WebSocket transport (tier 3, works through any HTTPS path), software H.264, keyframe-and-
//! skip when the connection backs up, and latency measured from the client's feedback.

use crate::capture::Source;
use crate::link::{E2eLink, Link, Recv, WsLink};
use rr_proto::{Codec, Config, Cursor, Hello, Message, Video, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH, VERSION_MAX, VERSION_MIN};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const INDEX: &str = include_str!("../../../clients/web/index.html");
pub const CLIENT_JS: &str = include_str!("../../../clients/web/rein-remote.js");

pub struct Options {
    pub listen: String,
    /// Frames a second at most; 0: the client's display refresh as it measured it (up to 240, 60 in
    /// software; 60 when the client doesn't say).
    pub fps: u32,
    /// The codec to prefer when the client decodes it (`--codec`, H.264 by default); `None` (auto):
    /// HEVC where both sides do it in hardware, else H.264.
    pub codec: Option<Codec>,
    pub bitrate_kbps: u32,
    /// When set, /stream and /stats need `?token=` this (another program on the machine, a proxy
    /// that didn't check who's asking, can't reach the screen).
    pub token: Option<String>,
    /// WebTransport (QUIC) next to the WebSocket, on the same port number over UDP.
    pub webtransport: bool,
    /// A relay to connect out to (ws:// or wss://), the room there, and the relay's secret for hosts.
    pub relay: Option<(String, String, Option<String>)>,
    /// The end-to-end key for sessions through the relay (in the viewer's link, after #).
    pub e2e_key: [u8; 32],
    /// HTTPS on the page's port: a certificate and its key (PEM files). With `auto`, a real one for
    /// the machine's tailnet name from Tailscale when it can give one.
    pub tls: Option<(std::path::PathBuf, std::path::PathBuf)>,
    pub tailscale_cert: bool,
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
    let tailnet_name = if opts.listen == "auto" { crate::links::magic_dns() } else { None };
    // "auto": every interface, the first free port from 7979.
    let listener = if opts.listen == "auto" {
        (7979..=7999)
            .find_map(|p| TcpListener::bind(("0.0.0.0", p)).ok())
            .ok_or("no free port from 7979 to 7999 (give one with --listen)")?
    } else {
        TcpListener::bind(&opts.listen).map_err(|e| format!("can't listen on {}: {e}", opts.listen))?
    };
    // HTTPS: the certificate given, or (listening everywhere) Tailscale's for this machine's name.
    let mut opts = opts;
    if opts.tls.is_none() && opts.tailscale_cert && opts.listen == "auto" {
        if let Some(name) = &tailnet_name {
            opts.tls = crate::conn::tailscale_cert(name);
            if opts.tls.is_none() {
                eprintln!("rein-remote: no HTTPS certificate from Tailscale (turn on HTTPS for your tailnet, or give --tls-cert and --tls-key): over plain HTTP the page works in its slower mode");
            }
        }
    }
    let (w, h) = source.size();
    // The address it got (a port of 0 picks a free one): a program that started it reads this line.
    let addr = listener.local_addr().map_or(opts.listen.clone(), |a| a.to_string());
    eprintln!("rein-remote: streaming a {w}×{h} screen on http://{addr} (open it in a browser)");
    // On every interface: the links to open, with the token (they're the key: share them as such).
    if let Ok(at) = listener.local_addr() {
        if at.ip().is_unspecified() {
            let token = opts.token.as_deref().map(|t| format!("/?token={t}")).unwrap_or_default();
            eprintln!("rein-remote: open it from any of your devices (the link is the key):");
            for p in crate::links::places() {
                // HTTPS for the name the certificate is for (full quality on every device).
                let scheme = if opts.tls.is_some() && tailnet_name.as_deref() == Some(p.host.as_str()) {
                    "https"
                } else {
                    "http"
                };
                eprintln!("  {:<14} {scheme}://{}:{}{token}", p.label, p.host, at.port());
            }
        }
    }
    let tls = match &opts.tls {
        Some((c, k)) => Some(crate::conn::tls_from_pem(c, k)?),
        None => None,
    };
    let source = Arc::new(Mutex::new(source));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let opts = Arc::new(opts);
    // WebTransport on the same port over UDP: the page connects with it when the browser can.
    let wt: Arc<Option<(u16, Option<[u8; 32]>)>> = Arc::new(match (opts.webtransport, listener.local_addr()) {
        (true, Ok(at)) => {
            let (source, stats, o) = (source.clone(), stats.clone(), opts.clone());
            match crate::link::webtransport(at, opts.token.clone(), opts.tls.clone(), move |link| {
                if let Err(e) = session(link, source.clone(), stats.clone(), &o) {
                    eprintln!("rein-remote: {e}");
                }
            }) {
                Ok(hash) => {
                    eprintln!("rein-remote: WebTransport on udp {at}");
                    Some((at.port(), hash))
                }
                Err(e) => {
                    eprintln!("rein-remote: no WebTransport ({e}); the WebSocket works on its own");
                    None
                }
            }
        }
        _ => None,
    });
    if let Some((relay, room, secret)) = opts.relay.clone() {
        let (source, stats, opts) = (source.clone(), stats.clone(), opts.clone());
        let page = relay.replacen("ws", "http", 1);
        eprintln!(
            "rein-remote: through the relay: open {}/?room={room}#k={}",
            page.trim_end_matches('/'),
            crate::e2e::b64url(&opts.e2e_key)
        );
        std::thread::spawn(move || through_relay(&relay, &room, secret.as_deref(), source, stats, &opts));
    }
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (source, stats, opts, wt, tls) = (source.clone(), stats.clone(), opts.clone(), wt.clone(), tls.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, source, stats, &opts, &wt, tls.as_ref()).or_else(|e| if e.is_empty() { Ok(()) } else { Err(e) }) {
                eprintln!("rein-remote: {e}");
            }
        });
    }
    Ok(())
}

/// Connected out to a relay: one viewer at a time, then the room again for the next.
fn through_relay(relay: &str, room: &str, secret: Option<&str>, source: Arc<Mutex<Box<dyn Source>>>, stats: Arc<Mutex<Stats>>, opts: &Options) {
    let url = format!(
        "{}/host?room={room}{}",
        relay.trim_end_matches('/'),
        secret.map(|s| format!("&secret={s}")).unwrap_or_default()
    );
    loop {
        match tungstenite::connect(&url) {
            Ok((ws, _)) => {
                use crate::link::Sock;
                let _ = ws.get_ref().tcp().set_nodelay(true);
                let _ = socket2::SockRef::from(ws.get_ref().tcp()).set_send_buffer_size((opts.bitrate_kbps as usize * 1000 / 8 / 20).max(64 * 1024));
                // Sealed end to end: the relay passes what it can't read.
                let link = match E2eLink::host(Box::new(WsLink(ws)), &opts.e2e_key) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("rein-remote: {e}");
                        continue;
                    }
                };
                if let Err(e) = session(Box::new(link), source.clone(), stats.clone(), opts) {
                    eprintln!("rein-remote: {e}");
                }
            }
            Err(e) => {
                eprintln!("rein-remote: the relay ({relay}): {e}");
                std::thread::sleep(Duration::from_secs(4));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn handle(
    stream: TcpStream,
    source: Arc<Mutex<Box<dyn Source>>>,
    stats: Arc<Mutex<Stats>>,
    opts: &Options,
    wt: &Option<(u16, Option<[u8; 32]>)>,
    tls: Option<&Arc<rustls::ServerConfig>>,
) -> Result<(), String> {
    let mut conn = crate::conn::Conn::accept(stream, tls)?;
    let head = conn.read_head()?;
    let req = String::from_utf8_lossy(&head).to_string();
    // The path without its query string ("/?measure" is the page).
    let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
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
        let _ = conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(());
    }
    if path.starts_with("/stream") && upgrade {
        // Frames go at once, and the kernel holds only ~50 ms of them at the top bitrate: a slow link
        // shows up as a connection that backs up within a frame or two (keyframe-and-skip, and the
        // controller backs off), instead of a second of video queued where neither can see it.
        use crate::link::Sock;
        let _ = conn.tcp().set_nodelay(true);
        let _ = socket2::SockRef::from(conn.tcp()).set_send_buffer_size((opts.bitrate_kbps as usize * 1000 / 8 / 20).max(64 * 1024));
        let ws = tungstenite::accept(crate::conn::Replay::new(head, conn)).map_err(|e| format!("websocket: {e}"))?;
        return session(Box::new(WsLink(ws)), source, stats, opts);
    }
    let (status, kind, body) = match path.as_str() {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", INDEX.to_string()),
        "/rein-remote.js" => ("200 OK", "text/javascript; charset=utf-8", CLIENT_JS.to_string()),
        "/stats" => ("200 OK", "application/json", stats_json(&stats.lock().unwrap())),
        // Where WebTransport is, and its certificate's SHA-256 when the browser has to be given it
        // (a self-signed one; a real certificate needs nothing).
        "/info" => (
            "200 OK",
            "application/json",
            match wt {
                Some((port, Some(hash))) => format!(r#"{{"webtransport":{{"port":{port},"hash":{hash:?}}}}}"#),
                Some((port, None)) => format!(r#"{{"webtransport":{{"port":{port},"hash":null}}}}"#),
                None => r#"{"webtransport":null}"#.to_string(),
            },
        ),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(resp.as_bytes()).map_err(|e| e.to_string())
}

/// A frame the encoder finished, out to the client: numbered, timed (reading, encoding) and kept for
/// the latency its feedback measures.
fn send_frame(
    e: crate::encode::Encoded,
    link: &mut (dyn Link + Send),
    encoding: &mut std::collections::VecDeque<(u64, Instant, f64)>,
    captured: &mut std::collections::VecDeque<(u32, u64, Instant)>,
    frame_no: &mut u32,
    stats: &Mutex<Stats>,
) -> Result<(), String> {
    // This frame's own timing (frames the encoder dropped, earlier than it, are forgotten).
    let mut timing = None;
    while let Some(&(tag, t0, grab_ms)) = encoding.front() {
        if tag > e.tag {
            break;
        }
        encoding.pop_front();
        if tag == e.tag {
            timing = Some((t0, grab_ms));
        }
    }
    *frame_no = frame_no.wrapping_add(1);
    captured.push_back((*frame_no, e.tag, Instant::now()));
    if captured.len() > 600 {
        captured.pop_front();
    }
    let len = e.data.len() as u64;
    link.send(&Message::Video(Video {
        keyframe: e.keyframe,
        frame: *frame_no,
        timestamp_us: e.tag,
        data: e.data,
    }))?;
    let mut s = stats.lock().unwrap();
    s.frames += 1;
    s.bytes += len;
    s.keyframes += e.keyframe as u64;
    if let Some((t0, grab_ms)) = timing {
        s.encode_ms.push(t0.elapsed().as_secs_f64() * 1000.0 - grab_ms);
        s.grab_ms.push(grab_ms);
        if s.encode_ms.len() > 2000 {
            s.encode_ms.drain(..1000);
            s.grab_ms.drain(..1000);
        }
    }
    Ok(())
}

/// The most frames a second a stream runs at, and what software encoding holds.
pub const MAX_FPS: u32 = 240;
pub const SOFTWARE_FPS: u32 = 60;

/// Frames a second for a stream: `asked` when given (capped at 240), else the client's display
/// refresh rate (millihertz, 0 when it didn't say: 60), capped at 240 with a hardware encoder and 60
/// without.
pub fn pace(asked: u32, refresh_mhz: u32, hardware: bool) -> u32 {
    if asked > 0 {
        return asked.min(MAX_FPS);
    }
    let refresh = if refresh_mhz >= 10_000 { (refresh_mhz + 500) / 1000 } else { SOFTWARE_FPS };
    refresh.clamp(10, if hardware { MAX_FPS } else { SOFTWARE_FPS })
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

fn session(mut link: Box<dyn Link + Send>, source: Arc<Mutex<Box<dyn Source>>>, stats: Arc<Mutex<Stats>>, opts: &Options) -> Result<(), String> {
    let (w, h) = source.lock().unwrap().size();
    // What this machine encodes, the codec asked for first (H.264 by default: every device decodes
    // it); with `auto`, HEVC first when this machine encodes it in hardware.
    let mut codecs = crate::encode::capabilities().to_vec();
    let first = opts.codec.unwrap_or(if codecs.iter().any(|c| c.codec == Codec::H265 && c.hardware) {
        Codec::H265
    } else {
        Codec::H264
    });
    codecs.sort_by_key(|c| c.codec != first);
    let hello = Hello {
        version_min: VERSION_MIN,
        version_max: VERSION_MAX,
        name: std::env::var("HOSTNAME").unwrap_or_else(|_| "rein-remote".into()),
        codecs,
        max_width: w as u16,
        max_height: h as u16,
        max_fps: if opts.fps == 0 { MAX_FPS } else { opts.fps } as u16,
        audio: false,
        input: INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH,
        refresh_mhz: 0,
    };
    link.send(&Message::Hello(hello.clone()))?;
    link.flushed()?;
    // The client's Hello (with a deadline), then the stream's config.
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        if Instant::now() > deadline {
            return Err("the client didn't say hello".into());
        }
        match link.recv_wait(Duration::from_millis(200))? {
            Recv::Data(b) => {
                if let Ok((Some(Message::Hello(c)), _)) = Message::decode(&b) {
                    break c;
                }
            }
            Recv::Closed => return Ok(()),
            Recv::Nothing => {}
        }
    };
    if rr_proto::negotiate(&hello, &client).is_none() {
        return Err(format!("{}: no protocol version in common", client.name));
    }
    let codec = rr_proto::pick_codec(&hello, &client).ok_or_else(|| format!("{}: decodes none of this server's codecs", client.name))?;
    // As fast as the client's display shows frames (measured there), up to 240 a second; software
    // encoding stays at 60 (what a CPU core holds).
    let auto = opts.fps == 0;
    let mut fps = pace(opts.fps, client.refresh_mhz, true);
    let mut enc = crate::encode::open(codec, w, h, fps as f32, opts.bitrate_kbps)?;
    if auto && !enc.hardware() && fps > SOFTWARE_FPS {
        fps = pace(opts.fps, client.refresh_mhz, false);
        enc = crate::encode::open(codec, w, h, fps as f32, opts.bitrate_kbps)?;
    }
    eprintln!("rein-remote: encoding {}", enc.name());
    let (ew, eh) = enc.size();
    link.send(&Message::Config(Config {
        codec,
        width: ew as u16,
        height: eh as u16,
        fps: fps as u16,
        bitrate_kbps: opts.bitrate_kbps,
    }))?;
    let refresh = match client.refresh_mhz {
        0 => String::new(),
        r => format!(", its display {:.1} Hz", r as f64 / 1000.0),
    };
    eprintln!("rein-remote: {} connected ({:?} {ew}×{eh} at {fps} fps{refresh})", client.name, codec);

    let mut frame_time = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut adapt = crate::adapt::Adapt::new(opts.bitrate_kbps, fps);
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
    // Frames in the encoder: their capture time (the tag), when they were read, and how long reading took.
    let mut encoding: std::collections::VecDeque<(u64, Instant, f64)> = Default::default();
    loop {
        // Everything the client sent (as soon as it arrives: see the wait at the end).
        loop {
            match link.recv()? {
                Recv::Data(b) => {
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
                            Some(Message::Ping { id, sent_us }) => link.send(&Message::Pong { id, sent_us })?,
                            _ => {}
                        }
                    }
                }
                Recv::Nothing => break,
                Recv::Closed => return Ok(()),
            }
        }
        let pointer = source.lock().unwrap().cursor();
        if let Some(p) = pointer {
            let new_shape = cursor_sent.is_none_or(|(_, _, s, _)| s != p.serial);
            if new_shape || cursor_sent.is_none_or(|(x, y, _, v)| (x, y, v) != (p.x, p.y, p.visible)) {
                cursor_sent = Some((p.x, p.y, p.serial, p.visible));
                let shape = new_shape.then_some(p.shape);
                link.send(&Message::Cursor(Cursor {
                    x: p.x,
                    y: p.y,
                    visible: p.visible,
                    shape,
                }))?;
            }
        }
        if last_ping.elapsed() > Duration::from_secs(1) {
            last_ping = Instant::now();
            let id = frame_no;
            pings.insert(id, Instant::now());
            link.send(&Message::Ping { id, sent_us: now_us() })?;
        }
        // A frame when one is due (input is handled between frames, the moment it comes).
        if Instant::now() >= next_frame {
            next_frame = (next_frame + frame_time).max(Instant::now());
            // Backed up: encode nothing new until what's queued is out. No keyframe after: nothing was
            // encoded meanwhile, so the next frame refers to the last one sent, which the client has (a
            // keyframe here would be the biggest frame at the worst moment).
            if !link.flushed()? {
                if !backed_up {
                    backed_up = true;
                }
                adapt.backed_up();
                stats.lock().unwrap().skipped_backed_up += 1;
            } else {
                backed_up = false;
                // A still screen (the source says nothing changed) is neither read nor encoded nor sent,
                // however long it stays still: only a keyframe someone asks for. (The client has the last
                // frame; frames are never lost on the way, and one that can't be decoded asks again.)
                let generation = source.lock().unwrap().generation();
                if generation.is_some() && generation == seen_generation && !enc.wants_keyframe() {
                    stats.lock().unwrap().still += 1;
                } else {
                    seen_generation = generation;
                    let t0 = Instant::now();
                    let captured_at = now_us();
                    let done = {
                        let mut src = source.lock().unwrap();
                        // Zero copy where both ends can: the system's own frame straight into the hardware encoder.
                        let surface = if enc.takes_surfaces() { src.surface() } else { None };
                        if let Some(surface) = surface {
                            let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            encoding.push_back((captured_at, t0, grab_ms));
                            enc.encode_surface(&surface, captured_at)?
                        } else {
                            let frame = src.grab()?;
                            let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            encoding.push_back((captured_at, t0, grab_ms));
                            enc.encode(frame.bgra, frame.stride, captured_at)?
                        }
                    };
                    for e in done {
                        send_frame(e, &mut *link, &mut encoding, &mut captured, &mut frame_no, &stats)?;
                    }
                }
            }
        }
        // Frames a pipelined encoder finished since: out at once.
        if enc.in_flight() > 0 || !encoding.is_empty() {
            for e in enc.poll()? {
                send_frame(e, &mut *link, &mut encoding, &mut captured, &mut frame_no, &stats)?;
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
        // While the encoder works on a frame, look again within a millisecond: it goes out once done.
        let mut rest = next_frame.saturating_duration_since(Instant::now());
        if enc.in_flight() > 0 {
            rest = rest.min(Duration::from_millis(1));
        }
        link.wait(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paces_to_the_clients_display() {
        assert_eq!(pace(0, 59_940, true), 60);
        assert_eq!(pace(0, 120_000, true), 120);
        assert_eq!(pace(0, 200_000, true), 200);
        assert_eq!(pace(0, 360_000, true), 240);
        // Software holds 60; an old client (no refresh rate) gets 60.
        assert_eq!(pace(0, 144_000, false), 60);
        assert_eq!(pace(0, 0, true), 60);
        // --fps N wins, up to 240.
        assert_eq!(pace(30, 144_000, true), 30);
        assert_eq!(pace(500, 0, false), 240);
    }
}
