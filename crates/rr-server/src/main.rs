//! `rein-remote`: the Rein Remote server.
//!
//!   rein-remote serve                      # this display ($DISPLAY on Linux), on 127.0.0.1:7979
//!   rein-remote serve --display :20        # a virtual display (Xvfb, Xvnc)
//!   rein-remote serve --test-pattern       # a moving pattern, no display needed
//!   rein-remote serve --window Simulator   # one app's window (macOS), by app name, title or id
//!   rein-remote windows [--json]           # the windows it can show
//!   rein-remote selftest --window Notepad --type hi   # capture works, and typing changes what's seen
//!   rein-remote serve --listen 0.0.0.0:7979 --fps 60 --bitrate 8000
//!   rein-remote serve --listen 127.0.0.1:0 --token T   # a free port (printed), the stream needs ?token=T
//!                                                      # (or REIN_REMOTE_TOKEN; kept out of `ps`)

mod adapt;
mod capture;
mod encode;
#[cfg(target_os = "macos")]
mod encode_vt;
mod link;
mod relay;
mod serve;
mod yuv;

use capture::{Source, TestPattern};

fn usage() -> ! {
    eprintln!(
        "rein-remote {}\n\nusage:\n  rein-remote serve [--display :N | --window APP|TITLE|ID | --test-pattern [WxH]] [--listen ADDR] [--fps N] [--bitrate KBPS] [--token T] [--webtransport on|off] [--relay ws(s)://HOST:PORT [--room CODE] [--relay-secret S]]\n  rein-remote relay [--listen ADDR] [--secret S]\n  rein-remote windows [--json]\n  rein-remote selftest [--window Q | --display :N | --test-pattern] [--type TEXT]\n  rein-remote --version",
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
        listen: "127.0.0.1:7979".into(),
        fps: 60,
        bitrate_kbps: 8000,
        token: std::env::var("REIN_REMOTE_TOKEN").ok().filter(|t| !t.is_empty()),
        webtransport: true,
        relay: None,
    };
    let (mut relay, mut room, mut relay_secret) = (None, None, std::env::var("REIN_REMOTE_RELAY_SECRET").ok());
    let mut display: Option<String> = None;
    let mut window: Option<String> = None;
    let mut pattern: Option<(usize, usize)> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => opts.listen = val()?,
            "--fps" => opts.fps = val()?.parse().map_err(|_| "--fps is a number")?,
            "--bitrate" => opts.bitrate_kbps = val()?.parse().map_err(|_| "--bitrate is kbit/s")?,
            "--display" => display = Some(val()?),
            "--window" => window = Some(val()?),
            "--token" => opts.token = Some(val()?),
            "--relay" => relay = Some(val()?),
            "--room" => room = Some(val()?),
            "--relay-secret" => relay_secret = Some(val()?),
            "--webtransport" => opts.webtransport = !matches!(val()?.as_str(), "off" | "false" | "no"),
            "--test-pattern" => {
                pattern = Some((1280, 720));
                if let Some(size) = it.clone().next().filter(|s| s.contains('x')) {
                    let (w, h) = size.split_once('x').unwrap();
                    pattern = Some((w.parse().map_err(|_| "WxH")?, h.parse().map_err(|_| "WxH")?));
                    it.next();
                }
            }
            _ => usage(),
        }
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
        (None, Some(q)) => open_window(&q)?,
        (None, None) => open_display(display.as_deref())?,
    };
    serve::run(source, opts)
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
    Ok(Box::new(capture::win::WinWindow::open(query)?))
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

#[cfg(not(target_os = "linux"))]
fn open_display(_display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Err("screen capture on this platform comes in phase 3 (see ROADMAP.md); use --test-pattern to try the stream".into())
}

/// Capture without a client (for CI and for checking a machine): the source gives a frame, and with
/// `--type`, typing into it (then Enter) changes what it shows.
fn selftest(args: &[String]) -> Result<String, String> {
    use std::time::{Duration, Instant};
    let mut it = args.iter();
    let (mut window, mut display, mut pattern, mut text) = (None, None, false, None);
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--window" => window = Some(val()?),
            "--display" => display = Some(val()?),
            "--test-pattern" => pattern = true,
            "--type" => text = Some(val()?),
            _ => usage(),
        }
    }
    let mut src: Box<dyn Source> = match (pattern, window) {
        (true, _) => Box::new(TestPattern::new(320, 240)),
        (false, Some(q)) => open_window(&q)?,
        (false, None) => open_display(display.as_deref())?,
    };
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
    let before = digest(&mut src)?;
    let Some(text) = text else { return Ok(format!("{w}×{h}, frames come")) };
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
            return Ok(format!("{w}×{h}, frames come, and typing changed the window"));
        }
    }
    Err("typing didn't change what the window shows".into())
}
