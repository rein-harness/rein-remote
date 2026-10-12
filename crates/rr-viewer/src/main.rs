//! `rein-remote-viewer`: Rein Remote's desktop client (Windows, macOS, Linux). It shows a server's
//! stream in a window, decoded by the machine's hardware where it can (VideoToolbox, Media Foundation,
//! VA-API) and by openh264 where it can't, draws the remote pointer itself, and sends the mouse, the
//! keyboard and gamepads back. The connection is rr-client's: WebTransport with datagrams, the
//! WebSocket, or a relay and then straight through NAT.
//!
//!   rein-remote-viewer                                   # pick a machine found on this network
//!   rein-remote-viewer http://box:7979/?token=…          # a link the server printed
//!   rein-remote-viewer 'https://relay.example/?room=…#k=…'
//!   rein-remote-viewer build-box --pair 123-456          # by name on this network, pairing once
//!   rein-remote-viewer URL --headless --frames 120 --json   # no window: decode, measure, report
//!
//! Presentation is softbuffer (the picture scaled into the window's pixels on the CPU, then handed to
//! the system's compositor), not a GPU API: the decoders already give BGRA (VideoToolbox converts in
//! hardware), so there's no color conversion left to put on a GPU, only the scaling, which is a few
//! milliseconds of one core at 1080p; and wgpu would bring a large dependency tree (and a newer Rust
//! than the workspace's) to a binary that wants to be small and quick to build.

mod decode;
mod keys;
#[cfg(target_os = "macos")]
mod mac_layer;
mod measure;
mod pad;
#[cfg(windows)]
mod platform;
mod present;
mod stream;
mod text;
mod window;

use decode::Prefer;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: rein-remote-viewer [URL|NAME] [--pair CODE] [--decoder auto|hardware|software] [--measure] [--seconds N] [--json] [--fullscreen] [--webtransport on|off] [--datagrams on|off] [--direct on|off] [--name NAME]
       rein-remote-viewer URL|NAME --headless [--frames N] [--seconds N] [--move X,Y] [--type TEXT] [--decoder …] [--json]

  With no URL or name: a list of the machines on this network to pick from.
  --measure: read the test pattern's barcode (rein-remote serve --test-pattern) for glass-to-glass latency.
  --headless: no window; decode N frames (default 120) and report.
  --move X,Y: send the pointer there and check it got there (the test pattern draws a cross; a real
    screen's pointer comes back at that place).
  --type TEXT: type it as keys (key names, as the window sends them), once frames come.";

pub struct Args {
    pub target: Option<String>,
    pub client: rr_client::Options,
    pub prefer: Prefer,
    pub measure: bool,
    pub seconds: Option<u64>,
    pub json: bool,
    pub headless: bool,
    pub frames: u64,
    pub move_to: Option<(u16, u16)>,
    pub type_text: Option<String>,
    pub fullscreen: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        target: None,
        client: rr_client::Options {
            // Feedback after decoding, with the decoder's own time in it.
            auto_feedback: false,
            input: rr_proto::INPUT_POINTER | rr_proto::INPUT_KEYBOARD | rr_proto::INPUT_GAMEPAD,
            ..Default::default()
        },
        prefer: Prefer::Auto,
        measure: false,
        seconds: None,
        json: false,
        headless: false,
        frames: 120,
        move_to: None,
        type_text: None,
        fullscreen: false,
    };
    let mut it = args.iter();
    let on = |v: String| !matches!(v.as_str(), "off" | "false" | "no");
    while let Some(arg) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--pair" => a.client.pair = Some(val()?),
            "--decoder" => {
                a.prefer = match val()?.as_str() {
                    "auto" => Prefer::Auto,
                    "hardware" | "hw" => Prefer::Hardware,
                    "software" | "sw" => Prefer::Software,
                    _ => return Err("--decoder is auto, hardware or software".into()),
                }
            }
            "--measure" => a.measure = true,
            "--seconds" => a.seconds = Some(val()?.parse().map_err(|_| "--seconds N")?),
            "--json" => a.json = true,
            "--headless" => a.headless = true,
            "--frames" => a.frames = val()?.parse().map_err(|_| "--frames N")?,
            "--move" => {
                let v = val()?;
                let (x, y) = v.split_once(',').ok_or("--move X,Y")?;
                a.move_to = Some((x.trim().parse().map_err(|_| "--move X,Y")?, y.trim().parse().map_err(|_| "--move X,Y")?));
            }
            "--type" => a.type_text = Some(val()?),
            "--fullscreen" => a.fullscreen = true,
            "--webtransport" => a.client.webtransport = on(val()?),
            "--datagrams" => a.client.datagrams = on(val()?),
            "--direct" => a.client.direct = on(val()?),
            "--name" => a.client.name = val()?,
            "--help" | "-h" => return Err(USAGE.into()),
            t if !t.starts_with("--") && a.target.is_none() => a.target = Some(t.to_string()),
            _ => return Err(format!("{arg}?\n{USAGE}")),
        }
    }
    a.client.codecs = decode::capabilities(a.prefer);
    if a.client.codecs.is_empty() {
        return Err(format!("--decoder hardware, but there's none: {}", decode::no_hardware_why()));
    }
    if a.headless && a.target.is_none() {
        return Err("--headless needs a server (a link it printed, or its name on this network)".into());
    }
    Ok(a)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("rein-remote-viewer {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let result = parse(&args).and_then(|a| if a.headless { headless(a) } else { window::run(a) });
    if let Err(e) = result {
        eprintln!("rein-remote-viewer: {e}");
        std::process::exit(1);
    }
}

/// What a run measured, printed at the end of a headless run (or a windowed one given `--seconds`).
#[derive(Default)]
pub struct Report {
    pub frames: u64,
    pub first: Option<Instant>,
    pub last: Option<Instant>,
    /// Glass to glass (barcode to shown, or to decoded when headless), ms.
    pub glass: Vec<f64>,
    /// Capture to decoded, ms, from the session's clock estimate (any source, not just the pattern).
    pub latency: Vec<f64>,
    /// Pointer sent to it seen where it was sent (the cross in a decoded picture, or the server's
    /// pointer there), ms, and which.
    pub input_ms: Option<f64>,
    pub input_seen: &'static str,
    pub cpu_start: Duration,
    pub wall_start: Option<Instant>,
}

impl Report {
    pub fn new() -> Self {
        Self {
            cpu_start: measure::cpu_time(),
            wall_start: Some(Instant::now()),
            ..Default::default()
        }
    }

    pub fn frame(&mut self, glass: Option<f64>, latency: Option<f64>) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.frames += 1;
        if let Some(g) = glass {
            self.glass.push(g);
        }
        if let Some(l) = latency {
            self.latency.push(l);
        }
    }

    fn fps(&self) -> f64 {
        match (self.first, self.last) {
            (Some(a), Some(b)) if b > a => (self.frames - 1) as f64 / (b - a).as_secs_f64(),
            _ => 0.0,
        }
    }

    /// This process's CPU since it started measuring, as a share of one core.
    fn cpu_percent(&self) -> f64 {
        let wall = self.wall_start.map_or(0.0, |w| w.elapsed().as_secs_f64());
        if wall <= 0.0 {
            return 0.0;
        }
        (measure::cpu_time().saturating_sub(self.cpu_start)).as_secs_f64() * 100.0 / wall
    }

    pub fn print(&self, s: &stream::Status, json: bool) {
        let p = measure::percentile;
        let (codec, size) = s.config.map_or(("none".into(), "0x0".into()), |c| {
            (format!("{:?}", c.codec), format!("{}x{}", c.width, c.height))
        });
        if json {
            println!(
                r#"{{"server":"{}","transport":"{}","codec":"{codec}","size":"{size}","decoder":"{}","hardware":{},"frames":{},"fps":{:.1},"glass_ms":{{"p50":{:.1},"p95":{:.1},"samples":{}}},"capture_to_decoded_ms":{{"p50":{:.1},"p95":{:.1}}},"input_to_picture_ms":{},"cpu_percent":{:.1},"keyframe_requests":{},"errors":{}}}"#,
                s.server.replace('"', ""),
                s.transport,
                s.decoder,
                s.hardware,
                self.frames,
                self.fps(),
                p(&self.glass, 0.5),
                p(&self.glass, 0.95),
                self.glass.len(),
                p(&self.latency, 0.5),
                p(&self.latency, 0.95),
                self.input_ms.map_or("null".into(), |m| format!("{m:.1}")),
                self.cpu_percent(),
                s.keyframe_requests,
                s.errors,
            );
        } else {
            println!(
                "rein-remote-viewer: {} over {} ({codec} {size}), decoded with {}",
                s.server, s.transport, s.decoder
            );
            println!(
                "  {} frames, {:.1} fps; glass to glass p50 {:.1} ms p95 {:.1} ms ({} read); capture to decoded p50 {:.1} ms; CPU {:.1}% of a core",
                self.frames,
                self.fps(),
                p(&self.glass, 0.5),
                p(&self.glass, 0.95),
                self.glass.len(),
                p(&self.latency, 0.5),
                self.cpu_percent()
            );
            if let Some(m) = self.input_ms {
                println!("  input: the pointer showed {} {m:.1} ms after it was sent", self.input_seen);
            }
            if s.keyframe_requests > 0 || s.errors > 0 {
                println!("  {} decode errors, {} keyframes asked for", s.errors, s.keyframe_requests);
            }
        }
    }
}

/// No window: connect, decode frames as they come, read the barcode from each, and say what happened.
fn headless(a: Args) -> Result<(), String> {
    let target = a.target.clone().unwrap_or_default();
    let session = rr_client::connect(&target, a.client)?;
    let control = session.control();
    let shared = Arc::new(stream::Shared::default());
    let report = Mutex::new(Report::new());
    let done = AtomicBool::new(false);
    let started = Instant::now();
    let limit = Duration::from_secs(a.seconds.unwrap_or(30));
    // The pointer, once frames come: sent then, and looked for in each picture after.
    let moved: Mutex<Option<Instant>> = Mutex::new(None);
    stream::run(
        session,
        shared.clone(),
        stream::Options {
            prefer: a.prefer,
            measure: true,
            zero_copy: false,
        },
        || done.load(Ordering::Relaxed) || started.elapsed() > limit,
        |u| {
            let c = match u {
                stream::Update::Frame => None,
                stream::Update::Cursor(c) => Some(c),
                stream::Update::Status => return,
            };
            // A real screen's pointer: the server says where it is, which is where it was sent.
            if let (Some(c), Some((x, y)), Some(t)) = (c, a.move_to, *moved.lock().unwrap()) {
                let mut r = report.lock().unwrap();
                if r.input_ms.is_none() && c.x.abs_diff(x) <= 2 && c.y.abs_diff(y) <= 2 {
                    r.input_ms = Some(t.elapsed().as_secs_f64() * 1000.0);
                    r.input_seen = "at the server's pointer";
                }
                return;
            }
            let Some(pic) = shared.latest.lock().unwrap().take() else { return };
            let latency = shared.status.lock().unwrap().latency_ms;
            let mut r = report.lock().unwrap();
            r.frame(pic.barcode.and_then(measure::glass_ms).map(f64::from), latency);
            if let Some((x, y)) = a.move_to {
                let mut m = moved.lock().unwrap();
                match *m {
                    None => {
                        control.input(rr_proto::Input::PointerMove { x, y });
                        *m = Some(Instant::now());
                    }
                    Some(t) if r.input_ms.is_none() && cross_at(&pic, x as usize, y as usize) => {
                        r.input_ms = Some(t.elapsed().as_secs_f64() * 1000.0);
                        r.input_seen = "in the picture";
                    }
                    _ => {}
                }
            }
            if let Some(text) = &a.type_text {
                if r.frames == 1 {
                    for ch in text.chars() {
                        let key = if ch == '\n' { "Enter".to_string() } else { ch.to_string() };
                        for down in [true, false] {
                            control.input(rr_proto::Input::Key {
                                hid: 0,
                                down,
                                key: key.clone(),
                            });
                        }
                    }
                }
            }
            let enough = r.frames >= a.frames && (a.move_to.is_none() || r.input_ms.is_some());
            drop(r);
            shared.spare.lock().unwrap().push(pic);
            if enough {
                done.store(true, Ordering::Relaxed);
            }
        },
    );
    let r = report.into_inner().unwrap();
    let status = shared.status.lock().unwrap().clone();
    r.print(&status, a.json);
    if r.frames < a.frames {
        return Err(format!("{} frames decoded of {} ({})", r.frames, a.frames, status.state));
    }
    if a.move_to.is_some() && r.input_ms.is_none() {
        return Err("the pointer was sent but never showed where it was sent".into());
    }
    Ok(())
}

/// Whether the test pattern's cross (a red line 20 pixels each way) is drawn at `x`,`y`: along its
/// right arm, off the center, redder than the background a few rows above (the line is one pixel
/// thick, so 4:2:0 chroma halves its color: it's compared, not matched).
fn cross_at(pic: &decode::Picture, x: usize, y: usize) -> bool {
    if y < 4 || x + 14 >= pic.w || y >= pic.h {
        return false;
    }
    let px = |x: usize, y: usize| {
        let p = pic.px[y * pic.w + x];
        ((p >> 16 & 255) as i32, (p >> 8 & 255) as i32)
    };
    let red = (6..14)
        .filter(|&d| {
            let ((r, g), (rb, gb)) = (px(x + d, y), px(x + d, y - 4));
            r - rb > 40 && gb - g > 30
        })
        .count();
    red >= 6
}
