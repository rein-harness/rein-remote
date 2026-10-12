//! Rein Remote's native client core (no UI, no decoding): connect to a server, say hello, and get its
//! stream as events (the config, encoded video frames with their capture times, the pointer, audio),
//! while it answers pings, tells the server what arrived (feedback, loss), and asks for keyframes. The
//! desktop and mobile apps build on it; `rein-remote connect` uses it to measure.
//!
//! How it gets there, fastest first:
//!
//! - **WebTransport** (QUIC, the endpoint browsers use too): video as datagrams with forward error
//!   correction, or a stream a frame; the certificate checked by the SHA-256 the server gives in
//!   `/info` (a self-signed one), or against the system's roots (a real one).
//! - **WebSocket** (TCP), when UDP doesn't get through.
//! - **A relay** (`http(s)://relay/?room=…#k=…`, the link `serve --relay` prints), sealed end to end
//!   with the key after `#` (the relay can't read or change what it passes); a fleet's other relays
//!   after it (`&alt=`), tried in turn when the first isn't there. From there, a **direct
//!   path** through NAT when there is one ([`direct`]): the session moves to it, and stays on the relay
//!   when there isn't.
//!
//! Pairing and signing in ([`pairing`]) when the link has no token, and servers found on the local
//! network by name ([`discover`]).

pub mod direct;
pub mod discover;
pub mod e2e;
pub mod http;
pub mod keys;
mod link;
pub mod pairing;

pub use link::redact;
use link::{Cert, Ev, In, Link};
use rr_proto::{
    Audio, CodecCap, Config, Cursor, DirectAnswer, DirectOffer, Feedback, Hello, Input, Message, FEATURE_DIRECT, FEATURE_VIDEO_DATAGRAMS, INPUT_KEYBOARD,
    INPUT_POINTER, INPUT_TOUCH, VERSION_MAX, VERSION_MIN,
};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the session runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    WebTransport,
    WebSocket,
    /// Through a relay (a WebSocket there, sealed end to end).
    Relay,
    /// WebTransport straight to the host, after punching through NAT from a relayed session.
    Direct,
}

impl Transport {
    /// Its name in stats: `webtransport-datagrams` or `-streams` (how video comes), `websocket`,
    /// `relay`, `direct-after-punch`.
    pub fn label(self, datagrams: bool) -> &'static str {
        match self {
            Transport::WebTransport if datagrams => "webtransport-datagrams",
            Transport::WebTransport => "webtransport-streams",
            Transport::WebSocket => "websocket",
            Transport::Relay => "relay",
            Transport::Direct => "direct-after-punch",
        }
    }
}

pub struct Options {
    /// The name the server shows for this client.
    pub name: String,
    /// A pairing code (the one the server shows) to pair with first, when this client isn't yet.
    pub pair: Option<String>,
    /// Try WebTransport (QUIC) before the WebSocket.
    pub webtransport: bool,
    /// Ask for video as datagrams with forward error correction (over WebTransport).
    pub datagrams: bool,
    /// Through a relay: move to a direct path when one can be punched.
    pub direct: bool,
    /// What this client decodes (H.264 in software, by default: the server picks from these).
    pub codecs: Vec<CodecCap>,
    /// This display's refresh rate in millihertz: the server paces to it.
    pub refresh_mhz: u32,
    /// Send feedback as each frame arrives (a client that decodes sends its own, after decoding, with
    /// [`Session::feedback`]).
    pub auto_feedback: bool,
    /// Ask for a keyframe when a frame is lost (what a decoder needs to go on).
    pub keyframe_on_loss: bool,
    /// The input kinds this client sends (`INPUT_*`): a client with gamepads adds `INPUT_GAMEPAD`.
    pub input: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            name: discover::machine_name(),
            pair: None,
            webtransport: true,
            datagrams: true,
            direct: true,
            codecs: vec![CodecCap {
                codec: rr_proto::Codec::H264,
                hardware: false,
            }],
            refresh_mhz: 60_000,
            auto_feedback: true,
            keyframe_on_loss: true,
            input: INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH,
        }
    }
}

/// What comes from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The server said hello (again after the session moved to a direct path).
    Hello(Hello),
    Config(Config),
    /// One encoded frame. `timestamp_us`: its capture time on the server's clock; `received_us`: when
    /// it came, on this one's; `latency_us`: capture to arrival, estimated (the server's clock set
    /// against this one's from its pings, less half the round trip), once there's an estimate.
    Video {
        frame: u32,
        keyframe: bool,
        timestamp_us: u64,
        received_us: u64,
        latency_us: Option<i64>,
        data: Vec<u8>,
    },
    Cursor(Cursor),
    Audio(Audio),
    Clipboard(String),
    /// The session moved to another path (`Transport::label`).
    Transport(&'static str),
    /// Something worth saying (a direct path tried, and how it went).
    Note(String),
    /// The session ended (and why, when it wasn't the server closing it).
    Closed(Option<String>),
}

/// The session's numbers so far.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub transport: &'static str,
    pub frames: u64,
    pub keyframes: u64,
    pub bytes: u64,
    /// Frames that never came (gaps in their numbers).
    pub frames_lost: u64,
    /// Datagram video: blocks rebuilt from parity, frames given up on, and the share of datagrams lost
    /// lately (thousandths).
    pub fec_recovered: u64,
    pub fec_lost: u64,
    pub loss_permille: u16,
    /// Datagrams the server sent (by their sequence numbers) and those that came, all told.
    pub datagrams_expected: u64,
    pub datagrams_received: u64,
    /// The last round trip (our pings), and the lowest seen.
    pub rtt_ms: f64,
    pub rtt_min_ms: f64,
    /// The server's clock against this one: add to a server time for this clock's (µs).
    pub clock_offset_us: Option<i64>,
}

pub(crate) enum Cmd {
    Send(Message),
    /// The session's new path (punched through): hello there, then the old one goes.
    Switch(Box<Link>),
    Note(String),
    Close,
}

/// A connected session: events in, input out.
pub struct Session {
    inbox: mpsc::Sender<Ev>,
    events: mpsc::Receiver<Event>,
    stats: Arc<Mutex<Stats>>,
}

impl Session {
    /// The next event, waiting up to `wait`.
    pub fn next_event(&self, wait: Duration) -> Option<Event> {
        self.events.recv_timeout(wait).ok()
    }

    /// A handle that sends to the server from any thread (a window's input, while another thread
    /// takes the events and decodes).
    pub fn control(&self) -> Control {
        Control {
            inbox: self.inbox.clone(),
            stats: self.stats.clone(),
        }
    }

    pub fn input(&self, i: Input) {
        self.send(Message::Input(i));
    }

    /// Asks for a keyframe (the decoder lost its place, or just started).
    pub fn keyframe(&self) {
        self.send(Message::KeyframeRequest);
    }

    /// Tells the server a frame was shown (with `auto_feedback` off: after decoding it).
    pub fn feedback(&self, f: Feedback) {
        self.send(Message::Feedback(f));
    }

    pub fn send(&self, m: Message) {
        let _ = self.inbox.send(Ev::Cmd(Cmd::Send(m)));
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }

    pub fn close(&self) {
        let _ = self.inbox.send(Ev::Cmd(Cmd::Close));
    }
}

/// Sends to the server from any thread: [`Session::control`]. The session ends when the `Session`
/// is dropped (or [`Control::close`]), not when these are.
#[derive(Clone)]
pub struct Control {
    inbox: mpsc::Sender<Ev>,
    stats: Arc<Mutex<Stats>>,
}

impl Control {
    pub fn input(&self, i: Input) {
        self.send(Message::Input(i));
    }

    pub fn keyframe(&self) {
        self.send(Message::KeyframeRequest);
    }

    pub fn feedback(&self, f: Feedback) {
        self.send(Message::Feedback(f));
    }

    pub fn send(&self, m: Message) {
        let _ = self.inbox.send(Ev::Cmd(Cmd::Send(m)));
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }

    pub fn close(&self) {
        let _ = self.inbox.send(Ev::Cmd(Cmd::Close));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

/// Through a relay: where to ask for this socket's outside address, and the room.
struct Relayed {
    reflector: Option<SocketAddr>,
    room: String,
}

/// Connects to `target`: a link a server printed (`http(s)://host:port/?token=…`), a relay's link
/// (`http(s)://relay/?room=…#k=…`), a stream's WebSocket (`ws(s)://host:port/stream?token=…`), or the
/// name (or fingerprint) of a server on this network (mDNS).
pub fn connect(target: &str, opts: Options) -> Result<Session, String> {
    let target = if target.contains("://") { target.to_string() } else { find(target)? };
    let url = http::parse(&target)?;
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?,
    );
    let (tx, rx) = mpsc::channel::<Ev>();
    let ws_scheme = if url.origin.tls { "wss" } else { "ws" };
    let mut relayed = None;
    let mut notes = Vec::new();
    let link = if let Some(room) = http::param(&url.query, "room") {
        // A relay: sealed with the key after # (never sent anywhere).
        let k = http::param(&url.fragment, "k").ok_or("this relay link is missing its key (the part after #k=): use the whole link the machine printed")?;
        let key: [u8; 32] = e2e::b64url_decode(k)
            .and_then(|k| k.try_into().ok())
            .ok_or("the link's key (#k=…) isn't a key")?;
        // The relay the link names, then the others of its fleet (after #, `alt=`), in order: the
        // machine waits in its room on each.
        let mut relays = vec![format!("{ws_scheme}://{}{}", url.origin.authority(), url.path.trim_end_matches('/'))];
        relays.extend(
            http::param(&url.fragment, "alt")
                .into_iter()
                .flat_map(|a| a.split(','))
                .filter(|a| !a.is_empty())
                .map(|a| a.trim_end_matches('/').to_string()),
        );
        let mut tried = Vec::new();
        let mut link = None;
        for relay in &relays {
            match link::websocket(&format!("{relay}/stream?room={room}"), tx.clone(), Transport::Relay) {
                Ok(l) => {
                    relayed = Some(Relayed {
                        reflector: direct::reflector(relay),
                        room: room.to_string(),
                    });
                    if !tried.is_empty() {
                        notes.push(format!("through {relay} ({})", tried.join("; ")));
                    }
                    link = Some(l);
                    break;
                }
                Err(e) => tried.push(format!("{relay}: {e}")),
            }
        }
        link.ok_or_else(|| tried.join("; "))?.sealed(key)
    } else if target.starts_with("ws") {
        link::websocket(&target, tx.clone(), Transport::WebSocket)?
    } else {
        let info = http::info(&url.origin).unwrap_or_default();
        let mut query = url.query.clone();
        // No token in the link: sign in with this client's key (pairing first, given the code).
        if http::param(&query, "token").is_none() {
            if let Some(id) = &info.id {
                if let Some(code) = &opts.pair {
                    if pairing::paired(id).is_none() {
                        let got = pairing::pair(&url.origin, code, &opts.name)?;
                        if &got != id {
                            return Err(format!("paired with {got}, but the server says it's {id}"));
                        }
                        notes.push(format!("paired with {id}"));
                    }
                }
                let ticket = pairing::sign_in(&url.origin, id)?;
                query = format!("ticket={ticket}");
            }
        }
        let wt = info.webtransport.filter(|_| opts.webtransport).and_then(|(port, hash)| {
            let wurl = format!("https://{}:{port}/stream?{query}", url.origin.host);
            let cert = hash.map_or(Cert::Roots, Cert::Hash);
            link::webtransport(rt.clone(), &[wurl], cert, None, Duration::from_secs(3), tx.clone(), Transport::WebTransport)
                .map_err(|e| notes.push(format!("no WebTransport ({e}): the WebSocket instead")))
                .ok()
        });
        match wt {
            Some(l) => l,
            None => link::websocket(
                &format!("{ws_scheme}://{}/stream?{query}", url.origin.authority()),
                tx.clone(),
                Transport::WebSocket,
            )?,
        }
    };
    let (ev_tx, ev_rx) = mpsc::channel();
    for n in notes {
        let _ = ev_tx.send(Event::Note(n));
    }
    let stats = Arc::new(Mutex::new(Stats {
        transport: link.transport.label(false),
        ..Default::default()
    }));
    let s = Run {
        hello: hello(&opts, relayed.is_some() && opts.direct),
        opts,
        link,
        events: ev_tx,
        stats: stats.clone(),
        inbox: tx.clone(),
        rt,
        relayed,
        upgrading: false,
        last_frame: None,
        missing: HashMap::new(),
        lost_since_feedback: 0,
        last_keyframe_ask: Instant::now() - Duration::from_secs(1),
        pings: HashMap::new(),
        ping_id: 0,
        offsets: VecDeque::new(),
    };
    std::thread::spawn(move || s.run(rx));
    Ok(Session {
        inbox: tx,
        events: ev_rx,
        stats,
    })
}

/// A server on this network by its name or fingerprint (mDNS): its link.
fn find(name: &str) -> Result<String, String> {
    let found = discover::discover(Duration::from_secs(3))?;
    let f = found
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(name) || (!name.is_empty() && f.id.starts_with(name)))
        .ok_or_else(|| {
            let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
            format!(
                "no server called {name} on this network (found: {})",
                if names.is_empty() { "none".into() } else { names.join(", ") }
            )
        })?;
    f.link()
}

fn hello(opts: &Options, direct: bool) -> Vec<u8> {
    Message::Hello(Hello {
        version_min: VERSION_MIN,
        version_max: VERSION_MAX,
        name: opts.name.clone(),
        codecs: opts.codecs.clone(),
        max_width: 3840,
        max_height: 2160,
        max_fps: 240,
        audio: false,
        input: opts.input,
        refresh_mhz: opts.refresh_mhz,
        features: if opts.datagrams { FEATURE_VIDEO_DATAGRAMS } else { 0 } | if direct { FEATURE_DIRECT } else { 0 },
    })
    .encode()
}

/// The session's own thread.
struct Run {
    opts: Options,
    link: Link,
    hello: Vec<u8>,
    events: mpsc::Sender<Event>,
    stats: Arc<Mutex<Stats>>,
    inbox: mpsc::Sender<Ev>,
    rt: Arc<tokio::runtime::Runtime>,
    relayed: Option<Relayed>,
    upgrading: bool,
    last_frame: Option<u32>,
    missing: HashMap<u32, Instant>,
    lost_since_feedback: u16,
    last_keyframe_ask: Instant,
    /// Our pings out (id → when, µs), and the server's clock against ours from its pings (lowest of the
    /// last ones: the least delayed).
    pings: HashMap<u32, u64>,
    ping_id: u32,
    offsets: VecDeque<i64>,
}

impl Run {
    fn run(mut self, inbox: mpsc::Receiver<Ev>) {
        let first = self.hello.clone();
        if let Err(e) = self.link.send(first) {
            let _ = self.events.send(Event::Closed(Some(e)));
            return;
        }
        let mut next_ping = Instant::now();
        loop {
            if Instant::now() >= next_ping {
                next_ping = Instant::now() + Duration::from_secs(1);
                self.ping_id = self.ping_id.wrapping_add(1);
                let t = now_us();
                self.pings.insert(self.ping_id, t);
                self.pings.retain(|_, s| t - *s < 10_000_000);
                let _ = self.link.send(Message::Ping { id: self.ping_id, sent_us: t }.encode());
            }
            let ev = match inbox.recv_timeout(next_ping.saturating_duration_since(Instant::now())) {
                Ok(ev) => ev,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            let result = match ev {
                Ev::Net(id, In::Data(b)) if id == self.link.id => self.data(b),
                Ev::Net(id, In::Closed(why)) if id == self.link.id => Err(why.unwrap_or_default()),
                // A link this session already left.
                Ev::Net(..) => Ok(()),
                Ev::Cmd(Cmd::Send(m)) => self.link.send(m.encode()),
                Ev::Cmd(Cmd::Switch(l)) => self.switch(*l),
                Ev::Cmd(Cmd::Note(n)) => {
                    let _ = self.events.send(Event::Note(n));
                    Ok(())
                }
                Ev::Cmd(Cmd::Close) => {
                    self.link.close();
                    let _ = self.events.send(Event::Closed(None));
                    return;
                }
            };
            if let Err(e) = result {
                self.link.close();
                let _ = self.events.send(Event::Closed((!e.is_empty()).then_some(e)));
                return;
            }
        }
    }

    fn data(&mut self, b: Vec<u8>) -> Result<(), String> {
        let Some(b) = self.link.open(b)? else { return Ok(()) };
        let mut at = 0;
        while at < b.len() {
            let (m, used) = Message::decode(&b[at..]).map_err(|e| e.to_string())?;
            at += used;
            let Some(m) = m else { continue };
            match m {
                Message::Video(v) => self.video(v)?,
                Message::Ping { id, sent_us } => {
                    // The server's clock against ours: lowest of the last 30 (the least delayed).
                    self.offsets.push_back(now_us() as i64 - sent_us as i64);
                    if self.offsets.len() > 30 {
                        self.offsets.pop_front();
                    }
                    self.link.send(Message::Pong { id, sent_us }.encode())?;
                }
                Message::Pong { id, sent_us } => {
                    if self.pings.remove(&id) == Some(sent_us) {
                        let rtt = (now_us().saturating_sub(sent_us)) as f64 / 1000.0;
                        let mut s = self.stats.lock().unwrap();
                        s.rtt_ms = rtt;
                        s.rtt_min_ms = if s.rtt_min_ms == 0.0 { rtt } else { s.rtt_min_ms.min(rtt) };
                        if let Some(&least) = self.offsets.iter().min() {
                            s.clock_offset_us = Some(least - (s.rtt_min_ms * 500.0) as i64);
                        }
                    }
                }
                Message::DirectOffer(o) => self.offered(o),
                Message::Hello(h) => {
                    let _ = self.events.send(Event::Hello(h));
                }
                Message::Config(c) => {
                    let _ = self.events.send(Event::Config(c));
                }
                Message::Cursor(c) => {
                    let _ = self.events.send(Event::Cursor(c));
                }
                Message::Audio(a) => {
                    let _ = self.events.send(Event::Audio(a));
                }
                Message::Clipboard { text } => {
                    let _ = self.events.send(Event::Clipboard(text));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn video(&mut self, v: rr_proto::Video) -> Result<(), String> {
        let received_us = now_us();
        // Frames skipped in the numbering are missing; one that comes late (a stream a frame can
        // overtake another) fills its gap. Missing for 100 ms: lost.
        let ahead = self.last_frame.map_or(1, |l| v.frame.wrapping_sub(l) as i32);
        if (1..1000).contains(&ahead) {
            let last = self.last_frame.unwrap_or(v.frame);
            for n in 1..ahead as u32 {
                self.missing.insert(last.wrapping_add(n), Instant::now());
            }
            self.last_frame = Some(v.frame);
        } else if (-1000..=0).contains(&ahead) {
            self.missing.remove(&v.frame);
        } else {
            // Numbers far off: the server started over (a new session on another path).
            self.missing.clear();
            self.last_frame = Some(v.frame);
        }
        let before = self.missing.len();
        self.missing.retain(|_, t| t.elapsed() < Duration::from_millis(100));
        let lost = (before - self.missing.len()) as u64;
        self.lost_since_feedback = self.lost_since_feedback.saturating_add(lost as u16);
        if lost > 0 && !v.keyframe && self.opts.keyframe_on_loss && self.last_keyframe_ask.elapsed() > Duration::from_millis(250) {
            // A decoder can't go on past a lost frame without one.
            self.last_keyframe_ask = Instant::now();
            self.link.send(Message::KeyframeRequest.encode())?;
        }
        let latency_us = {
            let mut s = self.stats.lock().unwrap();
            s.frames += 1;
            s.keyframes += v.keyframe as u64;
            s.bytes += v.data.len() as u64;
            s.frames_lost += lost;
            if let Some(f) = &self.link.fec {
                let f = f.lock().unwrap();
                (s.fec_recovered, s.fec_lost) = (f.recovered, f.lost);
                (s.datagrams_expected, s.datagrams_received) = (f.expected_total, f.received_total);
            }
            if let Some(d) = &self.link.datagrams {
                s.transport = self.link.transport.label(d.load(std::sync::atomic::Ordering::Relaxed));
            }
            s.clock_offset_us.map(|o| received_us as i64 - (v.timestamp_us as i64 + o))
        };
        // The share of datagrams lost lately: in the stats either way (a client that sends its own
        // feedback reports it from there).
        let loss_permille = self.link.loss_permille();
        self.stats.lock().unwrap().loss_permille = loss_permille;
        if self.opts.auto_feedback {
            self.link.send(
                Message::Feedback(Feedback {
                    frame: v.frame,
                    received_us,
                    decoded_us: received_us,
                    lost: std::mem::take(&mut self.lost_since_feedback),
                    loss_permille,
                })
                .encode(),
            )?;
        }
        let _ = self.events.send(Event::Video {
            frame: v.frame,
            keyframe: v.keyframe,
            timestamp_us: v.timestamp_us,
            received_us,
            latency_us,
            data: v.data,
        });
        Ok(())
    }

    /// The host offered a direct path: punch for it on a thread of its own, the relay carrying on
    /// meanwhile.
    fn offered(&mut self, offer: DirectOffer) {
        let Some(r) = self.relayed.as_ref().filter(|_| self.opts.direct && !self.upgrading) else {
            return;
        };
        self.upgrading = true;
        let (reflector, room, rt, inbox) = (r.reflector, r.room.clone(), self.rt.clone(), self.inbox.clone());
        std::thread::spawn(move || {
            let note = |n: String| {
                let _ = inbox.send(Ev::Cmd(Cmd::Note(n)));
            };
            match punch_through(offer, reflector, &room, rt, inbox.clone()) {
                Ok(link) => {
                    let _ = inbox.send(Ev::Cmd(Cmd::Switch(Box::new(link))));
                }
                Err(e) => note(format!("no direct path ({e}): staying on the relay")),
            }
        });
    }

    fn switch(&mut self, mut link: Link) -> Result<(), String> {
        let mut hello = self.hello.clone();
        // Hello on the new path (no direct path to offer on a direct path).
        if let Ok((Some(Message::Hello(mut h)), _)) = Message::decode(&hello) {
            h.features &= !FEATURE_DIRECT;
            hello = Message::Hello(h).encode();
        }
        link.send(hello)?;
        let mut old = std::mem::replace(&mut self.link, link);
        old.close();
        self.relayed = None;
        self.last_frame = None;
        self.missing.clear();
        let label = self.link.transport.label(false);
        self.stats.lock().unwrap().transport = label;
        let _ = self.events.send(Event::Transport(label));
        Ok(())
    }
}

/// The viewer's side of a direct path: its own candidates to the host (through the relay, sealed),
/// punch packets both ways, then WebTransport straight to the host from the same socket, checking the
/// certificate against the offer's hash.
fn punch_through(
    offer: DirectOffer,
    reflector: Option<SocketAddr>,
    room: &str,
    rt: Arc<tokio::runtime::Runtime>,
    inbox: mpsc::Sender<Ev>,
) -> Result<Link, String> {
    let hash: [u8; 32] = offer.cert_sha256.as_slice().try_into().map_err(|_| "the offer has no certificate hash")?;
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    let mut mine = Vec::new();
    if let Some(r) = reflector {
        match direct::probe(&socket, r, room, Duration::from_millis(1500)) {
            Some(a) => mine.push(a),
            None => {
                let _ = inbox.send(Ev::Cmd(Cmd::Note(format!(
                    "the relay ({r}) didn't say what address it sees over UDP: trying this network only"
                ))));
            }
        }
    }
    mine.extend(direct::local_candidates(port, true));
    let _ = inbox.send(Ev::Cmd(Cmd::Send(Message::DirectAnswer(DirectAnswer { candidates: mine.clone() }))));
    let mut theirs: Vec<SocketAddr> = offer.candidates.iter().copied().filter(SocketAddr::is_ipv4).collect();
    if theirs.is_empty() {
        return Err("the host gave no address to try".into());
    }
    let tag = rr_proto::punch::tag(&offer.ticket);
    let heard = direct::punch_and_listen(&socket, &theirs, tag, Duration::from_millis(1500));
    if let Some(h) = heard {
        theirs.retain(|a| *a != h);
        theirs.insert(0, h);
    }
    let _ = inbox.send(Ev::Cmd(Cmd::Note(format!(
        "punching: this side {}, the host {}{}",
        list(&mine),
        list(&theirs),
        heard.map_or(" (nothing heard back yet)".into(), |h| format!(" (heard from {h})"))
    ))));
    let urls: Vec<String> = theirs.iter().map(|a| format!("https://{a}/stream?ticket={}&direct=1", offer.ticket)).collect();
    link::webtransport(rt, &urls, Cert::Hash(hash), Some(socket), Duration::from_secs(5), inbox, Transport::Direct)
}

fn list(a: &[SocketAddr]) -> String {
    a.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
}
