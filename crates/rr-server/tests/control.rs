//! `rein-remote host` driven through its control API (docs/control-api.md), with a test pattern: the
//! token required, sharing started and stopped, the source switched under a connected rr-client
//! (which gets a new Config), a viewer kicked (a Notice, then closed, then audited), pairing and
//! unpairing, settings kept in host.toml and read again, a policy checked by its parser, and the audit
//! log's tail. Each host has a home folder of its own; this test binary's own (for the paired client's
//! keys) is set once.

mod common;
use common::Proc;
use rr_client::{connect, Event, Options};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(30);

struct Host {
    proc: Proc,
    home: PathBuf,
    addr: String,
    token: String,
}

fn home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rr-host-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A host on `home`, its viewers on this computer only (a port of its own), with `extra` flags.
fn host(home: &std::path::Path, extra: &[&str]) -> Host {
    let mut args = vec!["host", "--test-pattern", "320x240", "--listen", "127.0.0.1:0", "--mdns", "off"];
    args.extend_from_slice(extra);
    let mut proc = Proc::start(&args, &[("REIN_REMOTE_HOME", home.to_str().unwrap())]);
    proc.wait_for("control API on http://", T);
    let c: Value = serde_json::from_str(&std::fs::read_to_string(home.join("control.json")).unwrap()).unwrap();
    Host {
        proc,
        home: home.to_path_buf(),
        addr: c["url"].as_str().unwrap().trim_start_matches("http://").to_string(),
        token: c["token"].as_str().unwrap().to_string(),
    }
}

/// One request, raw: the status and the body as JSON.
fn raw(addr: &str, method: &str, path: &str, headers: &str, body: &str) -> (u16, Value) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

impl Host {
    fn api(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let body = if body.is_null() { String::new() } else { body.to_string() };
        raw(
            &self.addr,
            method,
            path,
            &format!("Host: {}\r\nAuthorization: Bearer {}\r\n", self.addr, self.token),
            &body,
        )
    }

    fn ok(&self, method: &str, path: &str, body: Value) -> Value {
        let (status, v) = self.api(method, path, body);
        assert_eq!(status, 200, "{method} {path}: {v}");
        v
    }

    /// Sharing started: its link (the token in it).
    fn share(&self) -> String {
        let s = self.ok("POST", "/control/sharing/start", Value::Null);
        assert_eq!(s["sharing"], true, "{s}");
        s["links"][0]["url"].as_str().unwrap().to_string()
    }

    fn audit(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.home.join("audit.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
}

/// Events from a session until `done` says so, or time's up (a close is an event too).
fn until(s: &rr_client::Session, within: Duration, mut done: impl FnMut(&Event) -> bool) -> bool {
    let end = Instant::now() + within;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Some(e) = s.next_event(left.min(Duration::from_millis(200))) {
            if done(&e) {
                return true;
            }
            assert!(!matches!(e, Event::Closed(_)), "the session closed before: {e:?}");
        }
    }
    false
}

fn websocket() -> Options {
    Options {
        webtransport: false,
        ..Default::default()
    }
}

/// Polls `f` until it's true (or time's up).
fn eventually(mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + T;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn the_api_needs_its_token_from_this_computer() {
    let h = host(&home("auth"), &[]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(h.home.join("control.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "control.json is its owner's alone");
    }
    let here = format!("Host: {}\r\n", h.addr);
    let (status, v) = raw(&h.addr, "GET", "/control/state", &here, "");
    assert_eq!(status, 401);
    assert!(v["error"].as_str().unwrap().contains("token"), "{v}");
    let (status, _) = raw(&h.addr, "GET", "/control/state", &format!("{here}Authorization: Bearer nope\r\n"), "");
    assert_eq!(status, 401, "a wrong token");
    let (status, _) = raw(&h.addr, "GET", &format!("/control/state?token={}", h.token), &here, "");
    assert_eq!(status, 401, "never from the URL");
    // Another name for this machine (a page's, through DNS rebinding): refused, even with the token.
    let (status, _) = raw(
        &h.addr,
        "GET",
        "/control/state",
        &format!("Host: evil.example\r\nAuthorization: Bearer {}\r\n", h.token),
        "",
    );
    assert_eq!(status, 403);
    // A second host for the same home folder doesn't start.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rein-remote"))
        .args(["host", "--test-pattern"])
        .env("REIN_REMOTE_HOME", &h.home)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("another rein-remote host is running"));

    let s = h.ok("GET", "/control/state", Value::Null);
    assert_eq!((s["api"].as_u64(), s["sharing"].as_bool()), (Some(1), Some(false)), "not sharing at first: {s}");
    assert_eq!(s["source"], "test-pattern:320x240");
    assert_eq!(h.api("GET", "/control/nothing", Value::Null).0, 404);
    assert_eq!(h.api("DELETE", "/control/state", Value::Null).0, 405);
    let (status, v) = h.api("PUT", "/control/source", json!("not an object"));
    assert_eq!(status, 400, "{v}");
    let sources = h.ok("GET", "/control/sources", Value::Null);
    assert_eq!(sources["other"][0]["id"], "test-pattern", "{sources}");
    let p = h.ok("GET", "/control/permissions", Value::Null);
    assert!(p["platform"].is_string(), "{p}");
    let m = h.ok("GET", "/control/admin/metrics", Value::Null);
    assert!(m["uptime_s"].is_number() && m["viewers"]["direct"].is_number(), "{m}");
    let _ = std::fs::remove_dir_all(&h.home);
}

#[test]
fn sharing_starts_and_stops_and_starts_again_on_the_same_ports() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let listen = format!("127.0.0.1:{port}");
    let mut h = host(&home("start"), &["--listen", &listen]);
    let link = h.share();
    h.proc.wait_for("WebTransport on udp", T);
    assert_eq!(h.api("POST", "/control/sharing/start", Value::Null).0, 409, "already sharing");
    assert!(std::fs::read_to_string(h.home.join("host.toml")).unwrap().contains("sharing = true"));

    // A viewer, then sharing stops: it's told, then it closes.
    let s = connect(&link, websocket()).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Video { .. })), "frames");
    assert!(eventually(|| h.ok("GET", "/control/state", Value::Null)["viewers"] == 1));
    let state = h.ok("POST", "/control/sharing/stop", json!({"reason": "lunch"}));
    assert_eq!(state["sharing"], false);
    let mut told = None;
    let closed = until(&s, T, |e| match e {
        Event::Note(n) => {
            told = Some(n.clone());
            false
        }
        Event::Closed(_) => true,
        _ => false,
    });
    assert!(closed, "the session closed");
    assert_eq!(told.as_deref(), Some("lunch"));
    assert_eq!(h.api("POST", "/control/sharing/stop", Value::Null).0, 409, "not sharing");
    assert!(std::fs::read_to_string(h.home.join("host.toml")).unwrap().contains("sharing = false"));
    // Its ports are free: a second start takes them again, WebTransport's UDP too.
    assert!(std::net::TcpStream::connect(&listen).is_err(), "nothing listens");
    let again = h.share();
    assert_eq!(again, link, "the same link (the token lasts as long as the host)");
    h.proc.wait_for("WebTransport on udp", T);
    let s = connect(&again, Options::default()).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Video { .. })), "frames after starting again");
    assert_eq!(s.stats().transport, "webtransport-datagrams");
    let _ = std::fs::remove_dir_all(&h.home);
}

#[test]
fn a_source_switch_reaches_a_connected_client_as_a_new_config() {
    let h = host(&home("switch"), &[]);
    let link = h.share();
    let s = connect(&link, Options::default()).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Config(c) if c.width == 320)), "the first config");
    assert!(until(&s, T, |e| matches!(e, Event::Video { .. })), "frames");
    let (status, v) = h.api("PUT", "/control/source", json!({"source": "window:"}));
    assert_eq!(status, 400, "{v}");
    let state = h.ok("PUT", "/control/source", json!({"source": "test-pattern:640x480"}));
    assert_eq!(state["source"], "test-pattern:640x480");
    assert!(
        until(&s, T, |e| matches!(e, Event::Config(c) if (c.width, c.height) == (640, 480))),
        "a new config on the same session"
    );
    assert!(until(&s, T, |e| matches!(e, Event::Video { keyframe: true, .. })), "then a keyframe");
    let toml = std::fs::read_to_string(h.home.join("host.toml")).unwrap();
    assert!(toml.contains("test-pattern = \"640x480\""), "{toml}");
    let _ = std::fs::remove_dir_all(&h.home);
}

#[test]
fn a_kicked_viewer_is_told_then_closed_and_the_audit_log_says_so() {
    let h = host(&home("kick"), &[]);
    let link = h.share();
    let s = connect(&link, websocket()).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Video { .. })), "frames");
    let mut row = Value::Null;
    assert!(eventually(|| {
        row = h.ok("GET", "/control/viewers", Value::Null)["viewers"][0].clone();
        row["fps"].as_u64().unwrap_or(0) > 0
    }));
    assert_eq!(
        (row["via"].as_str(), row["transport"].as_str(), row["user"].as_str()),
        (Some("token"), Some("websocket"), Some("token")),
        "{row}"
    );
    assert_eq!((row["width"].as_u64(), row["view_only"].as_bool()), (Some(320), Some(false)), "{row}");
    let id = row["id"].as_str().unwrap().to_string();
    assert_eq!(h.api("POST", "/control/viewers/nobody/kick", Value::Null).0, 404);
    let admin = h.ok("GET", "/control/admin/sessions", Value::Null);
    assert_eq!(admin["sessions"][0]["rule"], "none", "{admin}");

    let left = h.ok("POST", &format!("/control/viewers/{id}/kick"), json!({"reason": "time for a break"}));
    assert_eq!(left["viewers"], json!([]), "gone when it answers");
    let mut told = None;
    assert!(
        until(&s, T, |e| match e {
            Event::Note(n) => {
                told = Some(n.clone());
                false
            }
            Event::Closed(_) => true,
            _ => false,
        }),
        "closed"
    );
    assert_eq!(told.as_deref(), Some("the host ended this session: time for a break"));
    let log = h.audit();
    let kicked = log.iter().find(|e| e["event"] == "session_kicked").expect("audited");
    assert_eq!(
        (kicked["session"].as_str(), kicked["reason"].as_str()),
        (Some(id.as_str()), Some("time for a break"))
    );
    assert!(eventually(|| h
        .audit()
        .iter()
        .any(|e| e["event"] == "session_end" && e["session"] == id.as_str() && e["reason"] == "kicked")));
    let _ = std::fs::remove_dir_all(&h.home);
}

#[test]
fn pairing_a_device_then_unpairing_it() {
    // This process's keys (the paired client's) in a folder of its own.
    let mine = home("pair-client");
    rr_client::keys::set_home(mine.clone());
    let h = host(&home("pair"), &[]);
    let p = h.ok("GET", "/control/pairing", Value::Null);
    assert_eq!((p["enabled"].as_bool(), p["code"].as_str()), (Some(false), None), "not sharing: {p}");
    assert_eq!(h.api("POST", "/control/pairing/code", Value::Null).0, 409);
    let link = h.share();
    let first = h.ok("GET", "/control/pairing", Value::Null);
    assert_eq!(first["enabled"], true, "{first}");
    let code = h.ok("POST", "/control/pairing/code", Value::Null);
    assert_ne!(code["code"], first["code"], "a new code");
    assert!(code["expires_in_s"].as_u64().unwrap() > 500, "{code}");
    assert!(code["url"]
        .as_str()
        .unwrap()
        .contains(&format!("#pair={}", code["code"].as_str().unwrap().replace('-', ""))));
    // The old one no longer pairs; the new one does.
    let page = link.split("/?").next().unwrap().to_string() + "/";
    let with = |c: &str| Options {
        pair: Some(c.into()),
        name: "test phone".into(),
        ..websocket()
    };
    assert!(connect(&page, with(first["code"].as_str().unwrap())).is_err());
    let code = h.ok("POST", "/control/pairing/code", Value::Null)["code"].as_str().unwrap().to_string();
    let s = connect(&page, with(&code)).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Video { .. })), "frames, paired");
    let devices = h.ok("GET", "/control/devices", Value::Null);
    assert_eq!(devices["devices"][0]["name"], "test phone", "{devices}");
    let id = devices["devices"][0]["id"].as_str().unwrap().to_string();
    assert!(eventually(|| h.ok("GET", "/control/viewers", Value::Null)["viewers"][0]["via"] == "paired"));
    assert_eq!(h.ok("GET", "/control/state", Value::Null)["pairing"]["code"], Value::Null, "the code is spent");

    // Unpaired: its session ends, and its key no longer gets in.
    assert_eq!(h.api("DELETE", "/control/devices/zzzz", Value::Null).0, 404);
    let left = h.ok("DELETE", &format!("/control/devices/{id}"), Value::Null);
    assert_eq!(left["devices"], json!([]));
    let mut told = None;
    assert!(until(&s, T, |e| match e {
        Event::Note(n) => {
            told = Some(n.clone());
            false
        }
        Event::Closed(_) => true,
        _ => false,
    }));
    assert_eq!(told.as_deref(), Some("this device was unpaired from the host"));
    assert!(connect(&page, websocket()).is_err(), "its key isn't let in");
    assert!(h.audit().iter().any(|e| e["event"] == "unpair" && e["device"] == id.as_str()));
    let _ = std::fs::remove_dir_all(&h.home);
    let _ = std::fs::remove_dir_all(&mine);
}

#[test]
fn settings_are_kept_in_host_toml_and_read_again() {
    let dir = home("settings");
    let mut h = host(&dir, &[]);
    let (status, v) = h.api("PUT", "/control/settings", json!({"fps": 500}));
    assert_eq!((status, v["error"].as_str()), (400, Some("fps: \"auto\" or 1 to 240")));
    let (status, v) = h.api("PUT", "/control/settings", json!({"prefer": "quality", "fsp": 30}));
    assert_eq!(status, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("fsp"));
    assert_eq!(
        h.ok("GET", "/control/settings", Value::Null)["settings"]["prefer"],
        "framerate",
        "nothing changed"
    );

    let v = h.ok(
        "PUT",
        "/control/settings",
        json!({"prefer": "quality", "fps": 30, "bitrate": 4000, "codec": "auto", "audio": false, "relay-secret": "s3cret-value"}),
    );
    assert_eq!(v["applied"], "saved", "not sharing: {v}");
    let s = &v["settings"];
    assert_eq!(
        (
            s["prefer"].as_str(),
            s["fps"].as_u64(),
            s["bitrate"].as_u64(),
            s["codec"].as_str(),
            s["audio"].as_bool()
        ),
        (Some("quality"), Some(30), Some(4000), Some("auto"), Some(false)),
        "{v}"
    );
    assert_eq!(s["relay-secret-set"], true);
    assert!(!v.to_string().contains("s3cret-value"), "a secret is never returned");
    let toml = std::fs::read_to_string(dir.join("host.toml")).unwrap();
    assert!(toml.contains("prefer = \"quality\"") && toml.contains("fps = 30"), "{toml}");
    assert!(!toml.contains("s3cret-value"), "the secret is in a file of its own: {toml}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("relay-secret")).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
    }
    // Sharing with no one watching: applied by restarting it.
    h.share();
    let v = h.ok("PUT", "/control/settings", json!({"fps": "auto"}));
    assert_eq!(v["applied"], "restarted", "{v}");
    assert_eq!(h.ok("GET", "/control/state", Value::Null)["sharing"], true);

    // A new host on the same folder: the same settings, and sharing again (it was on).
    h.proc.kill();
    let h = host(&dir, &[]);
    let v = h.ok("GET", "/control/settings", Value::Null);
    assert_eq!(
        (v["settings"]["prefer"].as_str(), v["settings"]["fps"].as_str(), v["sharing"].as_bool()),
        (Some("quality"), Some("auto"), Some(true)),
        "{v}"
    );
    assert!(eventually(|| h.ok("GET", "/control/state", Value::Null)["sharing"] == true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_policy_is_checked_by_its_parser_and_then_enforced() {
    let h = host(&home("policy"), &[]);
    assert_eq!(h.ok("GET", "/control/admin/policy", Value::Null)["enabled"], false);
    let (status, v) = h.api("PUT", "/control/admin/policy", json!({"text": "[[rule]]\nview_ony = true\n"}));
    assert_eq!(status, 400);
    assert!(v["error"].as_str().unwrap().contains("unknown field `view_ony`"), "the parser's message: {v}");
    let (status, v) = h.api("PUT", "/control/admin/policy", json!({"text": ""}));
    assert_eq!(status, 400);
    assert!(v["error"].as_str().unwrap().contains("no [[rule]]s"), "{v}");
    let (status, _) = h.api("PUT", "/control/admin/policy", json!({"text": "{\"rule\": 3}", "format": "json"}));
    assert_eq!(status, 400);
    assert!(!h.home.join("policy.toml").exists(), "nothing written");

    let text = "[[rule]]\nname = \"paired only\"\nvia = [\"paired\"]\n";
    let v = h.ok("PUT", "/control/admin/policy", json!({"text": text}));
    assert_eq!(
        (v["enabled"].as_bool(), v["rules"].as_u64(), v["text"].as_str()),
        (Some(true), Some(1), Some(text)),
        "{v}"
    );
    assert_eq!(std::fs::read_to_string(h.home.join("policy.toml")).unwrap(), text);
    // In force: a link (the token) isn't a paired device.
    let link = h.share();
    let err = connect(&link, websocket()).err().expect("refused by the policy");
    assert!(err.contains("403") || err.contains("refus") || err.contains("policy"), "{err}");
    assert!(h.audit().iter().any(|e| e["event"] == "session_refused" && e["via"] == "token"));
    assert!(h.audit().iter().any(|e| e["event"] == "policy_changed"));
    let _ = std::fs::remove_dir_all(&h.home);
}

#[test]
fn the_audit_logs_tail_filters_and_pages() {
    let h = host(&home("audit"), &[]);
    for _ in 0..3 {
        h.share();
        h.ok("POST", "/control/sharing/stop", Value::Null);
    }
    let all = h.ok("GET", "/control/admin/audit", Value::Null);
    let entries = all["entries"].as_array().unwrap();
    assert!(entries.len() >= 7, "{all}");
    assert_eq!(entries[0]["event"], "sharing_stop", "newest first");
    let starts = h.ok("GET", "/control/admin/audit?event=sharing_start", Value::Null);
    assert_eq!(starts["entries"].as_array().unwrap().len(), 3);
    assert_eq!(starts["next"], Value::Null);
    // A page of one, then the next.
    let p1 = h.ok("GET", "/control/admin/audit?event=sharing_start,sharing_stop&limit=1", Value::Null);
    assert_eq!(p1["entries"].as_array().unwrap().len(), 1);
    assert_eq!(p1["entries"][0]["event"], "sharing_stop");
    let next = p1["next"].as_str().expect("more").to_string();
    let p2 = h.ok(
        "GET",
        &format!("/control/admin/audit?event=sharing_start,sharing_stop&limit=1&before={next}"),
        Value::Null,
    );
    assert_eq!(p2["entries"][0]["event"], "sharing_start");
    assert!(p2["entries"][0]["ts"].as_str() < p1["entries"][0]["ts"].as_str() || p2["entries"][0]["ts"] == p1["entries"][0]["ts"]);
    let future = h.ok("GET", "/control/admin/audit?since=2999-01-01", Value::Null);
    assert_eq!(future["entries"], json!([]));
    let nobody = h.ok("GET", "/control/admin/audit?user=nobody-at-all", Value::Null);
    assert_eq!(nobody["entries"], json!([]));
    assert_eq!(h.api("GET", "/control/admin/audit?limit=0", Value::Null).0, 400);
    let _ = std::fs::remove_dir_all(&h.home);
}

#[test]
fn the_first_run_makes_its_home_folder() {
    let base = home("fresh");
    let dir = base.join("not-yet").join("made");
    let h = host(&dir, &[]);
    assert!(dir.join("control.json").exists() && dir.join("audit.jsonl").exists());
    assert_eq!(h.ok("GET", "/control/state", Value::Null)["sharing"], false);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn events_come_as_they_happen() {
    use std::io::{BufRead, BufReader};
    let h = host(&home("events"), &[]);
    let mut s = std::net::TcpStream::connect(&h.addr).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    write!(
        s,
        "GET /control/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\r\n",
        h.addr, h.token
    )
    .unwrap();
    let mut lines = BufReader::new(s).lines().map_while(Result::ok);
    assert!(lines.next().unwrap().starts_with("HTTP/1.1 200"));
    // Each event: `event: NAME` then `data: JSON`.
    let mut next = |name: &str, want: &dyn Fn(&Value) -> bool| -> Value {
        let mut event = String::new();
        for l in lines.by_ref() {
            if let Some(e) = l.strip_prefix("event: ") {
                event = e.to_string();
            } else if let Some(d) = l.strip_prefix("data: ") {
                let v: Value = serde_json::from_str(d).unwrap();
                if event == name && want(&v) {
                    return v;
                }
            }
        }
        panic!("no {name} event")
    };
    // One of each when it connects.
    let first = next("state", &|_| true);
    assert_eq!(first["sharing"], false);
    next("viewers", &|_| true);
    next("devices", &|_| true);
    next("settings", &|_| true);
    // Then what changes.
    let link = h.share();
    let started = next("state", &|v| v["sharing"] == true);
    assert_eq!(started["links"][0]["url"].as_str(), Some(link.as_str()));
    let viewer = connect(&link, websocket()).unwrap();
    let joined = next("viewers", &|v| v["viewers"].as_array().is_some_and(|a| !a.is_empty()));
    assert_eq!(joined["viewers"][0]["via"], "token");
    drop(viewer);
    next("viewers", &|v| v["viewers"] == json!([]));
    h.ok("PUT", "/control/settings", json!({"prefer": "quality"}));
    next("settings", &|v| v["settings"]["prefer"] == "quality");
    let _ = std::fs::remove_dir_all(&h.home);
}
