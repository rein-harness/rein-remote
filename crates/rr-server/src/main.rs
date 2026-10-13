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
//!   rein-remote serve --config serve.toml  # flags from a file (TOML, each key a flag's name)
//!   rein-remote install-service --config serve.toml [--print]   # started with the session (systemd, launchd, a task)
//!   rein-remote displays                   # the displays (macOS)
//!   rein-remote discover                   # Rein Remote servers on this network (mDNS)
//!   rein-remote connect URL|NAME --seconds 10   # a viewer that counts what comes (transport, fps, latency)
//!   rein-remote selftest --window Notepad --type hi   # capture works, and typing changes what's seen
//!   rein-remote selftest --test-pattern 1920x1080 --encode out.h264   # the encoder in use, timed; its stream saved
//!   rein-remote serve --listen 0.0.0.0:7979 --fps 60 --bitrate 8000   # --fps auto (the default): the viewer's display rate
//!   rein-remote serve --prefer quality     # on a slow link frames give way before the picture (default: framerate)
//!   rein-remote serve --listen 127.0.0.1:0 --token T   # a free port (printed), the stream needs ?token=T
//!                                                      # (or REIN_REMOTE_TOKEN; kept out of `ps`)

mod adapt;
mod audio;
mod audit;
mod auth;
mod capture;
mod config;
mod conn;
mod connect;
mod encode;
#[cfg(windows)]
mod encode_mf;
#[cfg(target_os = "linux")]
mod encode_vaapi;
#[cfg(target_os = "macos")]
mod encode_vt;
mod fleet;
mod gamepad;
#[cfg(target_os = "linux")]
mod h264;
mod identity;
mod link;
mod links;
mod metrics;
mod pairing;
mod policy;
mod record;
mod relay;
mod serve;
mod service;
mod yuv;

use audio::Sound;
use capture::{Source, TestPattern};

fn usage() -> ! {
    eprintln!(
        "rein-remote {}\n\nusage:\n  rein-remote serve [--config FILE.toml] [--display :N | --window APP|TITLE|ID | --app NAME | --kms [CONNECTOR] | --vnc HOST:PORT [--vnc-password P] | --pipe WxH | --test-pattern [WxH] [--motion full]] [--listen ADDR] [--fps auto|N] [--codec h264|h265|auto] [--bitrate KBPS|auto] [--prefer framerate|quality] [--token T] [--webtransport on|off] [--tls-cert PEM --tls-key PEM] [--tailscale-cert on|off] [--mdns on|off] [--pair on|off] [--relay ws(s)://HOST:PORT ... [--room CODE] [--relay-secret S]] [--audio on|off] [--public-url URL --oidc-issuer URL --oidc-client-id ID [--oidc-client-secret-file F] [--oidc-scopes S] [--oidc-user-claim C] [--oidc-groups-claim C] | --saml-idp-metadata FILE|URL [--saml-entity-id ID] [--saml-user-attr A] [--saml-groups-attr A]] [--policy FILE] [--audit FILE] [--record DIR]\n  rein-remote relay [--config FILE.toml] [--listen ADDR] [--secret S] [--metrics-listen ADDR|PORT|off]\n  rein-remote install-service [serve|relay] [--config FILE.toml] [--system] [--name NAME] [--print]\n  rein-remote uninstall-service [serve|relay] [--system] [--name NAME] [--print]\n  rein-remote windows [--json]\n  rein-remote displays\n  rein-remote discover [SECONDS]\n  rein-remote paired | unpair ID\n  rein-remote connect URL|NAME [--seconds N] [--pair CODE] [--json] [--webtransport on|off] [--datagrams on|off] [--direct on|off]\n  rein-remote selftest [--window Q | --display :N | --kms [CONNECTOR] | --test-pattern [WxH]] [--type TEXT] [--encode OUT.h264 [--frames N]]\n  rein-remote --version",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}

fn main() {
    std::process::exit(dispatch(std::env::args().skip(1).collect()));
}

/// A command and its arguments (also what a Windows service runs): the exit code.
fn dispatch(mut args: Vec<String>) -> i32 {
    // A service's flags from a file (`--config PATH`, TOML: each key a flag's name); the command line wins.
    if matches!(args.first().map(String::as_str), Some("serve" | "relay")) {
        match config::expand(&args[1..]) {
            Ok(flags) => args.splice(1.., flags).for_each(drop),
            Err(e) => {
                eprintln!("rein-remote: {e}");
                std::process::exit(2);
            }
        }
    }
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
        Some("paired") => {
            let list = pairing::devices();
            if list.is_empty() {
                eprintln!("rein-remote: no devices paired (serve shows a code to pair one)");
            }
            for d in list {
                println!("{}  {}  (since {})", d.id, if d.name.is_empty() { "?" } else { &d.name }, d.since);
            }
        }
        Some("unpair") => match args.get(1).map(|id| pairing::unpair(id)) {
            Some(Ok(true)) => println!("rein-remote: unpaired"),
            Some(Ok(false)) => {
                eprintln!("rein-remote: no paired device has that id (rein-remote paired lists them)");
                std::process::exit(1);
            }
            Some(Err(e)) => {
                eprintln!("rein-remote: {e}");
                std::process::exit(1);
            }
            None => usage(),
        },
        Some("discover") => {
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
            match rr_client::discover::discover(std::time::Duration::from_secs(secs)) {
                Ok(list) if list.is_empty() => eprintln!("rein-remote: none found on this network in {secs} s"),
                Ok(list) => {
                    for f in list {
                        let addrs: Vec<String> = f.addrs.iter().map(|a| a.to_string()).collect();
                        println!(
                            "{}  {}://{}:{}  id {}{}{}",
                            f.name,
                            if f.https { "https" } else { "http" },
                            addrs.first().map_or("?", |a| a.as_str()),
                            f.port,
                            f.id,
                            f.webtransport.map_or(String::new(), |p| format!("  webtransport udp {p}")),
                            if addrs.len() > 1 {
                                format!("  (also {})", addrs[1..].join(", "))
                            } else {
                                String::new()
                            }
                        );
                    }
                }
                Err(e) => {
                    eprintln!("rein-remote: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some("relay") => {
            let (mut listen, mut secret) = ("0.0.0.0:7990".to_string(), std::env::var("REIN_REMOTE_RELAY_SECRET").ok());
            // Metrics on this machine only, unless asked: a port number alone is 127.0.0.1's.
            let mut metrics = Some("127.0.0.1:7991".to_string());
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match (a.as_str(), it.next()) {
                    ("--listen", Some(v)) => listen = v.clone(),
                    ("--secret", Some(v)) => secret = Some(v.clone()),
                    ("--metrics-listen", Some(v)) if v == "off" => metrics = None,
                    ("--metrics-listen", Some(v)) if v.parse::<u16>().is_ok() => metrics = Some(format!("127.0.0.1:{v}")),
                    ("--metrics-listen", Some(v)) => metrics = Some(v.clone()),
                    _ => usage(),
                }
            }
            if let Err(e) = relay::run(&listen, secret, metrics.as_deref()) {
                eprintln!("rein-remote: {e}");
                std::process::exit(1);
            }
        }
        Some("connect") => {
            if let Err(e) = connect::run(&args[1..]) {
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
                return 1;
            }
        }
        Some(c @ ("install-service" | "uninstall-service")) => {
            if let Err(e) = service::run(&args[1..], c == "install-service") {
                eprintln!("rein-remote: {e}");
                return 1;
            }
        }
        #[cfg(windows)]
        Some("service-run") => {
            if let Err(e) = service::windows::run(&args[1..]) {
                eprintln!("rein-remote: {e}");
                return 1;
            }
        }
        _ => usage(),
    }
    0
}

fn serve_cmd(args: &[String]) -> Result<(), String> {
    let mut opts = serve::Options {
        // Not given: every interface (your tailnet, this network), behind a token in the link.
        listen: "auto".into(),
        fps: 0,
        codec: Some(rr_proto::Codec::H264),
        // Not given (0): from the picture's size and frame rate (serve::top_kbps).
        bitrate_kbps: 0,
        token: std::env::var("REIN_REMOTE_TOKEN").ok().filter(|t| !t.is_empty()),
        webtransport: true,
        relay: None,
        tls: None,
        tailscale_cert: true,
        mdns: true,
        pair: None,
        audio: None,
        e2e_key: {
            let mut k = [0u8; 32];
            let _ = getrandom::fill(&mut k);
            k
        },
        signin: None,
        policy: None,
        audit: Default::default(),
        record: None,
        source_label: String::new(),
        prefer: adapt::Prefer::Framerate,
        shared: Default::default(),
    };
    // Sign-in with an identity provider; a policy; the audit log; recording.
    let (mut public_url, mut oidc_issuer, mut oidc_client, mut oidc_secret_file) = (None, None, None, None);
    let (mut oidc_scopes, mut oidc_user_claim, mut oidc_groups_claim) = ("openid email profile".to_string(), "email".to_string(), "groups".to_string());
    let (mut saml_metadata, mut saml_entity, mut saml_user_attr, mut saml_groups_attr) = (None, None, None, "groups".to_string());
    let (mut policy, mut audit) = (None, None);
    let (mut relay, mut room, mut relay_secret) = (Vec::new(), None, std::env::var("REIN_REMOTE_RELAY_SECRET").ok());
    let (mut tls_cert, mut tls_key) = (None, None);
    let mut display: Option<String> = None;
    let mut window: Option<String> = None;
    let mut app: Option<String> = None;
    let mut vnc: Option<String> = None;
    let mut pipe: Option<(usize, usize)> = None;
    let mut vnc_password = std::env::var("REIN_REMOTE_VNC_PASSWORD").ok();
    let mut pattern: Option<(usize, usize)> = None;
    let mut full_motion = false;
    let mut kms: Option<Option<String>> = None;
    // Sound: on unless turned off; for a VNC display or piped frames, off unless turned on (this
    // machine's sound isn't theirs).
    let mut sound: Option<bool> = None;
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
            "--prefer" => opts.prefer = adapt::Prefer::parse(&val()?).ok_or("--prefer is framerate or quality")?,
            "--bitrate" => opts.bitrate_kbps = match val()?.as_str() { "auto" => 0, v => v.parse().map_err(|_| "--bitrate is kbit/s (or auto)")? },
            "--display" => display = Some(val()?),
            "--window" => window = Some(val()?),
            "--app" => app = Some(val()?),
            "--motion" => full_motion = val()? == "full",
            "--vnc" => vnc = Some(val()?),
            "--pipe" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--pipe WxH")?;
                pipe = Some((w.parse().map_err(|_| "--pipe WxH")?, h.parse().map_err(|_| "--pipe WxH")?));
            }
            "--vnc-password" => vnc_password = Some(val()?),
            "--token" => opts.token = Some(val()?),
            "--public-url" => public_url = Some(val()?),
            "--oidc-issuer" => oidc_issuer = Some(val()?),
            "--oidc-client-id" => oidc_client = Some(val()?),
            "--oidc-client-secret-file" => oidc_secret_file = Some(val()?),
            "--oidc-client-secret" => return Err("the client secret goes in a file (--oidc-client-secret-file) or REIN_REMOTE_OIDC_CLIENT_SECRET, never on the command line (where `ps` shows it)".into()),
            "--oidc-scopes" => oidc_scopes = val()?,
            "--oidc-user-claim" => oidc_user_claim = val()?,
            "--oidc-groups-claim" => oidc_groups_claim = val()?,
            "--saml-idp-metadata" => saml_metadata = Some(val()?),
            "--saml-entity-id" => saml_entity = Some(val()?),
            "--saml-user-attr" => saml_user_attr = Some(val()?),
            "--saml-groups-attr" => saml_groups_attr = val()?,
            "--policy" => policy = Some(val()?),
            "--audit" => audit = Some(val()?),
            "--record" => opts.record = Some(std::path::PathBuf::from(val()?)),
            // Again (or A,B): a fleet.
            "--relay" => relay.push(val()?),
            "--tls-cert" => tls_cert = Some(std::path::PathBuf::from(val()?)),
            "--tls-key" => tls_key = Some(std::path::PathBuf::from(val()?)),
            "--tailscale-cert" => opts.tailscale_cert = !matches!(val()?.as_str(), "off" | "false" | "no"),
            "--mdns" => opts.mdns = !matches!(val()?.as_str(), "off" | "false" | "no"),
            "--pair" => opts.pair = Some(!matches!(val()?.as_str(), "off" | "false" | "no")),
            "--room" => room = Some(val()?),
            "--relay-secret" => relay_secret = Some(val()?),
            "--webtransport" => opts.webtransport = !matches!(val()?.as_str(), "off" | "false" | "no"),
            // `on`/`off` too (what `true`/`false` are in a config file).
            "--test-pattern" => {
                pattern = match on_off(&mut it) {
                    Some(false) => None,
                    _ => Some(size_arg(&mut it)?.unwrap_or((1280, 720))),
                }
            }
            "--kms" => {
                kms = match on_off(&mut it) {
                    Some(false) => None,
                    Some(true) => Some(None),
                    None => Some(optional_arg(&mut it)),
                }
            }
            "--audio" => {
                sound = Some(match val()?.as_str() {
                    "on" | "true" | "yes" => true,
                    "off" | "false" | "no" => false,
                    _ => return Err("--audio is on or off".into()),
                })
            }
            _ => usage(),
        }
    }
    match (tls_cert, tls_key) {
        (Some(c), Some(k)) => opts.tls = Some((c, k)),
        (None, None) => {}
        _ => return Err("--tls-cert and --tls-key go together".into()),
    }
    if let Some(p) = &policy {
        opts.policy = Some(policy::Policy::load(std::path::Path::new(p))?);
    }
    if let Some(a) = &audit {
        opts.audit = std::sync::Arc::new(audit::Audit::open(std::path::Path::new(a))?);
    }
    opts.signin = signin(SignInArgs {
        public_url,
        oidc_issuer,
        oidc_client,
        oidc_secret_file,
        oidc_scopes,
        oidc_user_claim,
        oidc_groups_claim,
        saml_metadata,
        saml_entity,
        saml_user_attr,
        saml_groups_attr,
    })?
    .map(std::sync::Arc::new);
    // Reachable from other machines: never without a token (the link carries it), unless viewers sign
    // in with the identity provider instead (then a token only when one is given).
    let loopback = opts.listen.starts_with("127.") || opts.listen.starts_with("localhost") || opts.listen.starts_with("[::1]");
    if opts.token.is_none() && !loopback && opts.signin.is_none() {
        let mut b = [0u8; 24];
        let _ = getrandom::fill(&mut b);
        const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
        opts.token = Some(b.iter().map(|x| ALPHABET[*x as usize % 32] as char).collect());
    }
    let relay = fleet::relays(&relay);
    if !relay.is_empty() {
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
        opts.relay = Some((relay, room, relay_secret));
    }
    // What it shows, as a policy's `sources` names it.
    opts.source_label = match (&pattern, &window) {
        (Some(_), _) => "test-pattern".into(),
        (None, _) if kms.is_some() => "kms".into(),
        (None, _) if pipe.is_some() => "pipe".into(),
        (None, _) if vnc.is_some() => format!("vnc:{}", vnc.as_deref().unwrap()),
        (None, Some(q)) => format!("window:{q}"),
        (None, None) if app.is_some() => format!("app:{}", app.as_deref().unwrap()),
        (None, None) => display.as_ref().map_or("display".into(), |d| format!("display:{d}")),
    };
    let elsewhere = || if sound == Some(true) { Sound::System } else { Sound::Off };
    let (source, with): (Box<dyn Source>, Sound) = match (pattern, window) {
        (Some((w, h)), _) if full_motion => (Box::new(TestPattern::full_motion(w, h)), Sound::Tone),
        (Some((w, h)), _) => (Box::new(TestPattern::new(w, h)), Sound::Tone),
        (None, _) if kms.is_some() => (open_kms(kms.unwrap().as_deref())?, Sound::System),
        (None, _) if pipe.is_some() => (Box::new(capture::pipe::Pipe::open(pipe.unwrap().0, pipe.unwrap().1)), elsewhere()),
        (None, _) if vnc.is_some() => (
            Box::new(capture::vnc::Vnc::open(vnc.as_deref().unwrap(), vnc_password.as_deref())?),
            elsewhere(),
        ),
        (None, Some(q)) => open_window(&q)?,
        (None, None) if app.is_some() => open_app(app.as_deref().unwrap())?,
        (None, None) => open_display(display.as_deref())?,
    };
    opts.audio = audio::Audio::new(if sound == Some(false) { Sound::Off } else { with });
    serve::run(source, opts)
}

struct SignInArgs {
    public_url: Option<String>,
    oidc_issuer: Option<String>,
    oidc_client: Option<String>,
    oidc_secret_file: Option<String>,
    oidc_scopes: String,
    oidc_user_claim: String,
    oidc_groups_claim: String,
    saml_metadata: Option<String>,
    saml_entity: Option<String>,
    saml_user_attr: Option<String>,
    saml_groups_attr: String,
}

/// Sign-in with an identity provider, as the flags set it up (None: not asked for). The provider is
/// reached now, so a wrong issuer or metadata fails at start, not when someone signs in.
fn signin(a: SignInArgs) -> Result<Option<auth::SignIn>, String> {
    if a.oidc_issuer.is_none() && a.saml_metadata.is_none() {
        if a.oidc_client.is_some() || a.public_url.is_some() {
            return Err("--oidc-client-id and --public-url go with --oidc-issuer (or --saml-idp-metadata)".into());
        }
        return Ok(None);
    }
    // Where the provider sends browsers back: it must be told exactly (never guessed from a request).
    let public = a.public_url.ok_or(
        "sign-in needs --public-url (how browsers reach this server, e.g. https://remote.example.com), the address the identity provider sends them back to",
    )?;
    if !(public.starts_with("https://") || public.starts_with("http://")) || public.contains('?') || public.contains('#') {
        return Err(format!("--public-url {public}: an http(s):// URL with no query"));
    }
    if public.starts_with("http://") {
        eprintln!("rein-remote: --public-url is plain HTTP: the session cookie isn't Secure (for a test setup only)");
    }
    let mut s = auth::SignIn::new(&public);
    if let Some(issuer) = a.oidc_issuer {
        let client_id = a.oidc_client.ok_or("--oidc-issuer needs --oidc-client-id")?;
        // The secret from a file or the environment (a command line shows in `ps`).
        let client_secret = match a.oidc_secret_file {
            Some(f) => Some(
                std::fs::read_to_string(&f)
                    .map_err(|e| format!("--oidc-client-secret-file {f}: {e}"))?
                    .trim()
                    .to_string(),
            ),
            None => std::env::var("REIN_REMOTE_OIDC_CLIENT_SECRET").ok().filter(|v| !v.is_empty()),
        };
        let o = auth::oidc::Oidc::discover(auth::oidc::Config {
            issuer,
            client_id,
            client_secret,
            redirect_uri: format!("{}/sso/oidc/callback", s.public_url),
            scopes: a.oidc_scopes,
            user_claim: a.oidc_user_claim,
            groups_claim: a.oidc_groups_claim,
        })
        .map_err(|e| format!("OIDC: {e}"))?;
        eprintln!("rein-remote: OIDC with {} (redirect URI {}/sso/oidc/callback)", o.issuer(), s.public_url);
        s.oidc = Some(o);
    }
    if let Some(m) = a.saml_metadata {
        #[cfg(feature = "saml")]
        {
            let entity = a.saml_entity.unwrap_or_else(|| format!("{}/sso/saml/metadata", s.public_url));
            let saml = auth::saml::Saml::new(&m, &entity, &s.public_url, a.saml_user_attr, a.saml_groups_attr).map_err(|e| format!("SAML: {e}"))?;
            eprintln!(
                "rein-remote: SAML with {} (this service provider: {entity}, ACS {}/sso/saml/acs)",
                saml.idp_entity(),
                s.public_url
            );
            s.saml = Some(saml);
        }
        #[cfg(not(feature = "saml"))]
        {
            let _ = (m, a.saml_entity, a.saml_user_attr, a.saml_groups_attr);
            return Err("this build has no SAML (build with `--features saml`, which needs libxmlsec1)".into());
        }
    }
    Ok(Some(s))
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

/// A flag that takes an optional value given `on` or `off`: which (and it's taken), or None.
fn on_off(it: &mut std::slice::Iter<String>) -> Option<bool> {
    let v = match it.clone().next().map(String::as_str) {
        Some("on" | "true" | "yes") => true,
        Some("off" | "false" | "no") => false,
        _ => return None,
    };
    it.next();
    Some(v)
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
    let mut kms = capture::kms::Kms::open(connector)?;
    if std::env::var_os("RR_VAAPI_DEVICE").is_none() {
        if let Some(node) = &kms.render_node {
            // Still one thread: nothing reads the environment meanwhile.
            std::env::set_var("RR_VAAPI_DEVICE", node);
        }
    }
    let (w, h) = kms.size();
    let mut enc = encode::open(rr_proto::Codec::H264, w, h, 30.0, 2000)?;
    if !enc.takes_surfaces() {
        return Err(format!(
            "--kms needs VA-API with colour conversion on the GPU, and the encoder here is {} (RR_ENCODER, and the log above, say why)",
            enc.name()
        ));
    }
    // One frame through, as a stream would: a screen the GPU can't import fails here, not per viewer.
    let frame = kms
        .surface()
        .ok_or("--kms: no frame from the screen (it's blanked, or the line above says why)")?;
    enc.encode_surface(&frame, 0).map_err(|e| format!("--kms: {e}"))?;
    eprintln!("rein-remote: --kms encodes with {}", enc.name());
    Ok(Box::new(kms))
}

#[cfg(not(target_os = "linux"))]
fn open_kms(_connector: Option<&str>) -> Result<Box<dyn Source>, String> {
    Err("--kms is Linux's (the screen through the kernel's display driver)".into())
}

// Each source with the sound that goes with it: on macOS, ScreenCaptureKit's for the same content
// (one app's sound for a window or an app); elsewhere, everything the machine plays.

#[cfg(target_os = "macos")]
fn open_window(query: &str) -> Result<(Box<dyn Source>, Sound), String> {
    let w = capture::mac::MacWindow::open(query)?;
    let sound = Sound::Mac(w.sound());
    Ok((Box::new(w), sound))
}

#[cfg(target_os = "linux")]
fn open_window(query: &str) -> Result<(Box<dyn Source>, Sound), String> {
    Ok((Box::new(capture::x11::X11::open_window(None, query)?), Sound::System))
}

#[cfg(windows)]
fn open_window(query: &str) -> Result<(Box<dyn Source>, Sound), String> {
    Ok((Box::new(capture::win::WinSource::open_window(query)?), Sound::System))
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
fn open_display(display: Option<&str>) -> Result<(Box<dyn Source>, Sound), String> {
    Ok((Box::new(capture::x11::X11::open(display)?), Sound::System))
}

#[cfg(target_os = "macos")]
fn open_display(display: Option<&str>) -> Result<(Box<dyn Source>, Sound), String> {
    let d = capture::mac::MacWindow::open_display(display)?;
    let sound = Sound::Mac(d.sound());
    Ok((Box::new(d), sound))
}

#[cfg(target_os = "macos")]
fn open_app(query: &str) -> Result<(Box<dyn Source>, Sound), String> {
    let a = capture::mac::MacWindow::open_app(query)?;
    let sound = Sound::Mac(a.sound());
    Ok((Box::new(a), sound))
}

#[cfg(not(target_os = "macos"))]
fn open_app(_query: &str) -> Result<(Box<dyn Source>, Sound), String> {
    Err("--app is for macOS (elsewhere, --window shows one of an app's windows)".into())
}

#[cfg(windows)]
fn open_display(display: Option<&str>) -> Result<(Box<dyn Source>, Sound), String> {
    if display.is_some() {
        return Err("--display is for Linux and macOS; on Windows, no source streams the primary display".into());
    }
    Ok((Box::new(capture::win::WinSource::open_display()?), Sound::System))
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
        (None, Some(q)) => open_window(&q)?.0,
        (None, None) if app.is_some() => open_app(app.as_deref().unwrap())?.0,
        (None, None) => open_display(display.as_deref())?.0,
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
