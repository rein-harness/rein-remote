//! `rein-remote`: the Rein Remote server.
//!
//!   rein-remote serve                      # this display ($DISPLAY on Linux), on 127.0.0.1:7979
//!   rein-remote serve --display :20        # a virtual display (Xvfb, Xvnc)
//!   rein-remote serve --test-pattern       # a moving pattern, no display needed
//!   rein-remote serve --window Simulator   # one app's window (macOS), by app name, title or id
//!   rein-remote windows [--json]           # the windows it can show
//!   rein-remote serve --listen 0.0.0.0:7979 --fps 60 --bitrate 8000
//!   rein-remote serve --listen 127.0.0.1:0 --token T   # a free port (printed), the stream needs ?token=T
//!                                                      # (or REIN_REMOTE_TOKEN; kept out of `ps`)

mod capture;
mod encode;
mod serve;
mod yuv;

use capture::{Source, TestPattern};

fn usage() -> ! {
    eprintln!(
        "rein-remote {}\n\nusage:\n  rein-remote serve [--display :N | --window APP|TITLE|ID | --test-pattern [WxH]] [--listen ADDR] [--fps N] [--bitrate KBPS] [--token T]\n  rein-remote windows [--json]\n  rein-remote --version",
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
    };
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

#[cfg(not(target_os = "macos"))]
fn open_window(_query: &str) -> Result<Box<dyn Source>, String> {
    Err("window capture on this platform is coming (see ROADMAP.md, phase 3)".into())
}

#[cfg(target_os = "macos")]
fn list_windows(json: bool) -> Result<(), String> {
    let list = capture::mac::windows()?;
    if json {
        let esc = |s: &str| {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        };
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

#[cfg(not(target_os = "macos"))]
fn list_windows(_json: bool) -> Result<(), String> {
    Err("listing windows on this platform is coming (see ROADMAP.md, phase 3)".into())
}

#[cfg(target_os = "linux")]
fn open_display(display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::x11::X11::open(display)?))
}

#[cfg(not(target_os = "linux"))]
fn open_display(_display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Err("screen capture on this platform comes in phase 3 (see ROADMAP.md); use --test-pattern to try the stream".into())
}
