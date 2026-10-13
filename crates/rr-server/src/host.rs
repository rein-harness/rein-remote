//! `rein-remote host`: the long-running process behind the platform apps. It owns the sharing state
//! and runs `serve` inside it (started, stopped, its source switched), keeps the settings in
//! `host.toml` (each key a `serve` flag, as `--config` takes them), and answers the control API
//! (`control.rs`; the contract is docs/control-api.md).
//!
//!   rein-remote host [--config host.toml] [--control-listen ADDR] [--control-remote on] [--sharing on|off] [serve's flags…]

use crate::serve;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The control API's version (docs/control-api.md).
pub const API: u32 = 1;

/// `host.toml`'s keys that are the host's own, not `serve`'s flags.
const HOST_KEYS: [&str; 3] = ["sharing", "control-listen", "control-remote"];
/// The keys that pick what's shown (one of them).
const SOURCE_KEYS: [&str; 8] = ["display", "window", "app", "vnc", "pipe", "test-pattern", "kms", "motion"];
/// Sign-in's keys (`serve`'s flags), set through the admin endpoint.
const SIGNIN_KEYS: [&str; 11] = [
    "public-url",
    "oidc-issuer",
    "oidc-client-id",
    "oidc-client-secret-file",
    "oidc-scopes",
    "oidc-user-claim",
    "oidc-groups-claim",
    "saml-idp-metadata",
    "saml-entity-id",
    "saml-user-attr",
    "saml-groups-attr",
];

/// A request the API refuses: its HTTP status and what to show.
#[derive(Debug)]
pub struct Fail(pub u16, pub String);

pub type Answer = Result<Value, Fail>;

fn bad(why: impl Into<String>) -> Fail {
    Fail(400, why.into())
}

fn time(t: SystemTime) -> String {
    crate::audit::rfc3339(t)
}

pub struct Host {
    pub home: PathBuf,
    /// The settings file, and what's in it.
    path: PathBuf,
    file: Mutex<toml::Table>,
    /// `serve` flags from the command line, for this run (they win over the file until the API sets
    /// the same key).
    cli: Mutex<Vec<(String, String)>>,
    /// The viewers' token, the relay room and its end-to-end key: one each for the process, so links
    /// stay the same when sharing stops and starts.
    token: String,
    room: String,
    e2e_key: [u8; 32],
    audit: Mutex<Arc<crate::audit::Audit>>,
    running: Mutex<Option<serve::Running>>,
    error: Mutex<Option<String>>,
    restart_needed: AtomicBool,
    /// One action at a time (start, stop, a switch, applying settings).
    act: Mutex<()>,
    subscribers: Mutex<Vec<mpsc::Sender<String>>>,
    /// The last of each event sent (one is sent again only when it changed).
    last: Mutex<HashMap<&'static str, String>>,
}

fn random_token(n: usize) -> String {
    let mut b = vec![0u8; n];
    let _ = getrandom::fill(&mut b);
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    b.iter().map(|x| ALPHABET[*x as usize % 32] as char).collect()
}

/// A value from the file as the flag takes it (a secret's file named, not read).
fn plain(v: &toml::Value) -> Vec<String> {
    match v {
        toml::Value::String(s) => vec![s.clone()],
        toml::Value::Integer(n) => vec![n.to_string()],
        toml::Value::Float(f) => vec![f.to_string()],
        toml::Value::Boolean(b) => vec![(if *b { "on" } else { "off" }).to_string()],
        toml::Value::Array(a) => a.iter().flat_map(plain).collect(),
        toml::Value::Table(_) => vec!["(secret)".to_string()],
        toml::Value::Datetime(d) => vec![d.to_string()],
    }
}

fn on(v: Option<&str>, default: bool) -> bool {
    match v {
        Some("on" | "true" | "yes") => true,
        Some("off" | "false" | "no") => false,
        _ => default,
    }
}

/// `"auto"` or a number, as JSON.
fn auto_or_number(v: Option<String>) -> Value {
    match v.as_deref().map(str::parse::<u64>) {
        Some(Ok(n)) if n > 0 => json!(n),
        _ => json!("auto"),
    }
}

/// `host.toml`'s keys for a source.
type SourceKeys = Vec<(&'static str, toml::Value)>;

/// A source id (docs/control-api.md) as `host.toml`'s keys, and what `serve` opens for it.
fn source_keys(id: &str) -> Result<(SourceKeys, crate::Pick), Fail> {
    let s = |v: &str| toml::Value::String(v.to_string());
    let mut pick = crate::Pick::default();
    let (kind, arg) = id.split_once(':').map_or((id, None), |(k, a)| (k, Some(a)));
    let keys = match (kind, arg) {
        ("display", None) => vec![],
        ("display", Some(n)) if !n.is_empty() => {
            pick.display = Some(n.into());
            vec![("display", s(n))]
        }
        ("window", Some(q)) if !q.is_empty() => {
            pick.window = Some(q.into());
            vec![("window", s(q))]
        }
        ("app", Some(a)) if !a.is_empty() => {
            pick.app = Some(a.into());
            vec![("app", s(a))]
        }
        ("test-pattern", None) => {
            pick.pattern = Some((1280, 720));
            vec![("test-pattern", toml::Value::Boolean(true))]
        }
        ("test-pattern", Some(size)) => {
            let (w, h) = size
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?)))
                .filter(|(w, h)| (16..=8192).contains(w) && (16..=8192).contains(h))
                .ok_or_else(|| bad(format!("{id}: test-pattern:WxH (e.g. test-pattern:1280x720)")))?;
            pick.pattern = Some((w, h));
            vec![("test-pattern", s(size))]
        }
        ("vnc", Some(at)) if at.contains(':') => {
            pick.vnc = Some(at.into());
            vec![("vnc", s(at))]
        }
        ("kms", None) => {
            pick.kms = Some(None);
            vec![("kms", toml::Value::Boolean(true))]
        }
        _ => {
            return Err(bad(format!(
                "{id:?} isn't a source (display, display:N, window:ID, app:NAME, test-pattern[:WxH], vnc:HOST:PORT)"
            )))
        }
    };
    Ok((keys, pick))
}

impl Host {
    /// The host with its settings (`path`, else `host.toml` in the home folder) and this run's flags.
    pub fn open(path: Option<PathBuf>, cli: Vec<(String, String)>) -> Result<Arc<Host>, String> {
        let home = crate::identity::home();
        let path = path.unwrap_or_else(|| home.join("host.toml"));
        let file: toml::Table = match std::fs::read_to_string(&path) {
            Ok(text) => text.parse().map_err(|e: toml::de::Error| format!("{}: {}", path.display(), e.message()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        for key in file.keys() {
            if key.starts_with('-') || key.contains(char::is_whitespace) {
                return Err(format!("{}: {key:?}: a key is a flag's name without its dashes", path.display()));
            }
        }
        let mut key = [0u8; 32];
        let _ = getrandom::fill(&mut key);
        let host = Host {
            home,
            path,
            file: Mutex::new(file),
            cli: Mutex::new(cli),
            token: random_token(24),
            room: random_token(24),
            e2e_key: key,
            audit: Mutex::default(),
            running: Mutex::default(),
            error: Mutex::default(),
            restart_needed: AtomicBool::new(false),
            act: Mutex::default(),
            subscribers: Mutex::default(),
            last: Mutex::default(),
        };
        host.reopen_audit()?;
        Ok(Arc::new(host))
    }

    /// The file as it is now (for the host's own keys).
    pub fn file_value(&self, key: &str) -> Option<String> {
        self.file.lock().unwrap().get(key).and_then(|v| plain(v).pop())
    }

    /// The `serve` flags in effect, as (key, value) pairs: the file's, then the command line's (which
    /// win; a source on the command line replaces the file's source).
    fn pairs(&self) -> Vec<(String, String)> {
        let cli = self.cli.lock().unwrap().clone();
        let cli_source = cli.iter().any(|(k, _)| SOURCE_KEYS.contains(&k.as_str()));
        let mut out = Vec::new();
        for (k, v) in self.file.lock().unwrap().iter() {
            if HOST_KEYS.contains(&k.as_str()) || cli.iter().any(|(c, _)| c == k) || (cli_source && SOURCE_KEYS.contains(&k.as_str())) {
                continue;
            }
            out.extend(plain(v).into_iter().map(|v| (k.clone(), v)));
        }
        out.extend(cli);
        out
    }

    /// A setting's value in effect (the last given).
    fn value(&self, key: &str) -> Option<String> {
        self.pairs().into_iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn values(&self, key: &str) -> Vec<String> {
        self.pairs()
            .into_iter()
            .filter(|(k, _)| k == key)
            .flat_map(|(_, v)| v.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>())
            .filter(|v| !v.is_empty())
            .collect()
    }

    /// What's shown, as a source id.
    pub fn source_id(&self) -> String {
        let get = |k: &str| self.value(k);
        if let Some(p) = get("test-pattern").filter(|v| v != "off") {
            return if p == "on" { "test-pattern".into() } else { format!("test-pattern:{p}") };
        }
        if get("kms").is_some_and(|v| v != "off") {
            return "kms".into();
        }
        if get("pipe").is_some() {
            return "pipe".into();
        }
        for k in ["vnc", "window", "app", "display"] {
            if let Some(v) = get(k) {
                return format!("{k}:{v}");
            }
        }
        "display".into()
    }

    /// The audit log's file (None: `audit = "off"`).
    pub fn audit_path(&self) -> Option<PathBuf> {
        match self.value("audit").as_deref() {
            Some("off") => None,
            Some(p) => Some(PathBuf::from(p)),
            None => Some(self.home.join("audit.jsonl")),
        }
    }

    fn reopen_audit(&self) -> Result<(), String> {
        let a = match self.audit_path() {
            Some(p) => crate::audit::Audit::open(&p)?,
            None => crate::audit::Audit::default(),
        };
        *self.audit.lock().unwrap() = Arc::new(a);
        Ok(())
    }

    pub fn log(&self, event: &str, fields: Value) {
        self.audit.lock().unwrap().log(event, fields);
    }

    /// Writes the settings file (owner-only: it may name secrets' files).
    fn save(&self, file: &toml::Table) -> Result<(), Fail> {
        let text = format!(
            "# rein-remote host's settings: each key a `serve` flag (docs/control-api.md). Written by the host.\n{}",
            toml::to_string(file).map_err(|e| Fail(500, e.to_string()))?
        );
        crate::identity::write_private(&self.path, text.as_bytes()).map_err(|e| Fail(500, format!("{}: {e}", self.path.display())))
    }

    /// Changes keys of the settings file (None: removed), and forgets the command line's for them.
    fn set(&self, changes: Vec<(String, Option<toml::Value>)>) -> Result<(), Fail> {
        let mut file = self.file.lock().unwrap().clone();
        for (k, v) in &changes {
            match v {
                Some(v) => file.insert(k.clone(), v.clone()),
                None => file.remove(k),
            };
        }
        self.save(&file)?;
        *self.file.lock().unwrap() = file;
        self.cli.lock().unwrap().retain(|(k, _)| !changes.iter().any(|(c, _)| c == k));
        Ok(())
    }

    /// A secret written to its own file in the home folder (owner-only), as the file names it.
    fn secret(&self, name: &str, value: &str) -> Result<PathBuf, Fail> {
        let path = self.home.join(name);
        crate::identity::write_private(&path, format!("{value}\n").as_bytes()).map_err(|e| Fail(500, format!("{}: {e}", path.display())))?;
        Ok(path)
    }

    /// `serve`'s arguments for the settings in effect (its secrets read from their files).
    fn serve_args(&self) -> Result<Vec<String>, String> {
        let cli = self.cli.lock().unwrap().clone();
        let cli_source = cli.iter().any(|(k, _)| SOURCE_KEYS.contains(&k.as_str()));
        let mut table = self.file.lock().unwrap().clone();
        table.retain(|k, _| !(HOST_KEYS.contains(&k) || k == "audit" || cli.iter().any(|(c, _)| c == k) || cli_source && SOURCE_KEYS.contains(&k)));
        let mut args = crate::config::table_flags(&table, &self.home, &[]).map_err(|e| format!("{}: {e}", self.path.display()))?;
        for (k, v) in cli.iter().filter(|(k, _)| k != "audit") {
            args.push(format!("--{k}"));
            args.push(v.clone());
        }
        let given = |k: &str| args.iter().any(|a| a.strip_prefix("--") == Some(k));
        let mut own = vec!["--token".to_string(), self.token.clone()];
        if !given("pair") {
            own.extend(["--pair".to_string(), "on".to_string()]);
        }
        if given("relay") && !given("room") {
            own.extend(["--room".to_string(), self.room.clone()]);
        }
        args.extend(own);
        Ok(args)
    }

    pub fn sharing(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    fn start_now(&self) -> Result<(), String> {
        let args = self.serve_args()?;
        let (source, mut opts) = crate::build_serve(&args)?;
        opts.e2e_key = self.e2e_key;
        opts.audit = self.audit.lock().unwrap().clone();
        let running = serve::start(source, opts)?;
        *self.running.lock().unwrap() = Some(running);
        Ok(())
    }

    fn stop_now(&self, text: &str) -> bool {
        let r = self.running.lock().unwrap().take();
        match r {
            Some(r) => {
                r.stop(text);
                true
            }
            None => false,
        }
    }

    /// Starts sharing with the settings as they are, and remembers it.
    pub fn start(&self) -> Answer {
        {
            let _one = self.act.lock().unwrap();
            if self.sharing() {
                return Err(Fail(409, "already sharing".into()));
            }
            if let Err(e) = self.start_now() {
                eprintln!("rein-remote: sharing didn't start: {e}");
                *self.error.lock().unwrap() = Some(e.clone());
                self.publish();
                return Err(Fail(500, e));
            }
            *self.error.lock().unwrap() = None;
            self.restart_needed.store(false, Ordering::SeqCst);
            self.log("sharing_start", json!({"source": self.source_id(), "by": "host"}));
            let _ = self.set(vec![("sharing".into(), Some(toml::Value::Boolean(true)))]);
        }
        self.publish();
        Ok(self.state())
    }

    /// Stops sharing: viewers are told `reason` (or that the host stopped sharing).
    pub fn stop(&self, reason: Option<&str>) -> Answer {
        {
            let _one = self.act.lock().unwrap();
            if !self.stop_now(reason.unwrap_or("the host stopped sharing")) {
                return Err(Fail(409, "not sharing".into()));
            }
            self.log("sharing_stop", json!({"reason": reason, "by": "host"}));
            let _ = self.set(vec![("sharing".into(), Some(toml::Value::Boolean(false)))]);
        }
        self.publish();
        Ok(self.state())
    }

    pub fn restart(&self) -> Answer {
        {
            let _one = self.act.lock().unwrap();
            if !self.stop_now("the host is restarting sharing: connect again in a moment") {
                return Err(Fail(409, "not sharing".into()));
            }
            self.restart_needed.store(false, Ordering::SeqCst);
            if let Err(e) = self.start_now() {
                *self.error.lock().unwrap() = Some(e.clone());
                self.publish();
                return Err(Fail(500, e));
            }
            *self.error.lock().unwrap() = None;
        }
        self.publish();
        Ok(self.state())
    }

    /// Settings changed: how they were applied. Not sharing, saved; sharing with no one watching,
    /// sharing restarted with them; with viewers, nothing interrupted (restart_needed until a restart).
    fn apply(&self) -> &'static str {
        let applied = {
            let _one = self.act.lock().unwrap();
            let viewers = self.running.lock().unwrap().as_ref().map(|r| r.shared.sessions().len());
            match viewers {
                None => "saved",
                Some(0) => {
                    self.stop_now("the host is applying new settings: connect again in a moment");
                    self.restart_needed.store(false, Ordering::SeqCst);
                    match self.start_now() {
                        Ok(()) => *self.error.lock().unwrap() = None,
                        Err(e) => {
                            eprintln!("rein-remote: sharing didn't start again: {e}");
                            *self.error.lock().unwrap() = Some(e);
                        }
                    }
                    "restarted"
                }
                Some(_) => {
                    self.restart_needed.store(true, Ordering::SeqCst);
                    "on_restart"
                }
            }
        };
        self.publish();
        applied
    }

    fn pairing_json(&self, seconds: bool) -> Value {
        let running = self.running.lock().unwrap();
        let Some(r) = running.as_ref().filter(|r| r.gate.pairs()) else {
            return json!({"enabled": false, "code": null, "expires_at": null, "url": null, "qr": null, "fingerprint": null});
        };
        let fp = r.gate.fingerprint();
        let mut v = match r.gate.code() {
            Some((code, until)) => {
                let url = format!("{}#pair={}&id={}", r.pair_page, code.replace('-', ""), fp.as_deref().unwrap_or(""));
                json!({"enabled": true, "code": code, "expires_at": time(until), "url": url, "qr": url})
            }
            None => json!({"enabled": true, "code": null, "expires_at": null, "url": null, "qr": null}),
        };
        if seconds {
            v["expires_in_s"] = json!(r
                .gate
                .code()
                .map(|(_, until)| until.duration_since(SystemTime::now()).unwrap_or_default().as_secs()));
            v["fingerprint"] = json!(fp);
        }
        v
    }

    pub fn state(&self) -> Value {
        let (sharing, listen, links, viewers) = {
            let running = self.running.lock().unwrap();
            match running.as_ref() {
                Some(r) => {
                    let mut links: Vec<Value> = r
                        .links
                        .iter()
                        .map(|l| json!({"kind": l.kind, "label": l.label, "url": l.url, "qr": l.url}))
                        .collect();
                    if !links.iter().any(|l| l["kind"] != "relay") {
                        // Listening on one address: its link.
                        let url = format!("http://{}/?token={}", r.addr, self.token);
                        links.insert(0, json!({"kind": "local", "label": "this computer", "url": url, "qr": url}));
                    }
                    (true, json!(r.addr.to_string()), links, r.shared.sessions().len())
                }
                None => (false, json!(self.value("listen").unwrap_or_else(|| "auto".into())), vec![], 0),
            }
        };
        json!({
            "api": API,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "sharing": sharing,
            "error": *self.error.lock().unwrap(),
            "source": self.source_id(),
            "listen": listen,
            "links": links,
            "pairing": self.pairing_json(false),
            "viewers": viewers,
            "restart_needed": self.restart_needed.load(Ordering::SeqCst),
        })
    }

    fn row(l: &serve::Live, admin: bool) -> Value {
        let n = l.now.lock().unwrap().clone();
        let mut v = json!({
            "id": l.adm.id, "device": l.device, "user": l.adm.principal.user, "via": l.adm.principal.via,
            "transport": l.adm.transport, "addr": l.adm.addr, "fps": n.fps, "rtt_ms": (n.rtt_ms * 10.0).round() / 10.0,
            "kbps": n.kbps, "width": n.width, "height": n.height, "since": time(l.since), "view_only": !l.adm.perms.input,
        });
        if admin {
            v["groups"] = json!(l.adm.principal.groups);
            v["rule"] = json!(l.adm.rule);
            v["clipboard"] = json!(l.adm.perms.clipboard);
            v["audio"] = json!(l.adm.perms.audio);
            v["recorded"] = json!(n.recorded);
            v["ends_at"] = json!(n.ends_at.map(time));
        }
        v
    }

    fn rows(&self, admin: bool) -> Vec<Value> {
        let running = self.running.lock().unwrap();
        running
            .as_ref()
            .map(|r| r.shared.sessions().iter().map(|l| Self::row(l, admin)).collect())
            .unwrap_or_default()
    }

    pub fn viewers(&self) -> Value {
        json!({"viewers": self.rows(false)})
    }

    pub fn admin_sessions(&self) -> Value {
        json!({"sessions": self.rows(true)})
    }

    pub fn devices(&self) -> Value {
        let list: Vec<Value> = crate::pairing::devices()
            .iter()
            .map(|d| json!({"id": d.id, "name": d.name, "since": time(UNIX_EPOCH + Duration::from_secs(d.since))}))
            .collect();
        json!({"devices": list})
    }

    pub fn settings(&self) -> Value {
        let get = |k: &str| self.value(k);
        let codec = match get("codec").as_deref() {
            Some("hevc" | "h265") => "h265",
            Some("auto") => "auto",
            _ => "h264",
        };
        let cli = self.cli.lock().unwrap().clone();
        let mut overridden: Vec<&str> = cli.iter().map(|(k, _)| k.as_str()).collect();
        overridden.dedup();
        json!({
            "settings": {
                "prefer": get("prefer").and_then(|p| crate::adapt::Prefer::parse(&p)).unwrap_or_default().name(),
                "fps": auto_or_number(get("fps")),
                "bitrate": auto_or_number(get("bitrate")),
                "codec": codec,
                "audio": on(get("audio").as_deref(), true),
                "listen": get("listen").unwrap_or_else(|| "auto".into()),
                "mdns": on(get("mdns").as_deref(), true),
                "pair": on(get("pair").as_deref(), true),
                "relay": self.values("relay"),
                "relay-secret-set": get("relay-secret").is_some(),
                "webtransport": on(get("webtransport").as_deref(), true),
                "tailscale-cert": on(get("tailscale-cert").as_deref(), true),
                "record": get("record"),
                "audit": self.audit_path().map_or("off".to_string(), |p| p.display().to_string()),
            },
            "sharing": on(self.file_value("sharing").as_deref(), false),
            "source": self.source_id(),
            "path": self.path.display().to_string(),
            "overridden": overridden,
            "restart_needed": self.restart_needed.load(Ordering::SeqCst),
        })
    }

    /// `PUT /control/settings`: every key checked first; none changed when one is refused.
    pub fn put_settings(&self, body: &Value) -> Answer {
        let obj = body.as_object().ok_or_else(|| bad("a JSON object of settings"))?;
        let mut changes: Vec<(String, Option<toml::Value>)> = Vec::new();
        let mut secret = None;
        for (k, v) in obj {
            let s = |v: &str| Some(toml::Value::String(v.to_string()));
            let flag = |b: bool| Some(toml::Value::Boolean(b));
            let value = if v.is_null() {
                None
            } else {
                match k.as_str() {
                    "prefer" => match v.as_str().and_then(crate::adapt::Prefer::parse) {
                        Some(p) => s(p.name()),
                        None => return Err(bad("prefer: framerate or quality")),
                    },
                    "fps" => match (v.as_str(), v.as_u64()) {
                        (Some("auto"), _) => s("auto"),
                        (_, Some(n)) if (1..=240).contains(&n) => Some(toml::Value::Integer(n as i64)),
                        _ => return Err(bad("fps: \"auto\" or 1 to 240")),
                    },
                    "bitrate" => match (v.as_str(), v.as_u64()) {
                        (Some("auto"), _) => s("auto"),
                        (_, Some(n)) if (100..=1_000_000).contains(&n) => Some(toml::Value::Integer(n as i64)),
                        _ => return Err(bad("bitrate: \"auto\" or kbit/s, 100 to 1000000")),
                    },
                    "codec" => match v.as_str() {
                        Some(c @ ("h264" | "h265" | "auto")) => s(c),
                        Some("hevc") => s("h265"),
                        _ => return Err(bad("codec: h264, h265 or auto")),
                    },
                    "audio" | "mdns" | "pair" | "webtransport" | "tailscale-cert" => match v.as_bool() {
                        Some(b) => flag(b),
                        None => return Err(bad(format!("{k}: true or false"))),
                    },
                    "listen" => match v.as_str() {
                        Some("auto") => s("auto"),
                        Some(a) if a.parse::<std::net::SocketAddr>().is_ok() => s(a),
                        _ => return Err(bad("listen: \"auto\" or an address and port (0.0.0.0:7979, 127.0.0.1:0)")),
                    },
                    "relay" => {
                        let list: Option<Vec<String>> = match v {
                            Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from)).collect(),
                            Value::String(x) => Some(vec![x.clone()]),
                            _ => None,
                        };
                        let list = list.ok_or_else(|| bad("relay: a list of ws:// or wss:// addresses"))?;
                        if let Some(r) = list.iter().find(|r| !(r.starts_with("ws://") || r.starts_with("wss://"))) {
                            return Err(bad(format!("relay: {r}: a ws:// or wss:// address")));
                        }
                        Some(toml::Value::Array(list.into_iter().map(toml::Value::String).collect()))
                    }
                    "relay-secret" => match v.as_str().filter(|x| !x.is_empty() && !x.contains(['\n', '\r'])) {
                        Some(x) => {
                            secret = Some(x.to_string());
                            let mut t = toml::Table::new();
                            t.insert("file".into(), toml::Value::String("relay-secret".into()));
                            Some(toml::Value::Table(t))
                        }
                        None => return Err(bad("relay-secret: a line of text")),
                    },
                    "record" => match v.as_str().filter(|x| !x.is_empty()) {
                        Some(x) => s(x),
                        None => return Err(bad("record: a folder, or null")),
                    },
                    "audit" => match v.as_str().filter(|x| !x.is_empty()) {
                        Some(x) => s(x),
                        None => return Err(bad("audit: a file, \"off\", or null (audit.jsonl in the home folder)")),
                    },
                    _ => return Err(bad(format!("unknown setting {k:?}"))),
                }
            };
            changes.push((k.clone(), value));
        }
        if let Some(sec) = secret {
            self.secret("relay-secret", &sec)?;
        }
        let audit = changes.iter().any(|(k, _)| k == "audit");
        let keys: Vec<String> = changes.iter().map(|(k, _)| k.clone()).collect();
        self.set(changes)?;
        if audit {
            self.reopen_audit().map_err(|e| Fail(500, e))?;
        }
        self.log("settings_changed", json!({"keys": keys}));
        let applied = self.apply();
        let mut v = self.settings();
        v["applied"] = json!(applied);
        Ok(v)
    }

    /// `PUT /control/source`: switched (live, while sharing) and remembered.
    pub fn switch(&self, id: &str) -> Answer {
        let (keys, mut pick) = source_keys(id)?;
        {
            let _one = self.act.lock().unwrap();
            pick.vnc_password = self.value("vnc-password").or_else(|| std::env::var("REIN_REMOTE_VNC_PASSWORD").ok());
            let running = self.running.lock().unwrap();
            if let Some(r) = running.as_ref() {
                let sound = self.value("audio").map(|v| on(Some(&v), true));
                let (source, _) = pick.open(sound).map_err(|e| Fail(500, e))?;
                r.switch(source, pick.label());
            }
            drop(running);
            let mut changes: Vec<(String, Option<toml::Value>)> = SOURCE_KEYS.iter().map(|k| (k.to_string(), None)).collect();
            changes.retain(|(k, _)| !keys.iter().any(|(n, _)| n == k));
            changes.extend(keys.into_iter().map(|(k, v)| (k.to_string(), Some(v))));
            self.set(changes)?;
        }
        eprintln!("rein-remote: the host now shows {id}");
        self.publish();
        Ok(self.state())
    }

    pub fn pairing(&self) -> Value {
        self.pairing_json(true)
    }

    pub fn new_code(&self) -> Answer {
        {
            let running = self.running.lock().unwrap();
            let r = running.as_ref().ok_or_else(|| Fail(409, "not sharing".into()))?;
            if !r.gate.pairs() {
                return Err(Fail(409, "pairing is off (turn on the pair setting)".into()));
            }
            r.gate.new_code();
        }
        self.publish();
        Ok(self.pairing())
    }

    /// Ends a session: the viewer is told why, then it closes; in the audit log.
    pub fn kick(&self, id: &str, reason: Option<&str>) -> Answer {
        let live = {
            let running = self.running.lock().unwrap();
            let r = running.as_ref().ok_or_else(|| Fail(409, "not sharing".into()))?;
            r.shared
                .session(id)
                .ok_or_else(|| Fail(404, "no viewer has that id (it may have just left)".into()))?
        };
        let text = match reason {
            Some(r) => format!("the host ended this session: {r}"),
            None => "the host ended this session".to_string(),
        };
        live.end(rr_proto::NOTICE_ENDED, text, "kicked");
        self.log(
            "session_kicked",
            json!({"session": live.adm.id, "user": live.adm.principal.user, "via": live.adm.principal.via, "addr": live.adm.addr, "reason": reason, "by": "host"}),
        );
        eprintln!("rein-remote: the host ended {}'s session", live.adm.principal.user);
        self.wait_gone(std::slice::from_ref(&live.adm.id));
        self.publish();
        Ok(self.viewers())
    }

    /// Until these sessions have closed (3 s at most).
    fn wait_gone(&self, ids: &[String]) {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            let open = self
                .running
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|r| r.shared.sessions().iter().any(|l| ids.contains(&l.adm.id)));
            if !open {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Unpairs a device (by its id or a unique start of it); its sessions end.
    pub fn unpair(&self, id: &str) -> Answer {
        let found: Vec<crate::pairing::Device> = crate::pairing::devices()
            .into_iter()
            .filter(|d| !id.is_empty() && d.id.starts_with(id))
            .collect();
        let d = match found.as_slice() {
            [d] => d.clone(),
            [] => return Err(Fail(404, "no paired device has that id".into())),
            _ => return Err(bad("more than one paired device starts with that: give more of its id")),
        };
        crate::pairing::unpair(&d.id).map_err(|e| Fail(500, e))?;
        self.log("unpair", json!({"device": d.id, "name": d.name, "by": "host"}));
        let ended: Vec<String> = {
            let running = self.running.lock().unwrap();
            let mut ended = vec![];
            for l in running.as_ref().map(|r| r.shared.sessions()).unwrap_or_default() {
                let p = &l.adm.principal;
                if p.via == "paired" && (p.user.ends_with(&format!("({})", d.id)) || p.user == format!("device {}", d.id)) {
                    l.end(rr_proto::NOTICE_ENDED, "this device was unpaired from the host".into(), "unpaired");
                    ended.push(l.adm.id.clone());
                }
            }
            ended
        };
        self.wait_gone(&ended);
        self.publish();
        Ok(self.devices())
    }

    pub fn login(&self) -> Answer {
        let (enabled, kind, path) = crate::service::host_at_login().map_err(|e| Fail(500, e))?;
        Ok(json!({"enabled": enabled, "kind": kind, "name": crate::service::HOST_NAME, "path": path.map(|p| p.display().to_string())}))
    }

    pub fn set_login(&self, body: &Value) -> Answer {
        let on = body
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| bad("{\"enabled\": true or false}"))?;
        crate::service::set_host_at_login(on).map_err(|e| Fail(500, e))?;
        self.login()
    }

    /// Sign-in's settings, secrets left out (whether there is one, only).
    pub fn signin(&self) -> Value {
        let get = |k: &str| self.value(k);
        let kind = if get("oidc-issuer").is_some() {
            json!("oidc")
        } else if get("saml-idp-metadata").is_some() {
            json!("saml")
        } else {
            Value::Null
        };
        json!({
            "kind": kind,
            "public-url": get("public-url"),
            "oidc-issuer": get("oidc-issuer"),
            "oidc-client-id": get("oidc-client-id"),
            "oidc-client-secret-set": get("oidc-client-secret-file").is_some_and(|f| Path::new(&f).exists()),
            "oidc-scopes": get("oidc-scopes").unwrap_or_else(|| "openid email profile".into()),
            "oidc-user-claim": get("oidc-user-claim").unwrap_or_else(|| "email".into()),
            "oidc-groups-claim": get("oidc-groups-claim").unwrap_or_else(|| "groups".into()),
            "saml-idp-metadata": get("saml-idp-metadata"),
            "saml-entity-id": get("saml-entity-id"),
            "saml-user-attr": get("saml-user-attr"),
            "saml-groups-attr": get("saml-groups-attr").unwrap_or_else(|| "groups".into()),
            "saml_available": cfg!(feature = "saml"),
        })
    }

    pub fn put_signin(&self, body: &Value) -> Answer {
        let obj = body.as_object().ok_or_else(|| bad("a JSON object of sign-in settings"))?;
        let mut changes: Vec<(String, Option<toml::Value>)> = Vec::new();
        let mut secret = None;
        for (k, v) in obj {
            if v.is_null() {
                if k == "oidc-client-secret" {
                    changes.push(("oidc-client-secret-file".into(), None));
                } else if SIGNIN_KEYS.contains(&k.as_str()) && k != "oidc-client-secret-file" {
                    changes.push((k.clone(), None));
                } else {
                    return Err(bad(format!("unknown sign-in setting {k:?}")));
                }
                continue;
            }
            let text = v
                .as_str()
                .filter(|x| !x.trim().is_empty())
                .ok_or_else(|| bad(format!("{k}: a string (or null)")))?;
            match k.as_str() {
                "oidc-client-secret" => secret = Some(text.to_string()),
                "oidc-client-secret-file" => return Err(bad("send the secret itself as oidc-client-secret: the host keeps it in a file of its own")),
                k if SIGNIN_KEYS.contains(&k) => changes.push((k.to_string(), Some(toml::Value::String(text.to_string())))),
                _ => return Err(bad(format!("unknown sign-in setting {k:?}"))),
            }
        }
        // The result, checked as `serve` checks it, before anything is written.
        let after = |k: &str| -> Option<String> {
            match changes.iter().find(|(c, _)| c == k) {
                Some((_, v)) => v.as_ref().and_then(|v| v.as_str()).map(String::from),
                None => self.value(k),
            }
        };
        if after("oidc-issuer").is_some() || after("saml-idp-metadata").is_some() {
            let public = after("public-url")
                .ok_or_else(|| bad("sign-in needs public-url: how browsers reach this machine (the identity provider sends them back there)"))?;
            if !(public.starts_with("https://") || public.starts_with("http://")) || public.contains(['?', '#']) {
                return Err(bad(format!("public-url {public}: an http(s):// URL with no query")));
            }
            if after("oidc-issuer").is_some() && after("oidc-client-id").is_none() {
                return Err(bad("oidc-issuer needs oidc-client-id"));
            }
            if after("saml-idp-metadata").is_some() && !cfg!(feature = "saml") {
                return Err(bad("this build has no SAML (it's built with --features saml)"));
            }
        }
        if let Some(sec) = secret {
            let path = self.secret("oidc-client-secret", &sec)?;
            changes.push(("oidc-client-secret-file".into(), Some(toml::Value::String(path.display().to_string()))));
        }
        let keys: Vec<String> = changes.iter().map(|(k, _)| k.clone()).collect();
        self.set(changes)?;
        self.log("signin_changed", json!({"keys": keys}));
        let applied = self.apply();
        let mut v = self.signin();
        v["applied"] = json!(applied);
        Ok(v)
    }

    pub fn policy(&self) -> Value {
        let Some(path) = self.value("policy") else {
            return json!({"enabled": false, "path": null, "format": null, "rules": 0, "text": null});
        };
        let json = path.to_ascii_lowercase().ends_with(".json");
        let text = std::fs::read_to_string(&path).ok();
        let rules = text
            .as_deref()
            .and_then(|t| crate::policy::Policy::parse(t, json).ok())
            .map_or(0, |p| p.rules.len());
        json!({"enabled": true, "path": path, "format": if json { "json" } else { "toml" }, "rules": rules, "text": text})
    }

    /// `PUT /control/admin/policy`: checked with the policy's own parser, then written and used.
    pub fn put_policy(&self, body: &Value) -> Answer {
        let text = match body.get("text") {
            Some(Value::Null) => None,
            Some(Value::String(t)) => Some(t.clone()),
            _ => return Err(bad("{\"text\": the policy (TOML or JSON), or null to turn it off}")),
        };
        let changes = match text {
            None => vec![("policy".to_string(), None)],
            Some(text) => {
                let json = match body.get("format").and_then(Value::as_str) {
                    None | Some("toml") => false,
                    Some("json") => true,
                    Some(f) => return Err(bad(format!("format: toml or json, not {f}"))),
                };
                let p = crate::policy::Policy::parse(&text, json).map_err(|e| bad(format!("policy: {e}")))?;
                let ext = if json { "json" } else { "toml" };
                let path = self
                    .value("policy")
                    .map(PathBuf::from)
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
                    .unwrap_or_else(|| self.home.join(format!("policy.{ext}")));
                crate::identity::write_private(&path, text.as_bytes()).map_err(|e| Fail(500, format!("{}: {e}", path.display())))?;
                self.log("policy_changed", json!({"path": path.display().to_string(), "rules": p.rules.len()}));
                vec![("policy".to_string(), Some(toml::Value::String(path.display().to_string())))]
            }
        };
        if changes[0].1.is_none() {
            self.log("policy_changed", json!({"path": null, "rules": 0}));
        }
        self.set(changes)?;
        let applied = self.apply();
        let mut v = self.policy();
        v["applied"] = json!(applied);
        Ok(v)
    }

    /// `GET /control/admin/audit`: newest first, filtered, a page at a time.
    pub fn audit(&self, q: &HashMap<String, String>) -> Answer {
        let Some(path) = self.audit_path() else {
            return Ok(json!({"path": null, "entries": [], "next": null}));
        };
        let events: Vec<&str> = q
            .get("event")
            .map(|e| e.split(',').map(str::trim).filter(|e| !e.is_empty()).collect())
            .unwrap_or_default();
        let user = q.get("user").map(|u| u.to_lowercase());
        let since = q.get("since").cloned();
        let limit = match q.get("limit") {
            Some(l) => l
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=1000).contains(n))
                .ok_or_else(|| bad("limit: 1 to 1000"))?,
            None => 100,
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Fail(500, format!("{}: {e}", path.display()))),
        };
        let lines: Vec<&str> = text.lines().collect();
        let before = match q.get("before") {
            Some(b) => b.parse::<usize>().map_err(|_| bad("before: the next of the page before"))?.min(lines.len()),
            None => lines.len(),
        };
        let (mut entries, mut next) = (Vec::new(), None);
        for i in (0..before).rev() {
            let Ok(e) = serde_json::from_str::<Value>(lines[i]) else { continue };
            let ev = e["event"].as_str().unwrap_or("");
            if !events.is_empty() && !events.contains(&ev) {
                continue;
            }
            if let Some(u) = &user {
                if !e["user"].as_str().is_some_and(|x| x.to_lowercase().contains(u.as_str())) {
                    continue;
                }
            }
            if let Some(s) = &since {
                if e["ts"].as_str().is_none_or(|ts| ts < s.as_str()) {
                    continue;
                }
            }
            if entries.len() == limit {
                next = Some((i + 1).to_string());
                break;
            }
            entries.push(e);
        }
        Ok(json!({"path": path.display().to_string(), "entries": entries, "next": next}))
    }

    /// Who's subscribed to events: each gets them as SSE text.
    pub fn subscribe(&self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        for (name, v) in self.snapshots() {
            let _ = tx.send(sse(name, &v.to_string()));
        }
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    fn snapshots(&self) -> [(&'static str, Value); 4] {
        [
            ("state", self.state()),
            ("viewers", self.viewers()),
            ("devices", self.devices()),
            ("settings", self.settings()),
        ]
    }

    /// Each event whose object changed since it was last sent, to every subscriber.
    pub fn publish(&self) {
        let snaps = self.snapshots();
        let mut last = self.last.lock().unwrap();
        let mut out = vec![];
        for (name, v) in snaps {
            let text = v.to_string();
            if last.get(name) != Some(&text) {
                out.push(sse(name, &text));
                last.insert(name, text);
            }
        }
        drop(last);
        if out.is_empty() {
            return;
        }
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|s| out.iter().all(|m| s.send(m.clone()).is_ok()));
    }
}

fn sse(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// What can be shared now (docs/control-api.md: `GET /control/sources`).
pub fn sources() -> Value {
    let other = json!([{"id": "test-pattern", "name": "Test pattern"}]);
    let (displays, windows, apps, error) = list_sources();
    json!({"displays": displays, "windows": windows, "apps": apps, "other": other, "error": error})
}

#[cfg(target_os = "macos")]
fn list_sources() -> (Vec<Value>, Vec<Value>, Vec<Value>, Option<String>) {
    // Listing without the permission would make macOS ask: only with it.
    if !permissions::screen_recording() {
        return (
            vec![],
            vec![],
            vec![],
            Some("Screen Recording is off for the app running rein-remote (System Settings → Privacy & Security → Screen Recording)".into()),
        );
    }
    let mut error = None;
    let displays = match crate::capture::mac::displays() {
        Ok(list) => list
            .iter()
            .map(|d| {
                let name = if d.main {
                    format!("Display {} (main)", d.n)
                } else {
                    format!("Display {}", d.n)
                };
                json!({"id": format!("display:{}", d.n), "name": name, "width": d.width, "height": d.height})
            })
            .collect(),
        Err(e) => {
            error = Some(e);
            vec![]
        }
    };
    let list = crate::capture::mac::windows().unwrap_or_else(|e| {
        error.get_or_insert(e);
        vec![]
    });
    let windows = list
        .iter()
        .map(|w| json!({"id": format!("window:{}", w.id), "app": w.app, "title": w.title, "width": w.width, "height": w.height}))
        .collect();
    let mut apps: Vec<(String, usize)> = vec![];
    for w in &list {
        match apps.iter_mut().find(|(a, _)| *a == w.app) {
            Some((_, n)) => *n += 1,
            None => apps.push((w.app.clone(), 1)),
        }
    }
    let apps = apps
        .into_iter()
        .map(|(a, n)| json!({"id": format!("app:{a}"), "name": a, "windows": n}))
        .collect();
    (displays, windows, apps, error)
}

#[cfg(target_os = "linux")]
fn list_sources() -> (Vec<Value>, Vec<Value>, Vec<Value>, Option<String>) {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    match crate::capture::x11::windows(None) {
        Ok(list) => (
            vec![json!({"id": "display", "name": format!("This display ({display})")})],
            list.iter()
                .map(|w| json!({"id": format!("window:{}", w.id), "app": w.class, "title": w.name, "width": w.width, "height": w.height}))
                .collect(),
            vec![],
            None,
        ),
        Err(e) => (vec![], vec![], vec![], Some(e)),
    }
}

#[cfg(windows)]
fn list_sources() -> (Vec<Value>, Vec<Value>, Vec<Value>, Option<String>) {
    let display = vec![json!({"id": "display", "name": "The primary display"})];
    match crate::capture::win::windows() {
        Ok(list) => (
            display,
            list.iter()
                .map(|w| json!({"id": format!("window:{}", w.id), "app": w.app, "title": w.title, "width": w.width, "height": w.height}))
                .collect(),
            vec![],
            None,
        ),
        Err(e) => (display, vec![], vec![], Some(e)),
    }
}

/// macOS's privacy permissions, read without ever making macOS prompt.
pub mod permissions {
    use serde_json::{json, Value};

    #[cfg(target_os = "macos")]
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    #[cfg(target_os = "macos")]
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    /// Whether this process may capture the screen (never prompts).
    #[cfg(target_os = "macos")]
    pub fn screen_recording() -> bool {
        // SAFETY: no arguments; only reads the permission.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn get() -> Value {
        #[cfg(target_os = "macos")]
        {
            let word = |b: bool| if b { "granted" } else { "denied" };
            // SAFETY: no arguments; only reads the permission (never the prompting `…WithOptions`).
            let ax = unsafe { AXIsProcessTrusted() };
            json!({"platform": "macos", "screen_recording": word(screen_recording()), "accessibility": word(ax)})
        }
        #[cfg(not(target_os = "macos"))]
        {
            let platform = if cfg!(windows) { "windows" } else { "linux" };
            json!({"platform": platform, "screen_recording": "not_needed", "accessibility": "not_needed"})
        }
    }

    /// The System Settings pane for `which` (and opened, when `open`).
    pub fn open(which: &str, open: bool) -> Result<Value, String> {
        let pane = match which {
            "screen_recording" => "Privacy_ScreenCapture",
            "accessibility" => "Privacy_Accessibility",
            _ => return Err("which: screen_recording or accessibility".into()),
        };
        if !cfg!(target_os = "macos") {
            return Ok(json!({"opened": false, "url": null}));
        }
        let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
        let opened = open && std::process::Command::new("open").arg(&url).status().is_ok_and(|s| s.success());
        Ok(json!({"opened": opened, "url": url}))
    }
}

/// `rein-remote host`: the settings read, the control API up (`control.json` written), sharing
/// started when the settings say so; then it runs until it's ended.
pub fn run(args: &[String]) -> Result<(), String> {
    let (mut config, mut listen, mut remote, mut share) = (None, None, false, None);
    let mut cli: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let key = a
            .strip_prefix("--")
            .filter(|k| !k.is_empty())
            .ok_or_else(|| format!("{a}? (host takes --flags: see docs/control-api.md)"))?;
        let value = args.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
        i += 1 + value.is_some() as usize;
        match key {
            "config" => config = Some(PathBuf::from(value.ok_or("--config needs a file")?)),
            "control-listen" => listen = Some(value.ok_or("--control-listen needs an address")?),
            "control-remote" => remote = on(value.as_deref(), true),
            "sharing" => share = Some(on(value.as_deref(), true)),
            _ => cli.push((key.to_string(), value.unwrap_or_else(|| "on".into()))),
        }
    }
    let host = Host::open(config, cli)?;
    let control = host.home.join("control.json");
    crate::control::refuse_second(&control)?;
    let listen = listen.or_else(|| host.file_value("control-listen")).unwrap_or_else(|| "127.0.0.1:0".into());
    let remote = remote || on(host.file_value("control-remote").as_deref(), false);
    let tls = if remote {
        let (cert, key) = (host.value("tls-cert"), host.value("tls-key"));
        match (cert, key) {
            (Some(c), Some(k)) => Some(crate::conn::tls_from_pem(Path::new(&c), Path::new(&k))?),
            _ => return Err("--control-remote needs HTTPS (the tls-cert and tls-key settings): the token would cross the network in the clear".into()),
        }
    } else {
        None
    };
    let token = random_token(48);
    let addr = crate::control::serve(host.clone(), &listen, remote, tls.clone(), token.clone())?;
    let scheme = if tls.is_some() { "https" } else { "http" };
    let url = format!("{scheme}://{addr}");
    crate::control::write_file(&control, &url, &token)?;
    eprintln!("rein-remote: control API on {url} (its token in {})", control.display());
    host.log("server_start", json!({"role": "host", "version": env!("CARGO_PKG_VERSION")}));
    // Events: what changed, within a second.
    {
        let host = host.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            host.publish();
        });
    }
    if share.unwrap_or_else(|| on(host.file_value("sharing").as_deref(), false)) {
        if let Err(Fail(_, e)) = host.start() {
            eprintln!("rein-remote: not sharing ({e}); the control API is up");
        }
    } else {
        eprintln!("rein-remote: not sharing (POST /control/sharing/start, or sharing = true in the settings)");
    }
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ids_are_keys_and_sources() {
        let (keys, pick) = source_keys("window:4242").unwrap();
        assert_eq!(keys[0].0, "window");
        assert_eq!(pick.label(), "window:4242");
        let (keys, pick) = source_keys("test-pattern:640x480").unwrap();
        assert_eq!(keys[0].1.as_str(), Some("640x480"));
        assert_eq!((pick.pattern, pick.label().as_str()), (Some((640, 480)), "test-pattern"));
        assert!(source_keys("display").unwrap().0.is_empty());
        assert_eq!(source_keys("display:2").unwrap().1.label(), "display:2");
        assert!(source_keys("window:").is_err());
        assert!(source_keys("test-pattern:big").is_err());
        assert!(source_keys("files:/etc").is_err());
    }
}
