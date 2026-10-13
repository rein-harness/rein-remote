//! `rein-remote serve`: one port for the web client (GET /) and the stream (a WebSocket on /stream).
//! Phase 1: WebSocket transport (tier 3, works through any HTTPS path), software H.264, keyframe-and-
//! skip when the connection backs up, and latency measured from the client's feedback.

use crate::capture::Source;
use crate::link::{E2eLink, Link, Recv, WsLink};
use rr_proto::{
    Access, Codec, Config, Cursor, DirectOffer, Hello, Message, Notice, Video, ACCESS_NO_AUDIO, ACCESS_NO_CLIPBOARD, ACCESS_RECORDED, ACCESS_VIEW_ONLY,
    FEATURE_AUDIO_DATAGRAMS, FEATURE_DIRECT, FEATURE_PASTE, FEATURE_VIDEO_DATAGRAMS, INPUT_GAMEPAD, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH, NOTICE_REFUSED,
    NOTICE_TIME_LIMIT, VERSION_MAX, VERSION_MIN,
};
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const INDEX: &str = include_str!("../../../clients/web/index.html");
pub const CLIENT_JS: &str = include_str!("../../../clients/web/rein-remote.js");
pub const PAIR_JS: &str = include_str!("../../../clients/web/pair.js");

/// The web client's files (the page and the modules it imports), as every server of the page — this
/// one and the relay — serves them: its content type and body.
pub fn web_file(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", INDEX)),
        "/rein-remote.js" => Some(("text/javascript; charset=utf-8", CLIENT_JS)),
        "/pair.js" => Some(("text/javascript; charset=utf-8", PAIR_JS)),
        _ => None,
    }
}

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
    /// Relays to connect out to (ws:// or wss://; more than one: a fleet, the room open on each), the
    /// room there, and the relays' secret for hosts.
    pub relay: Option<(Vec<String>, String, Option<String>)>,
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
    /// The sound that goes with the video, for viewers that play it (`--audio off`: none).
    pub audio: Option<Arc<crate::audio::Audio>>,
    /// Sign-in with an identity provider (`--oidc-issuer`, `--saml-idp-metadata`).
    pub signin: Option<Arc<crate::auth::SignIn>>,
    /// Who may do what (`--policy`); None: everyone let in may do everything.
    pub policy: Option<crate::policy::Policy>,
    /// `--audit FILE` (off: logs nothing).
    pub audit: Arc<crate::audit::Audit>,
    /// `--record DIR`: sessions recorded there (as the policy says, all of them without one).
    pub record: Option<std::path::PathBuf>,
    /// What this server shows, as a policy's `sources` name it (`display`, `window:Simulator`…).
    pub source_label: String,
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

/// What a session through the relay can offer a viewer that takes a direct path (FEATURE_DIRECT):
/// where WebTransport's socket may be reached (beyond NAT as the relay saw it, and on this machine's
/// networks), its certificate's hash, and the gate that gives one-time tickets. Punch packets go from
/// a clone of that socket (QUIC reads it; this only sends).
pub struct Direct {
    socket: UdpSocket,
    candidates: Vec<SocketAddr>,
    cert: [u8; 32],
    gate: Arc<crate::pairing::Gate>,
}

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

pub fn run(source: Box<dyn Source>, opts: Options) -> Result<(), String> {
    crate::metrics::start();
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
    // With sign-in, pairing only when asked for (the identity provider says who may connect).
    let pairing = opts.pair.unwrap_or(reachable && opts.signin.is_none());
    let gate = Arc::new(crate::pairing::Gate::new(opts.token.clone(), pairing).with_access(
        opts.signin.clone(),
        opts.policy.take(),
        opts.audit.clone(),
        opts.source_label.clone(),
    ));
    if let Some(s) = &opts.signin {
        eprintln!(
            "rein-remote: sign-in with {} at {}/ ({})",
            s.kind(),
            s.public_url,
            if gate.policy.is_some() {
                "the policy decides who may do what"
            } else {
                "no --policy: everyone the identity provider signs in may watch and control"
            }
        );
    }
    if opts.record.is_none() && gate.policy.as_ref().is_some_and(|p| p.rules.iter().any(|r| r.record == Some(true))) {
        eprintln!("rein-remote: the policy says to record some sessions, and there's no --record DIR: none will be");
    }
    if opts.audit.on() {
        opts.audit.log(
            "server_start",
            serde_json::json!({"source": opts.source_label, "signin": opts.signin.as_ref().map(|s| s.kind()), "policy": gate.policy.is_some(), "record": opts.record.as_ref().map(|d| d.display().to_string()), "version": env!("CARGO_PKG_VERSION")}),
        );
    }
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
    // Through a relay: connected out to it first (its room open), so it answers this machine's probe.
    let direct: Arc<OnceLock<Direct>> = Arc::default();
    // Several relays: the fastest to answer first (the link names it, and its UDP tells this machine
    // its address beyond NAT), and the room open on every one.
    let ranked = opts.relay.as_ref().map(|(relays, ..)| crate::fleet::rank(relays));
    if let (Some((_, room, secret)), Some(ranked)) = (opts.relay.clone(), ranked.clone()) {
        eprintln!("rein-remote: through the relay: open {}", crate::fleet::link(&ranked, &room, &opts.e2e_key));
        for relay in ranked.clone() {
            let (source, stats, opts, direct, room, secret, gate) = (
                source.clone(),
                stats.clone(),
                opts.clone(),
                direct.clone(),
                room.clone(),
                secret.clone(),
                gate.clone(),
            );
            std::thread::spawn(move || through_relay(&relay, &room, secret.as_deref(), source, stats, &opts, &direct, &gate));
        }
        crate::fleet::watch(ranked);
    }
    // WebTransport on the same port over UDP: the page connects with it when the browser can.
    let wt: Arc<Option<(u16, Option<[u8; 32]>)>> = Arc::new(match (opts.webtransport, listener.local_addr()) {
        (true, Ok(at)) => {
            let (source, stats, o) = (source.clone(), stats.clone(), opts.clone());
            let started = UdpSocket::bind(at).map_err(|e| e.to_string()).and_then(|udp| {
                // Through a relay: this socket's address beyond NAT, learned before QUIC takes it.
                let relayed = opts
                    .relay
                    .as_ref()
                    .zip(ranked.as_ref())
                    .map(|((_, room, _), ranked)| beyond_nat(&udp, &ranked[0], room));
                let punch = udp.try_clone().ok();
                let hash = crate::link::webtransport(udp, gate.clone(), opts.tls.clone(), move |link, adm| {
                    if let Err(e) = session(link, source.clone(), stats.clone(), &o, None, adm) {
                        eprintln!("rein-remote: {e}");
                    }
                })?;
                if let (Some(mapped), Some(cert), Some(socket)) = (relayed, hash, punch) {
                    let mut candidates: Vec<SocketAddr> = mapped.into_iter().collect();
                    if at.ip().is_unspecified() {
                        candidates.extend(rr_client::direct::local_candidates(at.port(), true));
                    } else if !at.ip().is_loopback() || candidates.is_empty() {
                        candidates.push(at);
                    }
                    let _ = direct.set(Direct {
                        socket,
                        candidates,
                        cert,
                        gate: gate.clone(),
                    });
                }
                Ok(hash)
            });
            match started {
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
                let a = rr_client::discover::advertise(at.port(), &id.fingerprint(), wt.as_ref().as_ref().map(|w| w.0), opts.tls.is_some())?;
                eprintln!(
                    "rein-remote: on this network as {} (id {})",
                    rr_client::discover::machine_name(),
                    id.fingerprint()
                );
                Ok(a)
            })
            .map_err(|e| eprintln!("rein-remote: not found by name on this network ({e})"))
            .ok(),
        _ => None,
    };
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

/// The address `udp` has beyond this machine's NAT, as the relay sees it (asked over UDP, for this
/// machine's room), and a keepalive from then on so the NAT keeps it. None when the relay doesn't
/// answer on UDP (an older relay, a proxy that passes no UDP): then only this machine's own networks
/// are offered.
fn beyond_nat(udp: &UdpSocket, relay: &str, room: &str) -> Option<SocketAddr> {
    let reflector = rr_client::direct::reflector(relay)?;
    // The room opens once the relay connection is up (on its own thread): a few seconds at most.
    let seen = rr_client::direct::probe(udp, reflector, room, Duration::from_secs(5));
    match seen {
        Some(a) => {
            eprintln!("rein-remote: the relay sees WebTransport's socket as {a} (offered to native viewers for a direct path)");
            if let Ok(clone) = udp.try_clone() {
                rr_client::direct::keep_alive(clone, reflector);
            }
        }
        None => eprintln!("rein-remote: the relay ({reflector}) didn't answer over UDP: a direct path only on this machine's networks"),
    }
    seen
}

/// Connected out to a relay: one viewer at a time, then the room again for the next.
#[allow(clippy::too_many_arguments)]
fn through_relay(
    relay: &str,
    room: &str,
    secret: Option<&str>,
    source: Arc<Mutex<Box<dyn Source>>>,
    stats: Arc<Mutex<Stats>>,
    opts: &Options,
    direct: &OnceLock<Direct>,
    gate: &crate::pairing::Gate,
) {
    let url = format!(
        "{}/host?room={room}{}",
        relay.trim_end_matches('/'),
        secret.map(|s| format!("&secret={s}")).unwrap_or_default()
    );
    // Said once, then again only after the relay was lost (not after each viewer).
    let mut lost = false;
    loop {
        match tungstenite::connect(&url) {
            Ok((ws, _)) => {
                use crate::link::Sock;
                crate::metrics::relay_update(relay, |r| {
                    r.registered = true;
                    r.registrations += 1;
                });
                if lost {
                    eprintln!("rein-remote: back in the room on the relay {}", crate::metrics::relay_name(relay));
                    lost = false;
                }
                let _ = ws.get_ref().tcp().set_nodelay(true);
                let _ = socket2::SockRef::from(ws.get_ref().tcp())
                    .set_send_buffer_size((top_kbps(opts.bitrate_kbps, 1280, 720, 60) as usize * 1000 / 8 / 20).max(64 * 1024));
                // It may wait a long time for a viewer: TCP keepalive finds a relay that went quietly
                // (a reboot, a network gone) within a minute, and this machine connects again.
                let keepalive = socket2::TcpKeepalive::new()
                    .with_time(Duration::from_secs(30))
                    .with_interval(Duration::from_secs(10))
                    .with_retries(3);
                let _ = socket2::SockRef::from(ws.get_ref().tcp()).set_tcp_keepalive(&keepalive);
                // Sealed end to end: the relay passes what it can't read.
                let link = match E2eLink::host(Box::new(WsLink::to_relay(ws)), &opts.e2e_key) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("rein-remote: {e}");
                        continue;
                    }
                };
                // A viewer through the relay is whoever has the link's key: the policy's `via = ["relay"]`.
                let adm = match gate.admission(crate::auth::Principal::new("relay viewer", "relay"), "relay", relay.to_string()) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("rein-remote: no viewers through the relay ({e})");
                        return;
                    }
                };
                let ended = session(Box::new(link), source.clone(), stats.clone(), opts, Some(direct), adm);
                crate::metrics::relay_update(relay, |r| r.registered = false);
                if let Err(e) = ended {
                    // The relay may be gone (it says so when this machine is back in its room).
                    eprintln!("rein-remote: {e}");
                    lost = true;
                }
            }
            Err(e) => {
                crate::metrics::relay_update(relay, |r| r.registered = false);
                // Said once while it's gone, not every few seconds.
                if !lost {
                    eprintln!("rein-remote: the relay ({}): {e}; trying again every 2 s", crate::metrics::relay_name(relay));
                }
                lost = true;
                std::thread::sleep(Duration::from_secs(2));
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
    let peer = stream.peer_addr().map_or_else(|_| "?".to_string(), |a| a.to_string());
    let mut conn = crate::conn::Conn::accept(stream, tls)?;
    let head = conn.read_head()?;
    let req = String::from_utf8_lossy(&head).to_string();
    // The path without its query string ("/?measure" is the page).
    let target = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let upgrade = req.to_ascii_lowercase().contains("upgrade: websocket");
    let query = target.split_once('?').map_or("", |(_, q)| q.split('#').next().unwrap_or(""));
    let method = req.split_whitespace().next().unwrap_or("GET").to_string();
    let (cookies, origin, host) = (header(&req, "cookie"), header(&req, "origin"), header(&req, "host"));
    // Signing in with the identity provider (a SAML response is a big form: 64 KB).
    if path.starts_with("/sso/") {
        let body = if method == "POST" {
            read_body(&head, &req, &mut conn, 64 * 1024)?
        } else {
            String::new()
        };
        let r = crate::auth::Req {
            method: &method,
            path: &path,
            query,
            body: &body,
            cookies: cookies.unwrap_or(""),
            origin,
            host,
            peer: peer.clone(),
        };
        if let Some(resp) = gate.sso(&r) {
            let extra: String = resp.headers.iter().map(|h| format!("{h}\r\n")).collect();
            let out = format!(
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n{extra}Connection: close\r\n\r\n{}",
                resp.status,
                resp.kind,
                resp.body.len(),
                resp.body
            );
            return conn.write_all(out.as_bytes()).map_err(|e| e.to_string());
        }
    }
    // Who it is: the token or a ticket (compared in full, not stopping at the first difference), or a
    // signed-in browser's cookie, from this server's own page only (a page elsewhere on this machine
    // could otherwise open the stream with this browser's sign-in).
    let own_page = gate.signin.as_ref().is_some_and(|s| crate::auth::same_origin(origin, host, &s.public_url));
    let principal = gate.admit(query, cookies.filter(|_| own_page)).or_else(|| {
        // Or the token as a bearer (a metrics scraper: kept out of the URL).
        crate::metrics::bearer(&req)
            .filter(|t| gate.token_is(t))
            .map(|_| crate::auth::Principal::new("token", "token"))
    });
    // Pairing and signing in: small forms, answered here.
    if path.starts_with("/pair/") || path.starts_with("/auth") {
        let body = if method == "POST" {
            read_body(&head, &req, &mut conn, 8192)?
        } else {
            String::new()
        };
        if let Some((status, json)) = gate.handle(&method, &path, query, &body) {
            // Pairing and a paired device signing in, for the audit log (never the answer: it has the ticket).
            let ok = status.starts_with("200");
            match path.as_str() {
                "/pair/finish" => gate.audit.log(
                    if ok { "pair" } else { "pair_failed" },
                    serde_json::json!({"addr": peer, "device": rr_client::http::json_str(&json, "id"), "reason": rr_client::http::json_str(&json, "error")}),
                ),
                "/auth" => gate.audit.log(
                    if ok { "device_signin" } else { "device_signin_failed" },
                    serde_json::json!({"addr": peer, "reason": rr_client::http::json_str(&json, "error")}),
                ),
                _ => {}
            }
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{json}",
                json.len()
            );
            return conn.write_all(resp.as_bytes()).map_err(|e| e.to_string());
        }
    }
    if (path.starts_with("/stream") || matches!(path.as_str(), "/stats" | "/metrics" | "/healthz")) && principal.is_none() {
        if path.starts_with("/stream") {
            gate.audit.log(
                "session_refused",
                serde_json::json!({"addr": peer, "transport": if upgrade { "websocket" } else { "http" }, "reason": "no valid token, ticket or sign-in"}),
            );
        }
        let _ = conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(());
    }
    if path.starts_with("/stream") && upgrade {
        let adm = match gate.admission(principal.unwrap(), "websocket", peer) {
            Ok(a) => a,
            Err(why) => {
                let resp = format!(
                    "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{why}",
                    why.len()
                );
                let _ = conn.write_all(resp.as_bytes());
                return Ok(());
            }
        };
        // Frames go at once, and the kernel holds only ~50 ms of them at the top bitrate: a slow link
        // shows up as a connection that backs up within a frame or two (keyframe-and-skip, and the
        // controller backs off), instead of a second of video queued where neither can see it.
        use crate::link::Sock;
        let _ = conn.tcp().set_nodelay(true);
        let _ = socket2::SockRef::from(conn.tcp()).set_send_buffer_size((top_kbps(opts.bitrate_kbps, 1280, 720, 60) as usize * 1000 / 8 / 20).max(64 * 1024));
        let ws = tungstenite::accept(crate::conn::Replay::new(head, conn)).map_err(|e| format!("websocket: {e}"))?;
        return session(Box::new(WsLink::new(ws)), source, stats, opts, None, adm);
    }
    let (status, kind, body) = match path.as_str() {
        p if web_file(p).is_some() => {
            let (kind, body) = web_file(p).unwrap();
            ("200 OK", kind, body.to_string())
        }
        "/stats" => ("200 OK", "application/json", stats_json(&stats.lock().unwrap(), opts.audio.as_deref())),
        "/healthz" => ("200 OK", "application/json", crate::metrics::serve_healthz()),
        "/metrics" => ("200 OK", "text/plain; version=0.0.4; charset=utf-8", crate::metrics::serve_prometheus()),
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
            // How the page signs in, when it signs in with an identity provider.
            let signin = gate.signin.as_ref().map_or("null".to_string(), |s| format!("\"{}\"", s.kind()));
            format!(r#"{{"webtransport":{wt},"id":{id},"signin":{signin}}}"#)
        }),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(resp.as_bytes()).map_err(|e| e.to_string())
}

/// A header's value (the first one by that name).
fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

/// A POST's form body (what came with the head, then the rest up to its Content-Length, `max` at most).
fn read_body(head: &[u8], req: &str, conn: &mut crate::conn::Conn, max: usize) -> Result<String, String> {
    use std::io::Read;
    let len: usize = req
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    if len > max {
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
    crate::metrics::FRAMES.inc();
    crate::metrics::VIDEO_BYTES.add(len);
    if e.keyframe {
        crate::metrics::KEYFRAMES.inc();
    }
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

/// A recording that ends (its file finished, its size said) however the session does.
struct RecordOnDrop(crate::record::Recorder);

impl Drop for RecordOnDrop {
    fn drop(&mut self) {
        match self.0.finish() {
            Ok(n) => eprintln!("rein-remote: recorded {} ({:.1} MB)", self.0.path.display(), n as f64 / 1e6),
            Err(e) => eprintln!("rein-remote: the recording {}: {e}", self.0.path.display()),
        }
    }
}

/// A frame for the recording, and the session's count of what it sent.
fn record(r: &Option<RecordOnDrop>, e: &crate::encode::Encoded, out: &mut Outcome) {
    out.frames += 1;
    out.bytes += e.data.len() as u64;
    if let Some(r) = r {
        r.0.video(&e.data, e.keyframe, e.tag);
    }
}

/// The picture's size at `scale` of `full` (even, at least 160×90).
/// The most the stream may take, kbit/s: `--bitrate`, or when it's not given, from the picture's
/// pixels a second: 0.12 bits a pixel a frame (8 Mbit/s at 1280×720 and 60 fps, ~30 at 1080p and
/// 120, ~240 at 4K and 240), at least 8 Mbit/s and at most 1 Gbit/s. A frame's share stays the same
/// at any rate (on a fixed 8 Mbit/s a hardware encoder skipped every other frame of a fast scene at
/// 240 fps). The controller still sends only what the link carries.
pub fn top_kbps(given: u32, w: usize, h: usize, fps: u32) -> u32 {
    if given > 0 {
        return given;
    }
    let kbps = (w * h) as f64 * fps.max(60) as f64 * 0.12 / 1000.0;
    (kbps as u32).clamp(8000, 1_000_000)
}

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

fn addrs(a: &[SocketAddr]) -> String {
    a.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
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

fn stats_json(s: &Stats, audio: Option<&crate::audio::Audio>) -> String {
    let pct = |v: &[f64], p: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        let mut v = v.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((v.len() - 1) as f64 * p) as usize]
    };
    // The sound: packets made, how many weren't silence, the last second's loudest (dBFS).
    let audio = match audio.map(|a| a.stats.lock().unwrap().clone()) {
        Some(a) => format!(
            r#"{{"packets":{},"bytes":{},"loud_packets":{},"level_db":{:.1},"listeners":{},"error":{:?}}}"#,
            a.packets, a.bytes, a.loud_packets, a.level_db, a.listeners, a.error
        ),
        None => "null".into(),
    };
    format!(
        r#"{{"frames":{},"keyframes":{},"skipped_backed_up":{},"bytes":{},"rtt_ms":{:.1},"latency_ms":{{"p50":{:.1},"p95":{:.1},"samples":{}}},"encode_ms":{{"p50":{:.1},"p95":{:.1}}},"grab_ms":{{"p50":{:.1},"p95":{:.1}}},"still":{},"kbps":{},"fps":{},"queuing_ms":{:.1},"audio":{audio}}}"#,
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

/// What a session did, for the audit log when it ends.
#[derive(Default)]
struct Outcome {
    frames: u64,
    bytes: u64,
    input_dropped: u64,
    pastes_dropped: u64,
    recording: Option<String>,
    recording_bytes: u64,
    ended: Option<&'static str>,
}

/// One viewer's session, let in as `adm` says: said in the audit log when it starts and ends.
fn session(
    link: Box<dyn Link + Send>,
    source: Arc<Mutex<Box<dyn Source>>>,
    stats: Arc<Mutex<Stats>>,
    opts: &Options,
    // Some for a session through the relay: what it can offer for a direct path, once there is that.
    direct: Option<&OnceLock<Direct>>,
    adm: crate::auth::Admission,
) -> Result<(), String> {
    let started = Instant::now();
    let _timing = FrameTiming::begin();
    let mut start = adm.audit_fields();
    start["source"] = opts.source_label.clone().into();
    opts.audit.log("session_start", start);
    let mut out = Outcome::default();
    let r = stream(link, source, stats, opts, direct, &adm, &mut out);
    // The recording is finished by now (its writer joined when the session's stream ended).
    if let Some(p) = &out.recording {
        out.recording_bytes = std::fs::metadata(p).map_or(0, |m| m.len());
    }
    opts.audit.log(
        "session_end",
        serde_json::json!({
            "session": adm.id, "user": adm.principal.user, "via": adm.principal.via, "addr": adm.addr, "transport": adm.transport,
            "seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0, "frames": out.frames, "bytes": out.bytes,
            "input_dropped": out.input_dropped, "pastes_dropped": out.pastes_dropped,
            "recording": out.recording, "recording_bytes": (out.recording_bytes > 0).then_some(out.recording_bytes),
            "reason": out.ended.unwrap_or(if r.is_ok() { "closed" } else { "error" }), "error": r.as_ref().err(),
        }),
    );
    r
}

/// Waits as short as a frame at 240 Hz (4 ms) need the system's timers to keep to them: on macOS an
/// ordinary thread's timers are coalesced (a 4 ms wait took about 8, which held 240 fps to ~110),
/// so the session's thread is marked user-interactive, as apps mark their frame-paced work; on
/// Windows the timer ticks every 15.6 ms unless asked for 1 ms (a 60 fps ceiling), asked here for as
/// long as the session runs.
struct FrameTiming;

impl FrameTiming {
    fn begin() -> Self {
        #[cfg(target_os = "macos")]
        {
            extern "C" {
                fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
            }
            const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
            // SAFETY: sets this thread's own QoS class; no pointers.
            unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
        }
        #[cfg(windows)]
        // SAFETY: plain call; undone in Drop.
        unsafe {
            windows::Win32::Media::timeBeginPeriod(1);
        }
        FrameTiming
    }
}

impl Drop for FrameTiming {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: pairs the timeBeginPeriod in begin.
        unsafe {
            windows::Win32::Media::timeEndPeriod(1);
        }
    }
}

fn stream(
    mut link: Box<dyn Link + Send>,
    source: Arc<Mutex<Box<dyn Source>>>,
    stats: Arc<Mutex<Stats>>,
    opts: &Options,
    direct: Option<&OnceLock<Direct>>,
    adm: &crate::auth::Admission,
    out: &mut Outcome,
) -> Result<(), String> {
    let perms = &adm.perms;
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
        // No sound for a viewer the policy gives none.
        audio: opts.audio.is_some() && perms.audio,
        // Gamepads too, where this machine can make a virtual one; none for a viewer who may only watch.
        input: if perms.input {
            INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH | if crate::gamepad::available().is_ok() { INPUT_GAMEPAD } else { 0 }
        } else {
            0
        },
        refresh_mhz: 0,
        features: FEATURE_PASTE,
    };
    link.send(&Message::Hello(hello.clone()))?;
    link.flushed()?;
    // The client's Hello (within 10 s), then the stream's config. Through the relay, the session starts
    // when this machine connects there, and the viewer comes when it comes: the 10 s from when the
    // relay says one joined.
    let mut deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        if link.waiting_for_viewer() {
            deadline = Instant::now() + Duration::from_secs(10);
        }
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
    // When the policy ends it.
    let deadline = adm.deadline();
    // The viewer's gamepads, as virtual pads for as long as the session lasts (why not, said when
    // the first comes).
    let mut pads = crate::gamepad::Pads::default();
    if rr_proto::negotiate(&hello, &client).is_none() {
        return Err(format!("{}: no protocol version in common", client.name));
    }
    let codec = rr_proto::pick_codec(&hello, &client).ok_or_else(|| format!("{}: decodes none of this server's codecs", client.name))?;
    // As fast as the client's display shows frames (measured there), up to 240 a second; software
    // encoding stays at 60 (what a CPU core holds).
    let auto = opts.fps == 0;
    let mut fps = pace(opts.fps, client.refresh_mhz, true);
    let mut enc = crate::encode::open(codec, w, h, fps as f32, top_kbps(opts.bitrate_kbps, w, h, fps))?;
    if auto && !enc.hardware() && fps > SOFTWARE_FPS {
        fps = pace(opts.fps, client.refresh_mhz, false);
        enc = crate::encode::open(codec, w, h, fps as f32, top_kbps(opts.bitrate_kbps, w, h, fps))?;
    }
    let top = top_kbps(opts.bitrate_kbps, w, h, fps);
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
        bitrate_kbps: top,
    }))?;
    let refresh = match client.refresh_mhz {
        0 => String::new(),
        r => format!(", its display {:.1} Hz", r as f64 / 1000.0),
    };
    // Video as datagrams with forward error correction, when the client takes them and the link has
    // them: a lost packet then costs nothing (parity rebuilds it), not a retransmission.
    let datagrams = client.features & FEATURE_VIDEO_DATAGRAMS != 0 && link.use_datagrams();
    let mut loss = 0.0f32;
    // Recorded when the server records and the policy (or no policy) says so; with the sound when there is.
    let record_dir = opts.record.as_ref().filter(|_| perms.record.unwrap_or(true));
    let recorder = match record_dir {
        Some(dir) => match crate::record::Recorder::start(dir, &adm.principal.user, &adm.id, codec, ew as u16, eh as u16, opts.audio.is_some()) {
            Ok(r) => {
                eprintln!("rein-remote: recording {}'s session to {}", adm.principal.user, r.path.display());
                out.recording = Some(r.path.display().to_string());
                Some(r)
            }
            Err(e) => {
                // A session the policy says to record isn't streamed unrecorded.
                link.send(&Message::Notice(Notice {
                    code: NOTICE_REFUSED,
                    text: format!("this session must be recorded, and can't be: {e}"),
                }))?;
                link.flushed()?;
                out.ended = Some("recording failed");
                return Err(format!("{}: not recorded ({e}), so not streamed", adm.principal.user));
            }
        },
        None => None,
    };
    let recorder = recorder.map(RecordOnDrop);
    // Sound, to a client that plays it (one that doesn't say so, an older one, gets none) and may
    // hear it: as datagrams of its own when it takes them (a lost packet is a gap, never a wait),
    // else in order. Listened to for the recording too.
    let play = client.audio && perms.audio;
    let audio = opts.audio.as_ref().filter(|_| play || recorder.is_some()).map(|a| a.listen());
    let audio_datagrams = play && audio.is_some() && client.features & FEATURE_AUDIO_DATAGRAMS != 0 && link.use_audio_datagrams();
    crate::metrics::SESSIONS.inc();
    let _viewer = if direct.is_some() {
        crate::metrics::VIEWERS_RELAYED.hold()
    } else {
        crate::metrics::VIEWERS_DIRECT.hold()
    };
    // What this viewer may do, so the page can say so (an older client skips it).
    let mut flags = 0;
    if !perms.input {
        flags |= ACCESS_VIEW_ONLY;
    }
    if !perms.clipboard {
        flags |= ACCESS_NO_CLIPBOARD;
    }
    if !perms.audio {
        flags |= ACCESS_NO_AUDIO;
    }
    if recorder.is_some() {
        flags |= ACCESS_RECORDED;
    }
    link.send(&Message::Access(Access {
        user: adm.principal.user.clone(),
        flags,
        seconds_left: deadline.map_or(0, |d| d.saturating_duration_since(Instant::now()).as_secs().max(1) as u32),
    }))?;
    eprintln!(
        "rein-remote: {} connected ({:?} {ew}×{eh} at {fps} fps{refresh}{}{})",
        client.name,
        codec,
        if datagrams { ", video as datagrams" } else { "" },
        match (play && audio.is_some(), audio_datagrams) {
            (false, _) => "",
            (true, true) => ", sound as datagrams",
            (true, false) => ", with sound",
        }
    );
    if !perms.input || adm.principal.via != "open" && adm.principal.via != "token" {
        eprintln!(
            "rein-remote: {} is {} ({}, policy rule {}){}",
            client.name,
            adm.principal.user,
            adm.principal.via,
            adm.rule,
            if perms.input { "" } else { ", view only" }
        );
    }
    // A viewer through the relay that can go direct: where to find this machine, the certificate to
    // expect, a ticket for one session. Sealed like everything else here, so the relay can't use it.
    // (Read now, not when the session began: a relayed session waits for its viewer from before
    // WebTransport is up.)
    let direct = direct.and_then(OnceLock::get);
    let mut punch_tag = None;
    if let Some(d) = direct.filter(|_| client.features & FEATURE_DIRECT != 0) {
        let ticket = d.gate.ticket_once(adm.principal.clone());
        punch_tag = Some(rr_proto::punch::tag(&ticket));
        link.send(&Message::DirectOffer(DirectOffer {
            candidates: d.candidates.clone(),
            cert_sha256: d.cert.to_vec(),
            ticket,
        }))?;
        crate::metrics::DIRECT_OFFERS.inc();
        eprintln!("rein-remote: offered {} a direct path ({})", client.name, addrs(&d.candidates));
    }

    let mut frame_time = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut adapt = crate::adapt::Adapt::new(top, fps);
    // Frames into the encoder and out of it: an encoder that skips frames (its rate control, when a
    // frame can't fit its share of the bitrate) is short of bits for the picture's size.
    let (mut enc_in, mut enc_out, mut enc_seen) = (0u64, 0u64, (0u64, 0u64));
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
    // The client's decode times (ms) over the last second.
    let mut decode_times: std::collections::VecDeque<(Instant, f64)> = Default::default();
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
                        // A viewer who may only watch: no input at all reaches the machine.
                        if !perms.input && matches!(m, Some(Message::Input(_))) {
                            out.input_dropped += 1;
                            continue;
                        }
                        // The clipboard off: no pastes (a viewer who may control can still type).
                        let m = match m {
                            Some(Message::Input(rr_proto::Input::Paste { .. })) | Some(Message::Clipboard { .. }) if !perms.clipboard => {
                                out.pastes_dropped += 1;
                                continue;
                            }
                            Some(Message::Input(rr_proto::Input::Paste { text })) => Some(Message::Input(rr_proto::Input::Text { text })),
                            m => m,
                        };
                        match m {
                            Some(Message::Input(rr_proto::Input::Gamepad {
                                pad,
                                buttons,
                                lx,
                                ly,
                                rx,
                                ry,
                                lt,
                                rt,
                            })) => pads.set(
                                pad,
                                crate::gamepad::State {
                                    buttons,
                                    lx,
                                    ly,
                                    rx,
                                    ry,
                                    lt,
                                    rt,
                                },
                            ),
                            Some(Message::Input(ev)) => {
                                // The client's pixels are the picture's: the source's when it's scaled down.
                                let ev = if (ew, eh) == enc_full { ev } else { unscale(ev, (ew, eh), (w, h)) };
                                let _ = source.lock().unwrap().input(&ev);
                            }
                            Some(Message::KeyframeRequest) => {
                                adapt.frame_lost(Instant::now());
                                enc.keyframe()
                            }
                            Some(Message::Feedback(f)) => {
                                if datagrams {
                                    // Parity for the loss seen lately: three times the loss rate, at least a tenth.
                                    loss = loss * 0.9 + (f.loss_permille as f32 / 1000.0) * 0.1;
                                    link.set_parity(parity_for(loss));
                                }
                                if let Some(&(_, at_us, sent, bytes)) = captured.iter().find(|(n, _, _, _)| *n == f.frame) {
                                    // The frame's round trip on this machine's clock alone (the client's can
                                    // be offset and drift, as a VM's does): queuing shows as it growing.
                                    // Less the client's usual time decoding a frame (both its times are on its
                                    // clock): its lowest over the last second, so decoding's own wander isn't
                                    // taken for queuing, while a decoder falling behind (frames waiting for it)
                                    // still is: that's a reason to send less too.
                                    let now = Instant::now();
                                    let decode_ms = f.decoded_us.saturating_sub(f.received_us) as f64 / 1000.0;
                                    decode_times.push_back((now, decode_ms.min(1000.0)));
                                    while decode_times.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(1)) {
                                        decode_times.pop_front();
                                    }
                                    let usual = decode_times.iter().map(|(_, d)| *d).fold(f64::INFINITY, f64::min);
                                    let rtt = sent.elapsed().as_secs_f64() * 1000.0 - usual;
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
                            Some(Message::DirectAnswer(a)) => {
                                // Punch packets to where the viewer may be, from WebTransport's own socket:
                                // this NAT then lets the viewer's QUIC in, as an answer to them.
                                if let (Some(d), Some(tag)) = (direct, punch_tag) {
                                    let to: Vec<SocketAddr> = a.candidates.into_iter().filter(SocketAddr::is_ipv4).collect();
                                    crate::metrics::DIRECT_ANSWERS.inc();
                                    eprintln!("rein-remote: punching toward the viewer ({})", addrs(&to));
                                    if let Ok(sock) = d.socket.try_clone() {
                                        std::thread::spawn(move || rr_client::direct::punch(&sock, &to, tag, Duration::from_secs(5)));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Recv::Nothing => break,
                Recv::Closed => return Ok(()),
            }
        }
        // Sound as it comes (small, and never held back with the video when the link backs up: a late
        // packet is the client's to drop).
        if let Some(l) = &audio {
            let mut sent = false;
            while let Ok(frame) = l.packets.try_recv() {
                if let Some(r) = &recorder {
                    if let Ok((Some(Message::Audio(a)), _)) = Message::decode(&frame) {
                        r.0.audio(&a.data, a.timestamp_us);
                    }
                }
                if !play {
                    continue;
                }
                link.send_audio(frame.to_vec())?;
                sent = true;
            }
            // Out now, not with the next frame (a WebSocket writes what's queued when flushed).
            if sent {
                link.flushed()?;
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
        // The policy's time limit: the viewer is told why, then the session ends.
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let mins = perms.max_session.map_or(0, |m| m.as_secs().div_ceil(60));
            link.send(&Message::Notice(Notice {
                code: NOTICE_TIME_LIMIT,
                text: format!("the session reached its time limit ({mins} min): sign in again for another"),
            }))?;
            let _ = link.flushed();
            eprintln!("rein-remote: {}'s session reached its time limit", adm.principal.user);
            out.ended = Some("time limit");
            return Ok(());
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
                crate::metrics::SKIPPED_BACKED_UP.inc();
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
                    enc_in += 1;
                    enc_out += done.len() as u64;
                    if !done.is_empty() {
                        adapt.sent(Instant::now());
                    }
                    for e in done {
                        record(&recorder, &e, out);
                        send_frame(e, &mut *link, &mut encoding, &mut captured, &mut frame_no, &stats)?;
                    }
                }
            }
        }
        // Frames a pipelined encoder finished since: out at once.
        if enc.in_flight() > 0 || !encoding.is_empty() {
            for e in enc.poll()? {
                enc_out += 1;
                adapt.sent(Instant::now());
                record(&recorder, &e, out);
                send_frame(e, &mut *link, &mut encoding, &mut captured, &mut frame_no, &stats)?;
            }
        }
        if last_report.elapsed() > Duration::from_secs(5) {
            last_report = Instant::now();
            eprintln!("rein-remote: {}", stats_json(&stats.lock().unwrap(), opts.audio.as_deref()));
        }
        // A few times a second: what the link takes now.
        if last_adapt.elapsed() > Duration::from_millis(250) {
            last_adapt = Instant::now();
            let off = std::env::var("RR_ADAPT").as_deref() == Ok("off");
            let (fed, got) = (enc_in - enc_seen.0, enc_out - enc_seen.1);
            enc_seen = (enc_in, enc_out);
            if fed >= 8 && got * 10 < fed * 9 {
                adapt.encoder_short(last_adapt, got == 0);
            }
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
        // With sound, a look every 2 ms: a packet comes every 10, and waits here no longer than that.
        if audio.is_some() {
            rest = rest.min(Duration::from_millis(2));
        }
        link.wait(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_top_bitrate_follows_pixels_a_second() {
        assert_eq!(top_kbps(5000, 3840, 2160, 240), 5000, "--bitrate wins");
        assert_eq!(top_kbps(0, 1280, 720, 60), 8000);
        assert_eq!(top_kbps(0, 640, 360, 30), 8000, "never under 8 Mbit/s");
        assert_eq!(top_kbps(0, 1920, 1080, 120), 29_859);
        assert_eq!(top_kbps(0, 3840, 2160, 240), 238_878);
        assert_eq!(top_kbps(0, 7680, 4320, 240), 955_514);
    }

    /// A connection in memory: what the session sends is kept, what the client says is queued.
    struct MemLink {
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
        inbox: std::sync::mpsc::Receiver<Vec<u8>>,
    }

    impl Link for MemLink {
        fn send_bytes(&mut self, bytes: Vec<u8>, _video: bool) -> Result<(), String> {
            self.sent.lock().unwrap().push(bytes);
            Ok(())
        }
        fn flushed(&mut self) -> Result<bool, String> {
            Ok(true)
        }
        fn recv(&mut self) -> Result<Recv, String> {
            match self.inbox.try_recv() {
                Ok(b) => Ok(Recv::Data(b)),
                Err(std::sync::mpsc::TryRecvError::Empty) => Ok(Recv::Nothing),
                Err(_) => Ok(Recv::Closed),
            }
        }
        fn wait(&mut self, d: Duration) {
            std::thread::sleep(d.min(Duration::from_millis(1)));
        }
    }

    /// A session with the test pattern for `secs` from its first video frame (opening an encoder can
    /// take a second or two), a client whose Hello is `hello`: the kinds of the frames it got, and the
    /// server's Hello.
    fn session_with(hello: Vec<u8>, sound: crate::audio::Sound, secs: f32) -> (Vec<u8>, Hello) {
        let (tx, inbox) = std::sync::mpsc::channel();
        let sent = Arc::new(Mutex::new(vec![]));
        let link = MemLink { sent: sent.clone(), inbox };
        let opts = Options {
            listen: String::new(),
            fps: 30,
            codec: Some(Codec::H264),
            bitrate_kbps: 2000,
            token: None,
            webtransport: false,
            relay: None,
            e2e_key: [0; 32],
            tls: None,
            tailscale_cert: false,
            mdns: false,
            pair: None,
            audio: crate::audio::Audio::new(sound),
            signin: None,
            policy: None,
            audit: Default::default(),
            record: None,
            source_label: "test-pattern".into(),
        };
        tx.send(hello).unwrap();
        let source: Arc<Mutex<Box<dyn Source>>> = Arc::new(Mutex::new(Box::new(crate::capture::TestPattern::new(320, 240))));
        let adm = crate::pairing::Gate::new(None, false)
            .admission(crate::auth::Principal::new("anyone", "open"), "memory", "test".into())
            .unwrap();
        let run = std::thread::spawn(move || session(Box::new(link), source, Arc::new(Mutex::new(Stats::default())), &opts, None, adm));
        let start = Instant::now();
        while !sent.lock().unwrap().iter().any(|f| f[0] == rr_proto::kind::VIDEO) && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_secs_f32(secs));
        drop(tx);
        run.join().unwrap().unwrap();
        let frames = sent.lock().unwrap().clone();
        let Ok((Some(Message::Hello(server)), _)) = Message::decode(&frames[0]) else {
            panic!("no hello first")
        };
        (frames.iter().map(|f| f[0]).collect(), server)
    }

    fn client_hello(audio: bool) -> Hello {
        Hello {
            version_min: 0,
            version_max: 0,
            name: "test".into(),
            codecs: vec![rr_proto::CodecCap {
                codec: Codec::H264,
                hardware: false,
            }],
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            audio,
            input: INPUT_POINTER,
            refresh_mhz: 60_000,
            features: 0,
        }
    }

    #[test]
    fn sound_goes_only_to_clients_that_play_it_and_its_absence_stops_nothing() {
        use crate::audio::Sound;
        use rr_proto::kind;
        let count = |kinds: &[u8], k: u8| kinds.iter().filter(|&&x| x == k).count();
        // A client that plays sound: Opus packets, 100 a second, next to the video.
        let (kinds, server) = session_with(Message::Hello(client_hello(true)).encode(), Sound::Tone, 1.5);
        assert!(server.audio);
        assert!(count(&kinds, kind::AUDIO) > 50, "{} audio packets: {kinds:?}", count(&kinds, kind::AUDIO));
        assert!(count(&kinds, kind::VIDEO) > 10);
        // One that says it doesn't: none.
        let (kinds, _) = session_with(Message::Hello(client_hello(false)).encode(), Sound::Tone, 1.0);
        assert_eq!(count(&kinds, kind::AUDIO), 0);
        assert!(count(&kinds, kind::VIDEO) > 10);
        // An older client (Hello version 1: no features, and audio: 0, as every web client before sound
        // said): none either.
        let mut old = Message::Hello(client_hello(false)).encode();
        old.truncate(old.len() - 4);
        old[1] = 1;
        let len = (old.len() - rr_proto::HEADER_LEN) as u32;
        old[3..7].copy_from_slice(&len.to_le_bytes());
        let (kinds, _) = session_with(old, Sound::Tone, 1.0);
        assert_eq!(count(&kinds, kind::AUDIO), 0);
        assert!(count(&kinds, kind::VIDEO) > 10);
        // No sound to be had (no device, no permission): the video streams on its own.
        let (kinds, _) = session_with(Message::Hello(client_hello(true)).encode(), Sound::Broken, 1.0);
        assert_eq!(count(&kinds, kind::AUDIO), 0);
        assert!(count(&kinds, kind::VIDEO) > 10);
        // And --audio off: the server says it has none.
        let (kinds, server) = session_with(Message::Hello(client_hello(true)).encode(), Sound::Off, 0.5);
        assert!(!server.audio);
        assert_eq!(count(&kinds, kind::AUDIO), 0);
    }

    /// The test pattern, with the input that reaches it kept (to see what a policy let through).
    struct Spy(crate::capture::TestPattern, Arc<Mutex<Vec<rr_proto::Input>>>);

    impl Source for Spy {
        fn size(&self) -> (usize, usize) {
            self.0.size()
        }
        fn grab(&mut self) -> Result<crate::capture::Frame<'_>, String> {
            self.0.grab()
        }
        fn input(&mut self, ev: &rr_proto::Input) -> Result<(), String> {
            self.1.lock().unwrap().push(ev.clone());
            Ok(())
        }
    }

    /// One HTTP request: status, headers (names lowercased), body.
    fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, Vec<(String, String)>, String) {
        use std::io::Read;
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        write!(
            c,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        let mut lines = head.lines();
        let status = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        let hs = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        (status, hs, body.to_string())
    }

    fn get<'a>(hs: &'a [(String, String)], name: &str) -> Vec<&'a str> {
        hs.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect()
    }

    /// Signs `user` in through the (mock) provider, as a browser would: the session cookie, or the page
    /// the server answered with (refused).
    fn sign_in(port: u16, idp: &crate::auth::oidc::mock::Idp, key: &crate::auth::jwt::testing::Signer, user: &str, groups: &[&str]) -> Result<String, String> {
        let (st, hs, _) = http(port, "GET", "/sso/login?next=/%3Fmeasure", &[], "");
        assert_eq!(st, 302);
        let to = get(&hs, "location")[0].to_string();
        assert!(to.starts_with(&format!("{}/authorize?", idp.issuer)), "{to}");
        let login = get(&hs, "set-cookie").into_iter().find(|c| c.starts_with("rr_login=")).unwrap();
        assert!(login.contains("HttpOnly") && login.contains("SameSite=Lax"));
        let login = login.split(';').next().unwrap().to_string();
        let q = crate::pairing::form(to.split_once('?').unwrap().1);
        let t = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let claims = serde_json::json!({"iss": idp.issuer, "aud": "rr-test", "sub": user, "email": user, "groups": groups,
            "nonce": q["nonce"], "iat": t, "exp": t + 300});
        *idp.next.lock().unwrap() = (200, serde_json::json!({"id_token": key.sign(&claims)}).to_string());
        let (st, hs, body) = http(
            port,
            "GET",
            &format!("/sso/oidc/callback?code=c-{user}&state={}", q["state"]),
            &[("Cookie", &login)],
            "",
        );
        if st != 302 {
            return Err(body);
        }
        assert_eq!(get(&hs, "location"), vec!["/?measure"]);
        let c = get(&hs, "set-cookie").into_iter().find(|c| c.starts_with("rr_session=")).unwrap();
        Ok(c.split(';').next().unwrap().to_string())
    }

    /// A viewer on /stream?ticket=…: what the server said (Access, Notice) and how many frames came in
    /// `secs`, after sending `input` (a key, a click, a paste) once the stream started.
    fn watch(port: u16, query: &str, extra: &[(&str, &str)], input: &[rr_proto::Input], secs: f32) -> Result<(Vec<Message>, usize), u16> {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://127.0.0.1:{port}/stream?{query}").into_client_request().unwrap();
        for (k, v) in extra {
            req.headers_mut()
                .insert(tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        let (mut ws, _) = match tungstenite::connect(req) {
            Ok(x) => x,
            Err(tungstenite::Error::Http(r)) => return Err(r.status().as_u16()),
            Err(e) => panic!("{e}"),
        };
        ws.send(tungstenite::Message::binary(Message::Hello(client_hello(true)).encode())).unwrap();
        let (mut said, mut frames) = (vec![], 0);
        // `secs` from the first frame (opening an encoder can take a second or two on a busy machine).
        let start = Instant::now();
        let mut first: Option<Instant> = None;
        let mut sent = false;
        while first.map_or(start.elapsed() < Duration::from_secs(30), |f| f.elapsed() < Duration::from_secs_f32(secs)) {
            let Ok(m) = ws.read() else { break };
            let tungstenite::Message::Binary(b) = m else { continue };
            let mut at = 0;
            while let Ok((m, used)) = Message::decode(&b[at..]) {
                at += used;
                match m {
                    Some(Message::Video(_)) => {
                        frames += 1;
                        first.get_or_insert_with(Instant::now);
                        if !sent {
                            sent = true;
                            for ev in input {
                                ws.send(tungstenite::Message::binary(Message::Input(ev.clone()).encode())).unwrap();
                            }
                        }
                    }
                    Some(m @ (Message::Access(_) | Message::Notice(_))) => said.push(m),
                    _ => {}
                }
                if at >= b.len() {
                    break;
                }
            }
        }
        let _ = ws.close(None);
        Ok((said, frames))
    }

    /// Sign-in with an identity provider, a policy, the audit log and recording, from end to end over
    /// HTTP and the WebSocket: two users let in with different rights, one refused.
    #[test]
    fn signed_in_viewers_get_what_the_policy_gives_them() {
        use crate::auth::jwt::testing::Signer;
        let dir = std::env::temp_dir().join(format!("rr-sso-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let idp = crate::auth::oidc::mock::start(vec![Signer::rsa("k1")]);
        let key = Signer::rsa("k1");
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let public = format!("http://127.0.0.1:{port}");
        let mut signin = crate::auth::SignIn::new(&public);
        signin.oidc = Some(
            crate::auth::oidc::Oidc::discover(crate::auth::oidc::Config {
                issuer: idp.issuer.clone(),
                client_id: "rr-test".into(),
                client_secret: Some("shh".into()),
                redirect_uri: format!("{public}/sso/oidc/callback"),
                scopes: "openid email".into(),
                user_claim: "email".into(),
                groups_claim: "groups".into(),
            })
            .unwrap(),
        );
        let policy: crate::policy::Policy = toml::from_str(
            r#"
[[rule]]
name = "admins"
groups = ["rr-admins"]
record = true
[[rule]]
name = "viewers"
groups = ["rr-viewers"]
view_only = true
clipboard = false
record = false
max_session_minutes = 0.05
"#,
        )
        .unwrap();
        let audit_path = dir.join("audit.jsonl");
        let inputs = Arc::new(Mutex::new(vec![]));
        let opts = Options {
            listen: format!("127.0.0.1:{port}"),
            fps: 30,
            codec: Some(Codec::H264),
            bitrate_kbps: 1000,
            token: None,
            webtransport: false,
            relay: None,
            e2e_key: [0; 32],
            tls: None,
            tailscale_cert: false,
            mdns: false,
            pair: Some(false),
            audio: crate::audio::Audio::new(crate::audio::Sound::Tone),
            signin: Some(Arc::new(signin)),
            policy: Some(policy),
            audit: Arc::new(crate::audit::Audit::open(&audit_path).unwrap()),
            record: Some(dir.join("rec")),
            source_label: "test-pattern".into(),
        };
        let spy = Spy(crate::capture::TestPattern::new(320, 240), inputs.clone());
        std::thread::spawn(move || run(Box::new(spy), opts));
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            std::thread::sleep(Duration::from_millis(20));
        }
        // The page says to sign in; without it, no ticket and no stream.
        let (_, _, info) = http(port, "GET", "/info", &[], "");
        assert!(info.contains(r#""signin":"oidc""#), "{info}");
        let origin = public.as_str();
        assert_eq!(http(port, "POST", "/sso/ticket", &[("Origin", origin)], "").0, 401);
        assert_eq!(watch(port, "", &[], &[], 1.0).unwrap_err(), 403);
        assert_eq!(watch(port, "ticket=00", &[], &[], 1.0).unwrap_err(), 403);

        let key_a = rr_proto::Input::Key {
            hid: 4,
            down: true,
            key: "a".into(),
        };
        let paste = rr_proto::Input::Paste { text: "pasted".into() };
        // Alice (rr-admins): everything; her session recorded.
        let alice = sign_in(port, &idp, &key, "alice@corp.example", &["rr-admins"]).unwrap();
        // A page elsewhere (another port on this machine) can't trade her cookie for a ticket.
        assert_eq!(
            http(port, "POST", "/sso/ticket", &[("Cookie", &alice), ("Origin", "http://127.0.0.1:1")], "").0,
            403
        );
        let (st, _, t) = http(port, "POST", "/sso/ticket", &[("Cookie", &alice), ("Origin", origin)], "");
        assert_eq!(st, 200, "{t}");
        let ticket = rr_client::http::json_str(&t, "ticket").unwrap();
        let (said, frames) = watch(port, &format!("ticket={ticket}"), &[], &[key_a.clone(), paste.clone()], 2.0).unwrap();
        assert!(frames > 10, "{frames} frames");
        let Some(Message::Access(a)) = said.first() else { panic!("{said:?}") };
        assert_eq!((a.user.as_str(), a.flags, a.seconds_left), ("alice@corp.example", rr_proto::ACCESS_RECORDED, 0));
        std::thread::sleep(Duration::from_millis(300));
        let got = inputs.lock().unwrap().clone();
        assert!(got.contains(&key_a), "{got:?}");
        assert!(
            got.contains(&rr_proto::Input::Text { text: "pasted".into() }),
            "a paste reaches the source as text: {got:?}"
        );
        // The WebSocket also takes her cookie, from this server's page (Origin), and not from elsewhere.
        assert!(watch(port, "", &[("Cookie", &alice), ("Origin", origin)], &[], 0.5).is_ok());
        assert_eq!(
            watch(port, "", &[("Cookie", &alice), ("Origin", "http://evil.example")], &[], 0.5).unwrap_err(),
            403
        );

        // Bob (rr-viewers): sees, can't type or paste, no recording, and 3 s from signing in.
        inputs.lock().unwrap().clear();
        let bob = sign_in(port, &idp, &key, "bob@corp.example", &["rr-viewers"]).unwrap();
        let (_, _, t) = http(port, "POST", "/sso/ticket", &[("Cookie", &bob), ("Origin", origin)], "");
        let ticket = rr_client::http::json_str(&t, "ticket").unwrap();
        let (said, frames) = watch(port, &format!("ticket={ticket}"), &[], &[key_a.clone(), paste.clone()], 6.0).unwrap();
        assert!(frames > 10, "{frames} frames");
        let Some(Message::Access(a)) = said.first() else { panic!("{said:?}") };
        assert_eq!(a.flags, rr_proto::ACCESS_VIEW_ONLY | rr_proto::ACCESS_NO_CLIPBOARD);
        assert!((1..=3).contains(&a.seconds_left), "{}", a.seconds_left);
        assert!(
            matches!(said.last(), Some(Message::Notice(n)) if n.code == rr_proto::NOTICE_TIME_LIMIT),
            "the session ended at its limit, saying why: {said:?}"
        );
        assert!(inputs.lock().unwrap().is_empty(), "a viewer's input never reaches the machine");
        // Her sign-in's time is up: no more tickets (the page goes to sign in again).
        assert_eq!(http(port, "POST", "/sso/ticket", &[("Cookie", &bob), ("Origin", origin)], "").0, 401);

        // Carol (no group the policy names): refused at sign-in, with the reason.
        let refused = sign_in(port, &idp, &key, "carol@corp.example", &[]).unwrap_err();
        assert!(refused.contains("no rule in the policy lets carol@corp.example in"), "{refused}");

        // Signing out ends the cookie (a POST: the GET only shows the button).
        http(port, "GET", "/sso/logout", &[("Cookie", &alice)], "");
        assert_eq!(http(port, "POST", "/sso/ticket", &[("Cookie", &alice), ("Origin", origin)], "").0, 200);
        http(port, "POST", "/sso/logout", &[("Cookie", &alice), ("Origin", origin)], "");
        assert_eq!(http(port, "POST", "/sso/ticket", &[("Cookie", &alice), ("Origin", origin)], "").0, 401);

        // The audit log: every event, nothing secret.
        std::thread::sleep(Duration::from_millis(500));
        let log = std::fs::read_to_string(&audit_path).unwrap();
        let events: Vec<serde_json::Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let find = |e: &str, user: &str| events.iter().filter(|x| x["event"] == e && x["user"] == user).cloned().collect::<Vec<_>>();
        assert_eq!(find("signin", "alice@corp.example").len(), 1);
        assert_eq!(find("signin", "bob@corp.example")[0]["rule"], "viewers");
        assert_eq!(find("signin_denied", "carol@corp.example").len(), 1);
        let ends = find("session_end", "alice@corp.example");
        assert!(
            ends.iter()
                .any(|e| e["recording"].is_string() && e["recording_bytes"].as_u64().unwrap_or(0) > 1000),
            "{ends:?}"
        );
        let end = &find("session_end", "bob@corp.example")[0];
        assert_eq!(end["reason"], "time limit");
        assert!(end["input_dropped"].as_u64().unwrap() >= 2, "{end}");
        assert!(end.get("recording").is_none());
        let start = &find("session_start", "bob@corp.example")[0];
        assert_eq!(
            (start["view_only"].as_bool(), start["transport"].as_str(), start["source"].as_str()),
            (Some(true), Some("websocket"), Some("test-pattern"))
        );
        assert!(events.iter().any(|e| e["event"] == "session_refused"));
        assert_eq!(find("signout", "alice@corp.example").len(), 1);
        for secret in [ticket.as_str(), alice.split('=').nth(1).unwrap(), bob.split('=').nth(1).unwrap(), "shh"] {
            assert!(!log.contains(secret), "the audit log has a secret in it");
        }
        // Alice's recording is there (and plays: see record's own test).
        let recs: Vec<String> = std::fs::read_dir(dir.join("rec"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(recs.len(), 2, "{recs:?}");
        assert!(
            recs.iter().all(|r| r.contains("-alice@corp.example-")),
            "only alice's sessions are recorded: {recs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_module_the_page_imports_is_served() {
        // `import … from './x.js'` in the page and its modules: each must be one web_file serves (the
        // relay serves the same), or the page doesn't load at all.
        for (name, text) in [("index.html", INDEX), ("rein-remote.js", CLIENT_JS), ("pair.js", PAIR_JS)] {
            for part in text.split("from './").skip(1) {
                let file = part.split('\'').next().unwrap();
                assert!(web_file(&format!("/{file}")).is_some(), "{name} imports ./{file}, which isn't served");
            }
        }
    }

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
