//! Health and metrics, for whoever runs a server or a relay as a service: `/healthz` (alive, its
//! version, how long it's been up, how many sessions) and `/metrics` (the same and more, in
//! Prometheus's text format).
//!
//! Counts only: never a room code, a token, a key or a device's fingerprint. A relay is labelled by
//! its host and port (what `--relay` was given, without anything after `?` or `#`).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// A number that only goes up.
pub struct Counter(AtomicU64);

impl Counter {
    const fn new() -> Counter {
        Counter(AtomicU64::new(0))
    }
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Relaxed);
    }
    pub fn inc(&self) {
        self.add(1)
    }
    pub fn get(&self) -> u64 {
        self.0.load(Relaxed)
    }
}

/// A number that goes up and down.
pub struct Gauge(AtomicI64);

impl Gauge {
    const fn new() -> Gauge {
        Gauge(AtomicI64::new(0))
    }
    pub fn get(&self) -> i64 {
        self.0.load(Relaxed)
    }
    pub fn set(&self, v: i64) {
        self.0.store(v, Relaxed)
    }
    /// One more for as long as what's returned lives (an error return or a panic counts it down too).
    pub fn hold(&'static self) -> Held {
        self.0.fetch_add(1, Relaxed);
        Held(self)
    }
}

pub struct Held(&'static Gauge);

impl Drop for Held {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Relaxed);
    }
}

static STARTED: OnceLock<Instant> = OnceLock::new();

/// From when uptime counts (called first thing by `serve` and `relay`).
pub fn start() {
    STARTED.get_or_init(Instant::now);
}

pub fn uptime_s() -> f64 {
    STARTED.get_or_init(Instant::now).elapsed().as_secs_f64()
}

// `serve`.
/// Viewers watching now: straight to this machine (WebSocket, WebTransport) or through a relay.
pub static VIEWERS_DIRECT: Gauge = Gauge::new();
pub static VIEWERS_RELAYED: Gauge = Gauge::new();
pub static SESSIONS: Counter = Counter::new();
pub static FRAMES: Counter = Counter::new();
pub static KEYFRAMES: Counter = Counter::new();
pub static VIDEO_BYTES: Counter = Counter::new();
pub static SKIPPED_BACKED_UP: Counter = Counter::new();
/// Relayed viewers offered a direct path, and those that answered with theirs (the punch starts).
pub static DIRECT_OFFERS: Counter = Counter::new();
pub static DIRECT_ANSWERS: Counter = Counter::new();
/// Each relay `serve` was given: whether it answers (`/healthz`), its round trip, how many times this
/// machine took its room there (the first time, then again after it went).
static RELAYS: Mutex<Vec<RelayState>> = Mutex::new(Vec::new());

#[derive(Clone, Debug, PartialEq)]
pub struct RelayState {
    pub name: String,
    pub up: bool,
    pub rtt_ms: Option<f64>,
    pub registered: bool,
    pub registrations: u64,
}

/// `name` as a label: the relay's host and port only.
pub fn relay_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).to_string()
}

pub fn relay_update(url: &str, f: impl FnOnce(&mut RelayState)) {
    let name = relay_name(url);
    let mut all = RELAYS.lock().unwrap();
    let i = match all.iter().position(|r| r.name == name) {
        Some(i) => i,
        None => {
            all.push(RelayState {
                name,
                up: false,
                rtt_ms: None,
                registered: false,
                registrations: 0,
            });
            all.len() - 1
        }
    };
    f(&mut all[i]);
}

pub fn relays() -> Vec<RelayState> {
    RELAYS.lock().unwrap().clone()
}

// `relay`.
pub static RELAY_HOSTS_WAITING: Gauge = Gauge::new();
pub static RELAY_SESSIONS: Gauge = Gauge::new();
pub static RELAY_SESSIONS_TOTAL: Counter = Counter::new();
pub static RELAY_BYTES_TO_VIEWERS: Counter = Counter::new();
pub static RELAY_BYTES_TO_HOSTS: Counter = Counter::new();
pub static RELAY_RENDEZVOUS: Counter = Counter::new();
pub static RELAY_REFUSED_SECRET: Counter = Counter::new();
pub static RELAY_REFUSED_ROOM: Counter = Counter::new();
pub static RELAY_NO_HOST: Counter = Counter::new();

fn metric<L: AsRef<str>>(out: &mut String, name: &str, kind: &str, help: &str, samples: &[(L, String)]) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
    for (labels, v) in samples {
        let labels = labels.as_ref();
        if labels.is_empty() {
            let _ = writeln!(out, "{name} {v}");
        } else {
            let _ = writeln!(out, "{name}{{{labels}}} {v}");
        }
    }
}

/// A label's value, escaped as Prometheus's text format wants.
fn label(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn info(out: &mut String, role: &str) {
    metric(
        out,
        "rein_remote_build_info",
        "gauge",
        "This build's version, and whether this is a server or a relay.",
        &[(&format!("version=\"{}\",role=\"{role}\"", env!("CARGO_PKG_VERSION")), "1".into())],
    );
    metric(
        out,
        "rein_remote_uptime_seconds",
        "gauge",
        "Seconds since it started.",
        &[("", format!("{:.0}", uptime_s()))],
    );
}

/// `serve`'s `/metrics`.
pub fn serve_prometheus() -> String {
    let mut out = String::new();
    info(&mut out, "serve");
    metric(
        &mut out,
        "rein_remote_viewers",
        "gauge",
        "Viewers watching now, by how they reach this machine.",
        &[
            ("path=\"direct\"", VIEWERS_DIRECT.get().to_string()),
            ("path=\"relay\"", VIEWERS_RELAYED.get().to_string()),
        ],
    );
    for (name, help, c) in [
        ("rein_remote_sessions_total", "Viewers that have started a session.", &SESSIONS),
        ("rein_remote_frames_sent_total", "Video frames sent.", &FRAMES),
        ("rein_remote_keyframes_sent_total", "Keyframes sent.", &KEYFRAMES),
        ("rein_remote_video_bytes_sent_total", "Bytes of encoded video sent.", &VIDEO_BYTES),
        (
            "rein_remote_frames_skipped_total",
            "Frames not sent because the viewer's link was backed up.",
            &SKIPPED_BACKED_UP,
        ),
        (
            "rein_remote_direct_offers_total",
            "Relayed viewers offered a direct path through NAT.",
            &DIRECT_OFFERS,
        ),
        (
            "rein_remote_direct_answers_total",
            "Relayed viewers that answered with their own candidates (hole punching started).",
            &DIRECT_ANSWERS,
        ),
    ] {
        metric(&mut out, name, "counter", help, &[("", c.get().to_string())]);
    }
    let relays = relays();
    if !relays.is_empty() {
        let each =
            |f: &dyn Fn(&RelayState) -> String| -> Vec<(String, String)> { relays.iter().map(|r| (format!("relay=\"{}\"", label(&r.name)), f(r))).collect() };
        let up = each(&|r| (r.up as u8).to_string());
        metric(&mut out, "rein_remote_relay_up", "gauge", "Whether the relay answers its /healthz.", &up);
        let reg = each(&|r| (r.registered as u8).to_string());
        metric(
            &mut out,
            "rein_remote_relay_registered",
            "gauge",
            "Whether this machine's room is open on the relay now (waiting there, or a viewer in it).",
            &reg,
        );
        let rtt: Vec<(String, String)> = relays
            .iter()
            .filter_map(|r| Some((format!("relay=\"{}\"", label(&r.name)), format!("{:.4}", r.rtt_ms? / 1000.0))))
            .collect();
        metric(
            &mut out,
            "rein_remote_relay_rtt_seconds",
            "gauge",
            "The last /healthz round trip to the relay.",
            &rtt,
        );
        let n = each(&|r| r.registrations.to_string());
        metric(
            &mut out,
            "rein_remote_relay_registrations_total",
            "counter",
            "Times this machine opened its room on the relay (again after each viewer, and after the relay came back).",
            &n,
        );
    }
    out
}

/// `serve`'s `/healthz`.
pub fn serve_healthz() -> String {
    let relays = relays();
    format!(
        r#"{{"status":"ok","role":"serve","version":"{}","uptime_s":{:.0},"sessions":{},"relays":{},"relays_up":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        uptime_s(),
        VIEWERS_DIRECT.get() + VIEWERS_RELAYED.get(),
        relays.len(),
        relays.iter().filter(|r| r.up).count()
    )
}

/// The relay's `/metrics`.
pub fn relay_prometheus() -> String {
    let mut out = String::new();
    info(&mut out, "relay");
    for (name, help, v) in [
        (
            "rein_remote_relay_hosts_waiting",
            "Machines waiting in their room for a viewer.",
            RELAY_HOSTS_WAITING.get(),
        ),
        ("rein_remote_relay_sessions", "Sessions being relayed now.", RELAY_SESSIONS.get()),
    ] {
        metric(&mut out, name, "gauge", help, &[("", v.to_string())]);
    }
    metric(
        &mut out,
        "rein_remote_relay_sessions_total",
        "counter",
        "Sessions relayed (a viewer met its machine).",
        &[("", RELAY_SESSIONS_TOTAL.get().to_string())],
    );
    metric(
        &mut out,
        "rein_remote_relay_bytes_total",
        "counter",
        "Bytes relayed (sealed end to end: the relay can't read them).",
        &[
            ("direction=\"to_viewer\"", RELAY_BYTES_TO_VIEWERS.get().to_string()),
            ("direction=\"to_host\"", RELAY_BYTES_TO_HOSTS.get().to_string()),
        ],
    );
    metric(
        &mut out,
        "rein_remote_relay_rendezvous_total",
        "counter",
        "UDP rendezvous probes answered (a host or viewer learning its address beyond NAT).",
        &[("", RELAY_RENDEZVOUS.get().to_string())],
    );
    metric(
        &mut out,
        "rein_remote_relay_refused_total",
        "counter",
        "Requests refused: a host with the wrong secret, a room code too short, a viewer whose room had no machine.",
        &[
            ("reason=\"secret\"", RELAY_REFUSED_SECRET.get().to_string()),
            ("reason=\"room\"", RELAY_REFUSED_ROOM.get().to_string()),
            ("reason=\"no_host\"", RELAY_NO_HOST.get().to_string()),
        ],
    );
    out
}

/// The relay's `/healthz`.
pub fn relay_healthz() -> String {
    format!(
        r#"{{"status":"ok","role":"relay","version":"{}","uptime_s":{:.0},"sessions":{},"hosts_waiting":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        uptime_s(),
        RELAY_SESSIONS.get(),
        RELAY_HOSTS_WAITING.get()
    )
}

/// The `Authorization: Bearer …` header's value, if the request has one (how Prometheus sends a
/// token: `authorization: {credentials_file: …}`), so the token needn't be in the scrape's URL.
pub fn bearer(head: &str) -> Option<&str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("authorization") {
            return None;
        }
        let v = v.trim();
        let (scheme, token) = v.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then_some(token.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_text_is_well_formed_and_has_no_secrets() {
        relay_update("wss://relay.example:443/?secret=hunter2#k=key", |r| {
            r.up = true;
            r.rtt_ms = Some(12.5);
        });
        let _held = VIEWERS_RELAYED.hold();
        let text = serve_prometheus();
        for l in text.lines() {
            if l.starts_with('#') {
                assert!(l.starts_with("# HELP rein_remote_") || l.starts_with("# TYPE rein_remote_"), "{l}");
                continue;
            }
            let (name, value) = l.rsplit_once(' ').unwrap();
            assert!(name.starts_with("rein_remote_"), "{l}");
            assert!(value.parse::<f64>().is_ok(), "{l}");
        }
        assert!(text.contains("rein_remote_relay_up{relay=\"relay.example:443\"} 1"), "{text}");
        assert!(text.contains("rein_remote_relay_rtt_seconds{relay=\"relay.example:443\"} 0.0125"), "{text}");
        assert!(text.contains("rein_remote_viewers{path=\"relay\"} 1"), "{text}");
        assert!(!text.contains("hunter2") && !text.contains("k=key"), "{text}");
        assert!(relay_prometheus().contains("rein_remote_relay_bytes_total{direction=\"to_viewer\"}"));
        assert!(serve_healthz().starts_with(r#"{"status":"ok","role":"serve""#));
    }

    #[test]
    fn held_gauges_count_down() {
        let before = VIEWERS_DIRECT.get();
        {
            let _a = VIEWERS_DIRECT.hold();
            let _b = VIEWERS_DIRECT.hold();
            assert_eq!(VIEWERS_DIRECT.get(), before + 2);
        }
        assert_eq!(VIEWERS_DIRECT.get(), before);
    }

    #[test]
    fn bearer_tokens_are_read_from_the_head() {
        let head = "GET /metrics HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer abc123\r\n\r\n";
        assert_eq!(bearer(head), Some("abc123"));
        assert_eq!(bearer("GET / HTTP/1.1\r\nauthorization: basic x\r\n"), None);
        assert_eq!(bearer("GET / HTTP/1.1\r\n\r\n"), None);
    }
}
