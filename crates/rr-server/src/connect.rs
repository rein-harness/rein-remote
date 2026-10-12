//! `rein-remote connect`: a native viewer without a screen, for measuring. It connects the way the
//! desktop apps will (rr-client), takes the stream without decoding it, tells the server what arrived
//! (so the server adapts as it would for a real viewer), and prints what it saw for each path the
//! session took: the transport, frames a second, bitrate, loss, frames rebuilt from parity, the round
//! trip, and capture to arrival.
//!
//!   rein-remote connect http://box:7979/?token=…            # WebTransport, else the WebSocket
//!   rein-remote connect 'https://relay.example/?room=…#k=…'  # through a relay, then direct if it can
//!   rein-remote connect build-box --pair 123-456             # by name on this network, pairing once

use rr_client::{Event, Options};
use std::time::{Duration, Instant};

/// One path the session took, and what came over it.
struct Phase {
    transport: &'static str,
    first_frame: Option<Instant>,
    last_frame: Option<Instant>,
    frames: u64,
    keyframes: u64,
    bytes: u64,
    /// Capture to arrival, ms: estimated (clocks set against each other from pings), and as the
    /// clocks say (right when both ends share one: the same machine, containers on one host).
    latency: Vec<f64>,
    latency_same_clock: Vec<f64>,
    rtt: Vec<f64>,
    /// The session's counters when this path began (they run across paths), and since.
    lost_at_start: u64,
    fec_at_start: (u64, u64),
    datagrams_at_start: (u64, u64),
    lost: u64,
    fec: (u64, u64),
    datagrams: (u64, u64),
}

impl Phase {
    fn new(transport: &'static str, s: &rr_client::Stats) -> Self {
        Self {
            transport,
            first_frame: None,
            last_frame: None,
            frames: 0,
            keyframes: 0,
            bytes: 0,
            latency: vec![],
            latency_same_clock: vec![],
            rtt: vec![],
            lost_at_start: s.frames_lost,
            fec_at_start: (s.fec_recovered, s.fec_lost),
            datagrams_at_start: (s.datagrams_expected, s.datagrams_received),
            lost: 0,
            fec: (0, 0),
            datagrams: (0, 0),
        }
    }

    fn update(&mut self, s: &rr_client::Stats) {
        self.lost = s.frames_lost - self.lost_at_start;
        self.fec = (
            s.fec_recovered.saturating_sub(self.fec_at_start.0),
            s.fec_lost.saturating_sub(self.fec_at_start.1),
        );
        self.datagrams = (
            s.datagrams_expected.saturating_sub(self.datagrams_at_start.0),
            s.datagrams_received.saturating_sub(self.datagrams_at_start.1),
        );
    }

    /// Of the datagrams sent on this path, the share lost on the way, in thousandths.
    fn loss_permille(&self) -> f64 {
        let (expected, got) = self.datagrams;
        if expected == 0 {
            0.0
        } else {
            (expected - got.min(expected)) as f64 * 1000.0 / expected as f64
        }
    }

    /// The time frames came over (first to last), seconds.
    fn seconds(&self) -> f64 {
        match (self.first_frame, self.last_frame) {
            (Some(a), Some(b)) if b > a => (b - a).as_secs_f64(),
            _ => 0.0,
        }
    }

    fn json(&self) -> String {
        let secs = self.seconds();
        let per = |n: f64| if secs > 0.0 { n / secs } else { 0.0 };
        format!(
            r#"{{"transport":"{}","seconds":{:.1},"frames":{},"keyframes":{},"fps":{:.1},"kbps":{:.0},"frames_lost":{},"fec_recovered":{},"fec_lost":{},"datagrams_lost_permille":{:.1},"rtt_ms":{{"p50":{:.1},"min":{:.1}}},"latency_ms":{{"p50":{:.1},"p95":{:.1},"samples":{}}},"latency_same_clock_ms":{{"p50":{:.1},"p95":{:.1}}}}}"#,
            self.transport,
            secs,
            self.frames,
            self.keyframes,
            per(self.frames.saturating_sub(1) as f64),
            per(self.bytes as f64 * 8.0 / 1000.0),
            self.lost,
            self.fec.0,
            self.fec.1,
            self.loss_permille(),
            pct(&self.rtt, 0.5),
            self.rtt.iter().copied().fold(f64::NAN, f64::min).max(0.0),
            pct(&self.latency, 0.5),
            pct(&self.latency, 0.95),
            self.latency.len(),
            pct(&self.latency_same_clock, 0.5),
            pct(&self.latency_same_clock, 0.95),
        )
    }

    fn text(&self) -> String {
        let secs = self.seconds();
        let per = |n: f64| if secs > 0.0 { n / secs } else { 0.0 };
        format!(
            "{:<22} {:>5.1} s  {:>5.1} fps  {:>6.0} kbit/s  lost {} frames (fec: {} rebuilt, {} given up; {:.1}‰ datagrams)  rtt {:.1} ms  capture→arrival p50 {:.1} ms p95 {:.1} ms (same clock: {:.1} / {:.1})",
            self.transport,
            secs,
            per(self.frames.saturating_sub(1) as f64),
            per(self.bytes as f64 * 8.0 / 1000.0),
            self.lost,
            self.fec.0,
            self.fec.1,
            self.loss_permille(),
            pct(&self.rtt, 0.5),
            pct(&self.latency, 0.5),
            pct(&self.latency, 0.95),
            pct(&self.latency_same_clock, 0.5),
            pct(&self.latency_same_clock, 0.95),
        )
    }
}

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p) as usize]
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut target = None;
    let mut seconds = 10u64;
    let mut json = false;
    let mut opts = Options::default();
    let mut it = args.iter();
    let on = |v: String| !matches!(v.as_str(), "off" | "false" | "no");
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--seconds" => seconds = val()?.parse().map_err(|_| "--seconds N")?,
            "--pair" => opts.pair = Some(val()?),
            "--json" => json = true,
            "--webtransport" => opts.webtransport = on(val()?),
            "--datagrams" => opts.datagrams = on(val()?),
            "--direct" => opts.direct = on(val()?),
            "--name" => opts.name = val()?,
            t if !t.starts_with("--") && target.is_none() => target = Some(t.to_string()),
            _ => return Err(format!("connect: {a}? (rein-remote connect URL|NAME [--seconds N] [--pair CODE] [--json] [--webtransport on|off] [--datagrams on|off] [--direct on|off])")),
        }
    }
    let target = target.ok_or("connect: which server? (a link it printed, a relay's link, or its name on this network)")?;
    let started = Instant::now();
    let session = rr_client::connect(&target, opts)?;
    let mut phases = vec![Phase::new(session.stats().transport, &session.stats())];
    let mut notes: Vec<String> = Vec::new();
    let end = started + Duration::from_secs(seconds);
    let mut next_sample = Instant::now() + Duration::from_secs(1);
    let mut closed = None;
    while Instant::now() < end {
        let ev = session.next_event(Duration::from_millis(100).min(end.saturating_duration_since(Instant::now())));
        let s = session.stats();
        let phase = phases.last_mut().unwrap();
        match ev {
            Some(Event::Video {
                keyframe,
                timestamp_us,
                received_us,
                latency_us,
                data,
                ..
            }) => {
                let now = Instant::now();
                phase.first_frame.get_or_insert(now);
                phase.last_frame = Some(now);
                phase.frames += 1;
                phase.keyframes += keyframe as u64;
                phase.bytes += data.len() as u64;
                // WebTransport's label says how video comes, known once it does.
                if phase.transport.starts_with("webtransport") {
                    phase.transport = s.transport;
                }
                if let Some(l) = latency_us {
                    phase.latency.push(l as f64 / 1000.0);
                }
                phase.latency_same_clock.push((received_us as i64 - timestamp_us as i64) as f64 / 1000.0);
            }
            Some(Event::Transport(t)) => {
                phase.update(&s);
                let msg = format!("now on {t} (after {:.1} s)", started.elapsed().as_secs_f64());
                eprintln!("rein-remote: {msg}");
                notes.push(msg);
                phases.push(Phase::new(t, &s));
            }
            Some(Event::Note(n)) => {
                eprintln!("rein-remote: {n}");
                notes.push(n);
            }
            Some(Event::Config(c)) => eprintln!(
                "rein-remote: {:?} {}×{} at {} fps, {} kbit/s",
                c.codec, c.width, c.height, c.fps, c.bitrate_kbps
            ),
            Some(Event::Hello(h)) => eprintln!("rein-remote: connected to {} over {}", h.name, s.transport),
            Some(Event::Closed(why)) => {
                closed = Some(why.unwrap_or_else(|| "the server closed the session".into()));
                break;
            }
            _ => {}
        }
        if Instant::now() >= next_sample {
            next_sample += Duration::from_secs(1);
            let phase = phases.last_mut().unwrap();
            if s.rtt_ms > 0.0 {
                phase.rtt.push(s.rtt_ms);
            }
            phase.update(&s);
            if !json {
                eprintln!("  {}", phase.text());
            }
        }
    }
    let s = session.stats();
    phases.last_mut().unwrap().update(&s);
    session.close();
    let phases: Vec<&Phase> = phases.iter().filter(|p| p.frames > 0).collect();
    if json {
        let notes: Vec<String> = notes.iter().map(|n| format!("\"{}\"", n.replace('\\', "\\\\").replace('"', "\\\""))).collect();
        println!(
            r#"{{"target":"{}","transport":"{}","phases":[{}],"notes":[{}],"closed":{}}}"#,
            rr_client::redact(&target).replace('"', ""),
            s.transport,
            phases.iter().map(|p| p.json()).collect::<Vec<_>>().join(","),
            notes.join(","),
            closed.as_deref().map_or("null".into(), |c| format!("\"{}\"", c.replace('"', "'")))
        );
    } else {
        println!(
            "rein-remote connect: {} for {:.1} s",
            rr_client::redact(&target),
            started.elapsed().as_secs_f64()
        );
        for p in &phases {
            println!("  {}", p.text());
        }
        if let Some(c) = &closed {
            println!("  closed: {c}");
        }
    }
    match (phases.is_empty(), closed) {
        (true, Some(c)) => Err(c),
        (true, None) => Err("no frames came".into()),
        _ => Ok(()),
    }
}
