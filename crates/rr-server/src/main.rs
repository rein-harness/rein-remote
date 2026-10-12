//! `rein-remote`: the Rein Remote server.
//!
//!   rein-remote serve                      # this display ($DISPLAY on Linux, the primary one on Windows), on 127.0.0.1:7979
//!   rein-remote serve --display :20        # a virtual display (Xvfb, Xvnc)
//!   rein-remote serve --test-pattern       # a moving pattern, no display needed
//!   sudo -E rein-remote serve --kms        # Linux: the screen as the GPU scans it out, encoded on the GPU (zero copy)
//!   rein-remote serve --window Simulator   # one app's window (macOS), by app name, title or id
//!   rein-remote serve --app Safari         # one app, all its windows (macOS)
//!   rein-remote serve --display 2          # a display (macOS: its number in `displays`; Linux: :N)
//!   rein-remote windows [--json]           # the windows it can show
//!   rein-remote displays                   # the displays (macOS)
//!   rein-remote selftest --window Notepad --type hi   # capture works, and typing changes what's seen
//!   rein-remote selftest --test-pattern 1920x1080 --encode out.h264   # the encoder in use, timed; its stream saved
//!   rein-remote serve --listen 0.0.0.0:7979 --fps 60 --bitrate 8000   # --fps auto (the default): the viewer's display rate
//!   rein-remote serve --listen 127.0.0.1:0 --token T   # a free port (printed), the stream needs ?token=T
//!                                                      # (or REIN_REMOTE_TOKEN; kept out of `ps`)

mod adapt;
mod capture;
mod conn;
mod e2e;
mod encode;
#[cfg(windows)]
mod encode_mf;
#[cfg(target_os = "linux")]
mod encode_vaapi;
#[cfg(target_os = "macos")]
mod encode_vt;
#[cfg(target_os = "linux")]
mod h264;
mod link;
mod links;
mod relay;
mod serve;
mod yuv;

use capture::{Source, TestPattern};

fn usage() -> ! {
    eprintln!(
        "rein-remote {}\n\nusage:\n  rein-remote serve [--display :N | --window APP|TITLE|ID | --app NAME | --kms [CONNECTOR] | --vnc HOST:PORT [--vnc-password P] | --pipe WxH | --test-pattern [WxH]] [--listen ADDR] [--fps auto|N] [--codec h264|h265|auto] [--bitrate KBPS] [--token T] [--webtransport on|off] [--tls-cert PEM --tls-key PEM] [--tailscale-cert on|off] [--relay ws(s)://HOST:PORT [--room CODE] [--relay-secret S]]\n  rein-remote relay [--listen ADDR] [--secret S]\n  rein-remote windows [--json]\n  rein-remote displays\n  rein-remote selftest [--window Q | --display :N | --kms [CONNECTOR] | --test-pattern [WxH]] [--type TEXT] [--encode OUT.h264 [--frames N]]\n  rein-remote --version",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => println!("rein-remote {}", env!("CARGO_PKG_VERSION")),
        Some("windows") => match list_windows(args.get(1).map(String::as_str) == Some("--json")) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("rein-remote: {e}");
                std::process::exit(1);
            }
        },
        Some("displays") => match list_displays() {
            Ok(()) => {}
            Err(e) => {
                eprintln!("rein-remote: {e}");
                std::process::exit(1);
            }
        },
        Some("relay") => {
            let (mut listen, mut secret) = ("0.0.0.0:7990".to_string(), std::env::var("REIN_REMOTE_RELAY_SECRET").ok());
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match (a.as_str(), it.next()) {
                    ("--listen", Some(v)) => listen = v.clone(),
                    ("--secret", Some(v)) => secret = Some(v.clone()),
                    _ => usage(),
                }
            }
            if let Err(e) = relay::run(&listen, secret) {
                eprintln!("rein-remote: {e}");
                std::process::exit(1);
            }
        }
        Some("selftest") => match selftest(&args[1..]) {
            Ok(msg) => println!("rein-remote selftest: {msg}"),
            Err(e) => {
                eprintln!("rein-remote selftest: {e}");
                std::process::exit(1);
            }
        },
        Some("serve") => {
            if let Err(e) = serve_cmd(&args[1..]) {
                eprintln!("rein-remote: {e}");
                std::process::exit(1);
            }
        }
        _ => usage(),
    }
}

fn serve_cmd(args: &[String]) -> Result<(), String> {
    let mut opts = serve::Options {
        // Not given: every interface (your tailnet, this network), behind a token in the link.
        listen: "auto".into(),
        fps: 0,
        codec: Some(rr_proto::Codec::H264),
        bitrate_kbps: 8000,
        token: std::env::var("REIN_REMOTE_TOKEN").ok().filter(|t| !t.is_empty()),
        webtransport: true,
        relay: None,
        tls: None,
        tailscale_cert: true,
        e2e_key: {
            let mut k = [0u8; 32];
            let _ = getrandom::fill(&mut k);
            k
        },
    };
    let (mut relay, mut room, mut relay_secret) = (None, None, std::env::var("REIN_REMOTE_RELAY_SECRET").ok());
    let (mut tls_cert, mut tls_key) = (None, None);
    let mut display: Option<String> = None;
    let mut window: Option<String> = None;
    let mut app: Option<String> = None;
    let mut vnc: Option<String> = None;
    let mut pipe: Option<(usize, usize)> = None;
    let mut vnc_password = std::env::var("REIN_REMOTE_VNC_PASSWORD").ok();
    let mut pattern: Option<(usize, usize)> = None;
    let mut kms: Option<Option<String>> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => opts.listen = val()?,
            "--fps" => {
                let v = val()?;
                opts.fps = if v == "auto" {
                    0
                } else {
                    v.parse().ok().filter(|n| (1..=240).contains(n)).ok_or("--fps is auto or 1 to 240")?
                };
            }
            "--codec" => {
                opts.codec = match val()?.as_str() {
                    "h264" => Some(rr_proto::Codec::H264),
                    "h265" | "hevc" => Some(rr_proto::Codec::H265),
                    "auto" => None,
                    _ => return Err("--codec is h264, h265 or auto".into()),
                }
            }
            "--bitrate" => opts.bitrate_kbps = val()?.parse().map_err(|_| "--bitrate is kbit/s")?,
            "--display" => display = Some(val()?),
            "--window" => window = Some(val()?),
            "--app" => app = Some(val()?),
            "--vnc" => vnc = Some(val()?),
            "--pipe" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--pipe WxH")?;
                pipe = Some((w.parse().map_err(|_| "--pipe WxH")?, h.parse().map_err(|_| "--pipe WxH")?));
            }
            "--vnc-password" => vnc_password = Some(val()?),
            "--token" => opts.token = Some(val()?),
            "--relay" => relay = Some(val()?),
            "--tls-cert" => tls_cert = Some(std::path::PathBuf::from(val()?)),
            "--tls-key" => tls_key = Some(std::path::PathBuf::from(val()?)),
            "--tailscale-cert" => opts.tailscale_cert = !matches!(val()?.as_str(), "off" | "false" | "no"),
            "--room" => room = Some(val()?),
            "--relay-secret" => relay_secret = Some(val()?),
            "--webtransport" => opts.webtransport = !matches!(val()?.as_str(), "off" | "false" | "no"),
            "--test-pattern" => pattern = Some(size_arg(&mut it)?.unwrap_or((1280, 720))),
            "--kms" => kms = Some(optional_arg(&mut it)),
            _ => usage(),
        }
    }
    match (tls_cert, tls_key) {
        (Some(c), Some(k)) => opts.tls = Some((c, k)),
        (None, None) => {}
        _ => return Err("--tls-cert and --tls-key go together".into()),
    }
    // Reachable from other machines: never without a token (the link carries it).
    let loopback = opts.listen.starts_with("127.") || opts.listen.starts_with("localhost") || opts.listen.starts_with("[::1]");
    if opts.token.is_none() && !loopback {
        let mut b = [0u8; 24];
        let _ = getrandom::fill(&mut b);
        const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
        opts.token = Some(b.iter().map(|x| ALPHABET[*x as usize % 32] as char).collect());
    }
    if let Some(r) = relay {
        // A room code nobody can guess: 24 characters from the system's randomness.
        let room = room.unwrap_or_else(|| {
            let mut b = [0u8; 24];
            let _ = getrandom::fill(&mut b);
            const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
            b.iter().map(|x| ALPHABET[*x as usize % 32] as char).collect()
        });
        if room.len() < 16 {
            return Err("--room needs 16 characters or more (or leave it out for a random one)".into());
        }
        opts.relay = Some((r, room, relay_secret));
    }
    let source: Box<dyn Source> = match (pattern, window) {
        (Some((w, h)), _) => Box::new(TestPattern::new(w, h)),
        (None, _) if kms.is_some() => open_kms(kms.unwrap().as_deref())?,
        (None, _) if pipe.is_some() => Box::new(capture::pipe::Pipe::open(pipe.unwrap().0, pipe.unwrap().1)),
        (None, _) if vnc.is_some() => Box::new(capture::vnc::Vnc::open(vnc.as_deref().unwrap(), vnc_password.as_deref())?),
        (None, Some(q)) => open_window(&q)?,
        (None, None) if app.is_some() => open_app(app.as_deref().unwrap())?,
        (None, None) => open_display(display.as_deref())?,
    };
    serve::run(source, opts)
}

/// `--test-pattern`'s optional size (the next argument when it's WxH).
fn size_arg(it: &mut std::slice::Iter<String>) -> Result<Option<(usize, usize)>, String> {
    let Some((w, h)) = it.clone().next().and_then(|s| s.split_once('x')) else {
        return Ok(None);
    };
    let size = (w.parse().map_err(|_| "WxH")?, h.parse().map_err(|_| "WxH")?);
    it.next();
    Ok(Some(size))
}

/// A flag's optional value (the next argument, unless it's another flag).
fn optional_arg(it: &mut std::slice::Iter<String>) -> Option<String> {
    let v = it.clone().next().filter(|v| !v.starts_with("--")).cloned();
    if v.is_some() {
        it.next();
    }
    v
}

/// The screen through KMS, encoded on the same GPU: checked here, so a machine where it can't work
/// says so before anyone connects.
#[cfg(target_os = "linux")]
fn open_kms(connector: Option<&str>) -> Result<Box<dyn Source>, String> {
    let kms = capture::kms::Kms::open(connector)?;
    if std::env::var_os("RR_VAAPI_DEVICE").is_none() {
        if let Some(node) = &kms.render_node {
            // Still one thread: nothing reads the environment meanwhile.
            std::env::set_var("RR_VAAPI_DEVICE", node);
        }
    }
    let (w, h) = kms.size();
    let enc = encode::open(rr_proto::Codec::H264, w, h, 30.0, 2000)?;
    if !enc.takes_surfaces() {
        return Err(format!(
            "--kms needs VA-API with colour conversion on the GPU, and the encoder here is {} (RR_ENCODER, and the log above, say why)",
            enc.name()
        ));
    }
    eprintln!("rein-remote: --kms encodes with {}", enc.name());
    Ok(Box::new(kms))
}

#[cfg(not(target_os = "linux"))]
fn open_kms(_connector: Option<&str>) -> Result<Box<dyn Source>, String> {
    Err("--kms is Linux's (the screen through the kernel's display driver)".into())
}

#[cfg(target_os = "macos")]
fn open_window(query: &str) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::mac::MacWindow::open(query)?))
}

#[cfg(target_os = "linux")]
fn open_window(query: &str) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::x11::X11::open_window(None, query)?))
}

#[cfg(windows)]
fn open_window(query: &str) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::win::WinSource::open_window(query)?))
}

#[cfg(target_os = "macos")]
fn list_windows(json: bool) -> Result<(), String> {
    let list = capture::mac::windows()?;
    if json {
        let rows: Vec<String> = list
            .iter()
            .map(|w| {
                format!(
                    r#"{{"id":{},"app":"{}","pid":{},"title":"{}","width":{},"height":{}}}"#,
                    w.id,
                    esc(&w.app),
                    w.pid,
                    esc(&w.title),
                    w.width,
                    w.height
                )
            })
            .collect();
        println!("[{}]", rows.join(","));
    } else {
        for w in list {
            println!("{:>8}  {}  {}  ({}×{})", w.id, w.app, w.title, w.width, w.height);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn list_windows(json: bool) -> Result<(), String> {
    let list = capture::x11::windows(None)?;
    if json {
        let rows: Vec<String> = list
            .iter()
            .map(|w| {
                format!(
                    r#"{{"id":{},"app":"{}","title":"{}","width":{},"height":{}}}"#,
                    w.id,
                    esc(&w.class),
                    esc(&w.name),
                    w.width,
                    w.height
                )
            })
            .collect();
        println!("[{}]", rows.join(","));
    } else {
        for w in list {
            println!("{:>10}  {}  {}  ({}×{})", format!("0x{:x}", w.id), w.class, w.name, w.width, w.height);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn list_windows(json: bool) -> Result<(), String> {
    let list = capture::win::windows()?;
    if json {
        let rows: Vec<String> = list
            .iter()
            .map(|w| {
                format!(
                    r#"{{"id":{},"app":"{}","title":"{}","width":{},"height":{}}}"#,
                    w.id,
                    esc(&w.app),
                    esc(&w.title),
                    w.width,
                    w.height
                )
            })
            .collect();
        println!("[{}]", rows.join(","));
    } else {
        for w in list {
            println!("{:>10}  {}  {}  ({}×{})", w.id, w.app, w.title, w.width, w.height);
        }
    }
    Ok(())
}

/// A string for JSON: quotes and backslashes escaped, control characters left out.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").chars().filter(|c| !c.is_control()).collect()
}

#[cfg(target_os = "linux")]
fn open_display(display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::x11::X11::open(display)?))
}

#[cfg(target_os = "macos")]
fn open_display(display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::mac::MacWindow::open_display(display)?))
}

#[cfg(target_os = "macos")]
fn open_app(query: &str) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::mac::MacWindow::open_app(query)?))
}

#[cfg(not(target_os = "macos"))]
fn open_app(_query: &str) -> Result<Box<dyn Source>, String> {
    Err("--app is for macOS (elsewhere, --window shows one of an app's windows)".into())
}

#[cfg(windows)]
fn open_display(display: Option<&str>) -> Result<Box<dyn Source>, String> {
    if display.is_some() {
        return Err("--display is for Linux and macOS; on Windows, no source streams the primary display".into());
    }
    Ok(Box::new(capture::win::WinSource::open_display()?))
}

#[cfg(target_os = "macos")]
fn list_displays() -> Result<(), String> {
    for d in capture::mac::displays()? {
        println!("{:>3}  id {}  {}×{}{}", d.n, d.id, d.width, d.height, if d.main { "  (main)" } else { "" });
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn list_displays() -> Result<(), String> {
    Err("`displays` is for macOS (on Linux, --display :N picks an X display)".into())
}

/// This process's CPU time so far (user and system), in milliseconds.
fn cpu_ms() -> f64 {
    #[cfg(unix)]
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        let t = |v: libc::timeval| v.tv_sec as f64 * 1000.0 + v.tv_usec as f64 / 1000.0;
        t(u.ru_utime) + t(u.ru_stime)
    }
    #[cfg(not(unix))]
    0.0
}

/// `selftest --encode OUT.h264`: frames from the source through the encoder a stream would use (the
/// system's own frames where it takes them), as fast as it goes. Saves the stream (play it with
/// ffplay, check it with ffprobe) and says how long a frame took and what it cost the CPU.
fn encode_test(src: &mut Box<dyn Source>, path: &str, frames: usize) -> Result<String, String> {
    use std::time::Instant;
    let (w, h) = src.size();
    let mut enc = encode::open(rr_proto::Codec::H264, w, h, 60.0, 8000)?;
    let (mut out, mut times, mut keyframes, mut zero_copy) = (Vec::new(), Vec::new(), 0, 0);
    let (start, cpu) = (Instant::now(), cpu_ms());
    for n in 0..frames as u64 {
        let t = Instant::now();
        let surface = if enc.takes_surfaces() { src.surface() } else { None };
        let mut done = match surface {
            Some(s) => {
                zero_copy += 1;
                enc.encode_surface(&s, n)?
            }
            None => {
                let f = src.grab()?;
                enc.encode(f.bgra, f.stride, n)?
            }
        };
        while enc.in_flight() > 0 {
            done.extend(enc.poll()?);
        }
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        for e in done {
            keyframes += e.keyframe as usize;
            out.extend_from_slice(&e.data);
        }
    }
    let (wall, cpu) = (start.elapsed().as_secs_f64() * 1000.0, cpu_ms() - cpu);
    std::fs::write(path, &out).map_err(|e| format!("{path}: {e}"))?;
    times.sort_by(f64::total_cmp);
    let n = frames.max(1) as f64;
    Ok(format!(
        "{w}×{h}, {frames} frames by {}{}: {:.2} ms a frame (median {:.2}, slowest {:.2}), {:.2} ms of CPU a frame, {} keyframe(s), {} bytes in {path}",
        enc.name(),
        if zero_copy > 0 {
            format!(", {zero_copy} straight from the GPU's memory")
        } else {
            String::new()
        },
        wall / n,
        times.get(times.len() / 2).copied().unwrap_or(0.0),
        times.last().copied().unwrap_or(0.0),
        cpu / n,
        keyframes,
        out.len()
    ))
}

/// Capture without a client (for CI and for checking a machine): the source gives a frame, the
/// encoder a stream would use (`RR_ENCODER` picks it) makes a keyframe of it, and with `--type`,
/// typing into it changes what it shows.
fn selftest(args: &[String]) -> Result<String, String> {
    use std::time::{Duration, Instant};
    let mut it = args.iter();
    let (mut window, mut display, mut pattern, mut text, mut app, mut kms) = (None, None, None, None, None, None);
    let (mut encode_to, mut frames) = (None, 120usize);
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--window" => window = Some(val()?),
            "--display" => display = Some(val()?),
            "--app" => app = Some(val()?),
            "--test-pattern" => pattern = Some(size_arg(&mut it)?.unwrap_or((320, 240))),
            "--kms" => kms = Some(optional_arg(&mut it)),
            "--type" => text = Some(val()?),
            "--encode" => encode_to = Some(val()?),
            "--frames" => frames = val()?.parse().map_err(|_| "--frames N")?,
            _ => usage(),
        }
    }
    let mut src: Box<dyn Source> = match (pattern, window) {
        (Some((w, h)), _) => Box::new(TestPattern::new(w, h)),
        (None, _) if kms.is_some() => open_kms(kms.clone().unwrap().as_deref())?,
        (None, Some(q)) => open_window(&q)?,
        (None, None) if app.is_some() => open_app(app.as_deref().unwrap())?,
        (None, None) => open_display(display.as_deref())?,
    };
    if let Some(path) = encode_to {
        return encode_test(&mut src, &path, frames);
    }
    let (w, h) = src.size();
    let digest = |src: &mut Box<dyn Source>| -> Result<u64, String> {
        use std::hash::{Hash, Hasher};
        let f = src.grab()?;
        let mut hsh = std::collections::hash_map::DefaultHasher::new();
        for row in 0..h {
            f.bgra[row * f.stride..row * f.stride + w * 4]
                .iter()
                .step_by(4)
                .collect::<Vec<_>>()
                .hash(&mut hsh);
        }
        Ok(hsh.finish())
    };
    // The first frame (a source that pushes frames needs a moment).
    let start = Instant::now();
    while src.generation().is_some_and(|g| g == 0) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    if src.generation() == Some(0) {
        return Err("no frame came in 5 seconds".into());
    }
    let encoded = encode_check(&mut src)?;
    if kms.is_some() {
        // KMS frames never reach the CPU: nothing to compare typing against.
        return Ok(format!("{w}×{h} through KMS, {encoded}"));
    }
    let before = digest(&mut src)?;
    let Some(text) = text else {
        return Ok(format!("{w}×{h}, frames come, {encoded}"));
    };
    // Clicking first gives the window the keyboard (as a person would).
    src.input(&rr_proto::Input::PointerMove {
        x: (w / 2) as u16,
        y: (h / 2) as u16,
    })?;
    src.input(&rr_proto::Input::Button {
        button: rr_proto::Button::Left,
        down: true,
    })?;
    src.input(&rr_proto::Input::Button {
        button: rr_proto::Button::Left,
        down: false,
    })?;
    std::thread::sleep(Duration::from_millis(300));
    src.input(&rr_proto::Input::Text { text })?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
        src.generation();
        if digest(&mut src)? != before {
            return Ok(format!("{w}×{h}, frames come, {encoded}, and typing changed what's seen"));
        }
    }
    Err("typing didn't change what the window shows".into())
}

/// A few frames from the source through the encoder a stream would get, as a stream would give them
/// (on the GPU where both can): which encoder it was, and that the first frame out is a keyframe.
fn encode_check(src: &mut Box<dyn Source>) -> Result<String, String> {
    use std::time::{Duration, Instant};
    let (w, h) = src.size();
    let mut enc = encode::open(rr_proto::Codec::H264, w, h, 30.0, 4000)?;
    let (mut out, mut zero_copy) = (Vec::new(), false);
    for tag in 1..=3u64 {
        let surface = if enc.takes_surfaces() { src.surface() } else { None };
        out.extend(match surface {
            Some(s) => {
                zero_copy = true;
                enc.encode_surface(&s, tag)?
            }
            None => {
                let f = src.grab()?;
                enc.encode(f.bgra, f.stride, tag)?
            }
        });
    }
    let start = Instant::now();
    while enc.in_flight() > 0 && start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(1));
        out.extend(enc.poll()?);
    }
    let first = out.first().ok_or_else(|| format!("{} gave no frame back", enc.name()))?;
    if !first.keyframe {
        return Err(format!("{}: the first frame out isn't a keyframe", enc.name()));
    }
    let gpu = if zero_copy { ", zero copy" } else { "" };
    Ok(format!("encoded with {}{gpu} (a {}-byte keyframe)", enc.name(), first.data.len()))
}
