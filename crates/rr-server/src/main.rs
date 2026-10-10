//! `rein-remote`: the Rein Remote server. Phase 0: the command line and the protocol handshake's
//! shape; capture, encoding and transport arrive phase by phase (see ROADMAP.md).

use rr_proto::{CodecCap, Hello, Message, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH, VERSION_MAX, VERSION_MIN};

/// What this server will offer in `Hello` (filled in by capability detection as backends land).
fn hello() -> Hello {
    Hello {
        version_min: VERSION_MIN,
        version_max: VERSION_MAX,
        name: std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "rein-remote".into()),
        codecs: Vec::<CodecCap>::new(),
        max_width: 3840,
        max_height: 2160,
        max_fps: 60,
        audio: false,
        input: INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => println!("rein-remote {}", env!("CARGO_PKG_VERSION")),
        Some("hello") => {
            // The handshake this build would send, as hex (for checking a client against it).
            let bytes = Message::Hello(hello()).encode();
            println!("{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        _ => {
            eprintln!(
                "rein-remote {}: early development (see https://github.com/rein-harness/rein-remote).",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("usage: rein-remote --version | hello");
            std::process::exit(2);
        }
    }
}
