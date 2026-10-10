//! `rein-remote`: the Rein Remote server.
//!
//!   rein-remote serve                      # this display ($DISPLAY on Linux), on 127.0.0.1:7979
//!   rein-remote serve --display :20        # a virtual display (Xvfb, Xvnc)
//!   rein-remote serve --test-pattern       # a moving pattern, no display needed
//!   rein-remote serve --listen 0.0.0.0:7979 --fps 60 --bitrate 8000

mod capture;
mod encode;
mod serve;
mod yuv;

use capture::{Source, TestPattern};

fn usage() -> ! {
    eprintln!(
        "rein-remote {}\n\nusage:\n  rein-remote serve [--display :N | --test-pattern [WxH]] [--listen ADDR] [--fps N] [--bitrate KBPS]\n  rein-remote --version",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => println!("rein-remote {}", env!("CARGO_PKG_VERSION")),
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
    };
    let mut display: Option<String> = None;
    let mut pattern: Option<(usize, usize)> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => opts.listen = val()?,
            "--fps" => opts.fps = val()?.parse().map_err(|_| "--fps is a number")?,
            "--bitrate" => opts.bitrate_kbps = val()?.parse().map_err(|_| "--bitrate is kbit/s")?,
            "--display" => display = Some(val()?),
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
    let source: Box<dyn Source> = match pattern {
        Some((w, h)) => Box::new(TestPattern::new(w, h)),
        None => open_display(display.as_deref())?,
    };
    serve::run(source, opts)
}

#[cfg(target_os = "linux")]
fn open_display(display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Ok(Box::new(capture::x11::X11::open(display)?))
}

#[cfg(not(target_os = "linux"))]
fn open_display(_display: Option<&str>) -> Result<Box<dyn Source>, String> {
    Err("screen capture on this platform comes in phase 3 (see ROADMAP.md); use --test-pattern to try the stream".into())
}
