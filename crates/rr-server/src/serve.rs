//! `rein-remote serve`: one port for the web client (GET /) and the stream (a WebSocket on /stream).
//! Phase 1: WebSocket transport (tier 3, works through any HTTPS path), software H.264, keyframe-and-
//! skip when the connection backs up, and latency measured from the client's feedback.

use crate::capture::Source;
use crate::link::{E2eLink, Link, Recv, WsLink};
use rr_proto::{Codec, Config, Cursor, Hello, Message, Video, FEATURE_VIDEO_DATAGRAMS, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH, VERSION_MAX, VERSION_MIN};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const INDEX: &str = include_str!("../../../clients/web/index.html");
pub const CLIENT_JS: &str = include_str!("../../../clients/web/rein-remote.js");
pub const PAIR_JS: &str = include_str!("../../../clients/web/pair.js");

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
    /// Say where this server is on the local network (mDNS), when it listens beyond this computer.
    pub mdns: bool,
    /// Pairing (a code to enter once, then each device signs in with its key): `None` is on when it
    /// listens beyond this computer.
    pub pair: Option<bool>,
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
    // Which encoders this machine has, found now, and one opened and closed: the first a process opens
    // takes a second or two (the system loads it), which the first viewer would otherwise wait for.
    std::thread::spawn(move || {
        crate::encode::capabilities();
        let _ = crate::encode::open(Codec::H264, w, h, 60.0, 1000);
    });
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
    // Who may connect: the token in the link, or a device paired with a code.
    let reachable = listener.local_addr().is_ok_and(|a| a.ip().is_unspecified());
    let gate = Arc::new(crate::pairing::Gate::new(opts.token.clone(), opts.pair.unwrap_or(reachable)));
    if let (Some(code), Ok(at)) = (gate.new_code(), listener.local_addr()) {
        let host = crate::links::places()
            .into_iter()
            .find(|p| p.label == "this network")
            .map_or("localhost".to_string(), |p| p.host);
        let url = format!(
            "http://{host}:{}/#pair={}&id={}",
            at.port(),
            code.replace('-', ""),
            gate.fingerprint().unwrap_or_default()
        );
        eprintln!(
            "rein-remote: or pair a device once, then no link is needed: open this machine's page on it (http://{host}:{}/) and enter {code}, or scan:",
            at.port()
        );
        eprintln!("{}", crate::links::qr(&url));
        eprintln!("rein-remote: (the code works for 10 minutes, once; `rein-remote paired` lists the devices)");
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
            match crate::link::webtransport(at, gate.clone(), opts.tls.clone(), move |link| {
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
    // Found on the local network (mDNS), when it's reachable from there: by its fingerprint, nothing secret.
    let _advert = match listener.local_addr() {
        Ok(at) if opts.mdns && at.ip().is_unspecified() => crate::identity::Identity::load_or_create()
            .and_then(|id| {
                let a = crate::discover::advertise(at.port(), &id.fingerprint(), wt.as_ref().as_ref().map(|w| w.0), opts.tls.is_some())?;
                eprintln!("rein-remote: on this network as {} (id {})", crate::discover::machine_name(), id.fingerprint());
                Ok(a)
            })
            .map_err(|e| eprintln!("rein-remote: not found by name on this network ({e})"))
            .ok(),
        _ => None,
    };
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
        let (source, stats, opts, wt, tls, gate) = (source.clone(), stats.clone(), opts.clone(), wt.clone(), tls.clone(), gate.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, source, stats, &opts, &wt, tls.as_ref(), &gate).or_else(|e| if e.is_empty() { Ok(()) } else { Err(e) }) {
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
    gate: &crate::pairing::Gate,
) -> Result<(), String> {
    let mut conn = crate::conn::Conn::accept(stream, tls)?;
    let head = conn.read_head()?;
    let req = String::from_utf8_lossy(&head).to_string();
    // The path without its query string ("/?measure" is the page).
    let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let upgrade = req.to_ascii_lowercase().contains("upgrade: websocket");
    let query = target.split_once('?').map_or("", |(_, q)| q.split('#').next().unwrap_or(""));
    // The token or a paired device's ticket (compared in full, not stopping at the first difference).
    let token_ok = gate.allows(query);
    // Pairing and signing in: small forms, answered here.
    let method = req.split_whitespace().next().unwrap_or("GET").to_string();
    if path.starts_with("/pair/") || path.starts_with("/auth") {
        let body = if method == "POST" {
            read_body(&head, &req, &mut conn)?
        } else {
            String::new()
        };
        if let Some((status, json)) = gate.handle(&method, &path, query, &body) {
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{json}",
                json.len()
            );
            return conn.write_all(resp.as_bytes()).map_err(|e| e.to_string());
        }
    }
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
        "/pair.js" => ("200 OK", "text/javascript; charset=utf-8", PAIR_JS.to_string()),
        "/stats" => ("200 OK", "application/json", stats_json(&stats.lock().unwrap())),
        // Where WebTransport is, and its certificate's SHA-256 when the browser has to be given it
        // (a self-signed one; a real certificate needs nothing).
        "/info" => ("200 OK", "application/json", {
            let wt = match wt {
                Some((port, Some(hash))) => format!(r#"{{"port":{port},"hash":{hash:?}}}"#),
                Some((port, None)) => format!(r#"{{"port":{port},"hash":null}}"#),
                None => "null".to_string(),
            };
            // This machine's fingerprint (the page signs in to the one it paired with) when it pairs.
            let id = gate.fingerprint().map_or("null".to_string(), |f| format!("\"{f}\""));
            format!(r#"{{"webtransport":{wt},"id":{id}}}"#)
        }),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(resp.as_bytes()).map_err(|e| e.to_string())
}

/// A POST's form body (what came with the head, then the rest up to its Content-Length, 8 KB at most).
fn read_body(head: &[u8], req: &str, conn: &mut crate::conn::Conn) -> Result<String, String> {
    use std::io::Read;
    let len: usize = req
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    if len > 8192 {
        return Err("a request body too big".into());
    }
    let at = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |p| p + 4);
    let mut body = head[at..].to_vec();
    let mut b = [0u8; 1024];
    while body.len() < len {
        let n = conn.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&b[..n]);
    }
    body.truncate(len);
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A frame the encoder finished, out to the client: numbered, timed (reading, encoding) and kept for
/// the latency its feedback measures.
fn send_frame(
    e: crate::encode::Encoded,
    link: &mut (dyn Link + Send),
    encoding: &mut std::collections::VecDeque<(u64, Instant, f64)>,
    captured: &mut std::collections::VecDeque<(u32, u64, Instant, usize)>,
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
    captured.push_back((*frame_no, e.tag, Instant::now(), e.data.len()));
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

/// The picture's size at `scale` of `full` (even, at least 160×90).
fn scaled_size(full: (usize, usize), scale: f32) -> (usize, usize) {
    if scale >= 1.0 {
        return full;
    }
    let f = |n: usize, min: usize| (((n as f32 * scale) as usize).max(min.min(n))) & !1;
    (f(full.0, 160), f(full.1, 90))
}

/// An input event's coordinates from the picture's pixels (`pic`) to the source's (`src`).
fn unscale(ev: rr_proto::Input, pic: (usize, usize), src: (usize, usize)) -> rr_proto::Input {
    use rr_proto::Input;
    if pic.0 >= src.0 && pic.1 >= src.1 || pic.0 == 0 || pic.1 == 0 {
        return ev;
    }
    let x = |v: u16| ((v as usize * src.0) / pic.0).min(src.0.saturating_sub(1)) as u16;
    let y = |v: u16| ((v as usize * src.1) / pic.1).min(src.1.saturating_sub(1)) as u16;
    match ev {
        Input::PointerMove { x: px, y: py } => Input::PointerMove { x: x(px), y: y(py) },
        Input::Touch { points } => Input::Touch {
            points: points.into_iter().map(|(id, px, py)| (id, x(px), y(py))).collect(),
        },
        other => other,
    }
}

/// Parity shards per data shard for a datagram loss rate: enough that a frame very rarely loses more
/// than its parity covers (3× the loss rate, at least 10%, at most half).
pub fn parity_for(loss: f32) -> f32 {
    (loss * 3.0).clamp(0.1, 0.5)
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
        features: 0,
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
    let (mut ew, mut eh) = enc.size();
    // The picture at full size, and a buffer for it scaled down (when the link asks for that).
    let enc_full = (ew, eh);
    let mut small: Vec<u8> = Vec::new();
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
    // Video as datagrams with forward error correction, when the client takes them and the link has
    // them: a lost packet then costs nothing (parity rebuilds it), not a retransmission.
    let datagrams = client.features & FEATURE_VIDEO_DATAGRAMS != 0 && link.use_datagrams();
    let mut loss = 0.0f32;
    eprintln!(
        "rein-remote: {} connected ({:?} {ew}×{eh} at {fps} fps{refresh}{})",
        client.name,
        codec,
        if datagrams { ", video as datagrams" } else { "" }
    );

    let mut frame_time = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut adapt = crate::adapt::Adapt::new(opts.bitrate_kbps, fps);
    let mut last_adapt = Instant::now();
    let mut frame_no: u32 = 0;
    // Frames sent: number, capture time (µs, for latency) and when they went out (for the round trip).
    let mut captured: std::collections::VecDeque<(u32, u64, Instant, usize)> = Default::default();
    let mut next_frame = Instant::now();
    let mut backed_up = false;
    let mut last_ping = Instant::now() - Duration::from_secs(10);
    let mut pings: std::collections::HashMap<u32, Instant> = Default::default();
    let mut last_report = Instant::now();
    // The pointer as last sent: its place, and its image's serial (the image goes only when it changes).
    let mut cursor_sent: Option<(u16, u16, u32, bool)> = None;
    let mut seen_generation: Option<u64> = None;
    // Whether a frame has gone in where it was (said once in the log).
    let mut zero_copy = false;
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
                                // The client's pixels are the picture's: the source's when it's scaled down.
                                let ev = if (ew, eh) == enc_full { ev } else { unscale(ev, (ew, eh), (w, h)) };
                                let _ = source.lock().unwrap().input(&ev);
                            }
                            Some(Message::KeyframeRequest) => enc.keyframe(),
                            Some(Message::Feedback(f)) => {
                                if datagrams {
                                    // Parity for the loss seen lately: three times the loss rate, at least a tenth.
                                    loss = loss * 0.9 + (f.loss_permille as f32 / 1000.0) * 0.1;
                                    link.set_parity(parity_for(loss));
                                }
                                if let Some(&(_, at_us, sent, bytes)) = captured.iter().find(|(n, _, _, _)| *n == f.frame) {
                                    // The frame's round trip on this machine's clock alone (the client's can
                                    // be offset and drift, as a VM's does): queuing shows as it growing.
                                    // Less the client's own time decoding it (both its times are on its clock):
                                    // what's left is the network's, which is what the bitrate can change.
                                    let decode_ms = f.decoded_us.saturating_sub(f.received_us) as f64 / 1000.0;
                                    let rtt = sent.elapsed().as_secs_f64() * 1000.0 - decode_ms.min(1000.0);
                                    adapt.feedback(rtt.max(0.0), bytes, Instant::now());
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
                        let scaled = (ew, eh) != enc_full;
                        let surface = if enc.takes_surfaces() && (!scaled || enc.scales_surfaces()) {
                            src.surface()
                        } else {
                            None
                        };
                        if let Some(surface) = surface {
                            if !zero_copy {
                                zero_copy = true;
                                eprintln!("rein-remote: zero copy: frames go from the capture into the encoder on the GPU");
                            }
                            let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            encoding.push_back((captured_at, t0, grab_ms));
                            enc.encode_surface(&surface, captured_at)?
                        } else {
                            let frame = src.grab()?;
                            let grab_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            encoding.push_back((captured_at, t0, grab_ms));
                            if scaled {
                                crate::yuv::scale_down(frame.bgra, frame.stride, w, h, &mut small, ew, eh);
                                enc.encode(&small, ew * 4, captured_at)?
                            } else {
                                enc.encode(frame.bgra, frame.stride, captured_at)?
                            }
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
                // A new picture size: a new encoder (its first frame a keyframe) and Config.
                let size = scaled_size(enc_full, t.scale);
                if size != (ew, eh) && std::env::var("RR_SCALE").as_deref() != Ok("off") {
                    enc = crate::encode::open(codec, size.0, size.1, fps as f32, t.kbps)?;
                    (ew, eh) = enc.size();
                    encoding.clear();
                    link.send(&Message::Config(Config {
                        codec,
                        width: ew as u16,
                        height: eh as u16,
                        fps: fps as u16,
                        bitrate_kbps: t.kbps,
                    }))?;
                    eprintln!("rein-remote: picture {ew}×{eh} ({:.0}%) for {} kbit/s", t.scale * 100.0, t.kbps);
                }
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
    fn scaled_pictures_map_input_back_to_the_source() {
        assert_eq!(scaled_size((1920, 1080), 0.5), (960, 540));
        assert_eq!(scaled_size((1920, 1080), 0.75), (1440, 810));
        assert_eq!(scaled_size((200, 100), 0.5), (160, 90));
        let ev = unscale(rr_proto::Input::PointerMove { x: 480, y: 270 }, (960, 540), (1920, 1080));
        assert_eq!(ev, rr_proto::Input::PointerMove { x: 960, y: 540 });
        let same = unscale(rr_proto::Input::PointerMove { x: 5, y: 6 }, (1920, 1080), (1920, 1080));
        assert_eq!(same, rr_proto::Input::PointerMove { x: 5, y: 6 });
    }

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
