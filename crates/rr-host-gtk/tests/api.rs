//! The control API's client against docs/control-api.md: every JSON example in the doc read into the
//! app's types, and the client against a fake host (a TCP listener answering with those examples):
//! the bearer token, the bodies sent, errors as the host words them, the event stream.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use rr_host_gtk::client::{changes, Client};
use rr_host_gtk::launch::{self, Link, Msg, Options, Started};
use rr_host_gtk::model::*;
use serde_json::{json, Map, Value};

const DOC: &str = include_str!("../../../docs/control-api.md");

/// The doc's ```json blocks, made parseable (`…` in an object stands for more keys).
fn doc_json() -> Vec<Value> {
    let mut out = Vec::new();
    let mut rest = DOC;
    while let Some(i) = rest.find("```json\n") {
        rest = &rest[i + 8..];
        let end = rest.find("```").unwrap();
        let block = rest[..end].replace(", …}", "}").replace(", …", "");
        rest = &rest[end + 3..];
        match serde_json::from_str::<Value>(&block) {
            Ok(v) => out.push(v),
            Err(_) => assert!(block.contains("[ … ]"), "a JSON example in the doc doesn't parse: {block}"),
        }
    }
    out
}

/// The doc's example that has all of `keys` at its top.
fn example(keys: &[&str]) -> Value {
    doc_json()
        .into_iter()
        .find(|v| keys.iter().all(|k| v.get(*k).is_some()))
        .unwrap_or_else(|| panic!("no example in the doc with {keys:?}"))
}

/// The doc's example whose `key` is a list (`{"viewers": [ … ]}`, not the state's count).
fn list_example(key: &str) -> Value {
    doc_json()
        .into_iter()
        .find(|v| v.get(key).is_some_and(Value::is_array))
        .unwrap_or_else(|| panic!("no example in the doc with a list of {key}"))
}

#[test]
fn the_docs_examples_read() {
    let all = doc_json();
    assert!(all.len() >= 15, "only {} JSON examples found in the doc", all.len());

    let info: ControlInfo = serde_json::from_value(example(&["url", "token", "pid", "api"])).unwrap();
    assert_eq!((info.url.as_str(), info.pid, info.api), ("http://127.0.0.1:52817", 41237, 1));

    let state: State = serde_json::from_value(example(&["sharing", "links", "pairing"])).unwrap();
    assert!(state.sharing);
    assert_eq!(state.source, "display:1");
    assert_eq!(state.links.len(), 4);
    assert_eq!(state.links[1].kind, "lan");
    assert_eq!(state.links[3].label, "through relay.example");
    assert_eq!(state.pairing.code.as_deref(), Some("123-456"));
    assert_eq!(state.viewers, 1);

    let s: Sources = serde_json::from_value(example(&["displays", "windows"])).unwrap();
    assert_eq!(s.displays[1].id, "display:2");
    assert_eq!(s.windows[0].app, "Safari");
    assert_eq!(s.apps[0].windows, 2);
    assert_eq!(s.other[0].id, "test-pattern");

    let p: Pairing = serde_json::from_value(example(&["expires_in_s", "fingerprint"])).unwrap();
    assert_eq!(p.expires_in_s, Some(512));
    assert_eq!(p.fingerprint.as_deref(), Some("abcd-efgh-ijkl-mnop"));

    let d: Devices = serde_json::from_value(list_example("devices")).unwrap();
    assert_eq!(d.devices[0].name, "Gary's iPhone");

    let v: Viewers = serde_json::from_value(list_example("viewers")).unwrap();
    let v = &v.viewers[0];
    assert_eq!(
        (v.id.as_str(), v.via.as_str(), v.transport.as_str()),
        ("5f1c2a9b0e7d4c31", "paired", "webtransport")
    );
    assert_eq!((v.fps, v.rtt_ms, v.width), (60.0, Some(4.2), 1280));

    let st: Settings = serde_json::from_value(example(&["settings", "overridden"])).unwrap();
    assert_eq!(st.str("prefer"), "framerate");
    assert_eq!(st.str("fps"), "auto");
    assert!(st.bool("relay-secret-set"));
    assert_eq!(st.list("relay"), vec!["wss://relay.example"]);
    assert_eq!(st.str("record"), "");
    assert!(st.is_overridden("test-pattern"));

    let put = example(&["prefer", "fps", "relay-secret"]);
    assert_eq!(put["fps"], 60);

    let sal: StartAtLogin = serde_json::from_value(example(&["enabled", "kind", "name"])).unwrap();
    assert_eq!(sal.kind.as_deref(), Some("launchd"));

    let perm: Permissions = serde_json::from_value(example(&["platform", "screen_recording"])).unwrap();
    assert_eq!(perm.accessibility, "denied");

    let si: Signin = serde_json::from_value(example(&["kind", "oidc-issuer", "saml_available"])).unwrap();
    assert_eq!(si.kind().as_deref(), Some("oidc"));
    assert_eq!(si.str("oidc-client-id"), "0oa1b2c3");
    assert!(si.bool("oidc-client-secret-set"));
    assert!(!si.bool("saml_available"));

    let pol: Policy = serde_json::from_value(example(&["enabled", "rules", "text"])).unwrap();
    assert_eq!((pol.rules, pol.format.as_deref()), (2, Some("toml")));
    assert!(pol.text.unwrap().contains("[[rule]]"));

    let audit: AuditPage = serde_json::from_value(example(&["entries", "next"])).unwrap();
    assert_eq!(audit.next.as_deref(), Some("1841"));
    assert_eq!(audit.entries[0]["event"], "session_end");
    assert_eq!(audit.entries[0]["reason"], "kicked");

    let m: Metrics = serde_json::from_value(example(&["uptime_s", "sessions_total"])).unwrap();
    let rows = m.rows();
    assert!(rows.contains(&("viewers.direct".into(), "1".into())), "{rows:?}");
    assert!(
        rows.iter().any(|(k, v)| k == "relays relay.example:443" && v.contains("rtt_ms 12.5")),
        "{rows:?}"
    );

    let e: Value = example(&["error"]);
    assert!(e["error"].as_str().is_some());
}

#[test]
fn the_docs_event_stream_reads() {
    // The doc's example stream, with real objects for its "…".
    let state = example(&["sharing", "links", "pairing"]);
    let viewers = list_example("viewers");
    let text = format!("event: state\ndata: {state}\n\nevent: viewers\ndata: {viewers}\n\n: keepalive\n\nevent: later\ndata: {{}}\n\n");
    let mut p = rr_host_gtk::sse::SseParser::new();
    let evs: Vec<Event> = text
        .split_inclusive('\n')
        .filter_map(|l| p.line(l))
        .map(|e| Event::parse(&e.event, &e.data).unwrap())
        .collect();
    assert!(matches!(&evs[0], Event::State(s) if s.sharing && s.links.len() == 4));
    assert!(matches!(&evs[1], Event::Viewers(v) if v.len() == 1));
    assert_eq!(evs[2], Event::Other("later".into()));
    assert!(Event::parse("state", "not json").is_err());
}

#[test]
fn audit_queries_and_labels() {
    let q = AuditQuery {
        events: vec!["session_start".into(), "session_end".into()],
        user: "alice".into(),
        since: "2026-10-12T00:00:00Z".into(),
        limit: 50,
        before: Some("1841".into()),
    };
    assert_eq!(
        q.query_string(),
        "?event=session_start%2Csession_end&user=alice&since=2026-10-12T00%3A00%3A00Z&limit=50&before=1841"
    );
    assert_eq!(AuditQuery::default().query_string(), "");
    assert_eq!(source_label("test-pattern:1280x720"), "Test pattern 1280×720");
    assert_eq!(source_label("display"), "The display");
    assert_eq!(source_label("window:4242"), "Window 4242");
    assert_eq!(
        applied_text(Some("on_restart")).unwrap(),
        "Saved. Viewers are connected: it applies when sharing restarts."
    );
    let before: Map<String, Value> = serde_json::from_value(json!({"fps": "auto", "mdns": true})).unwrap();
    let after: Map<String, Value> = serde_json::from_value(json!({"fps": 60, "mdns": true})).unwrap();
    assert_eq!(Value::Object(changes(&before, &after)), json!({"fps": 60}));
}

/// A request the fake host saw.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    auth: Option<String>,
    body: String,
}

/// A fake host: answers each request with `answer(method, path) -> (status, body)`; an `events`
/// path streams `body` and then closes.
fn fake_host(answer: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let answer = Arc::new(answer);
    thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { continue };
            let seen = seen2.clone();
            let answer = answer.clone();
            thread::spawn(move || serve_one(s, &*answer, &seen));
        }
    });
    (url, seen)
}

fn serve_one(s: TcpStream, answer: &(dyn Fn(&str, &str) -> (u16, String) + Send + Sync), seen: &Mutex<Vec<Seen>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let (mut auth, mut len) = (None, 0usize);
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap();
        match k.to_ascii_lowercase().as_str() {
            "authorization" => auth = Some(v.trim().to_owned()),
            "content-length" => len = v.trim().parse().unwrap(),
            _ => {}
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    seen.lock().unwrap().push(Seen {
        method: method.clone(),
        path: path.clone(),
        auth,
        body: String::from_utf8(body).unwrap(),
    });
    let (status, text) = answer(&method, &path);
    let mut w = s;
    let ctype = if path.ends_with("/events") { "text/event-stream" } else { "application/json" };
    let _ = write!(
        w,
        "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
}

#[test]
fn the_client_against_a_fake_host() {
    let state = example(&["sharing", "links", "pairing"]);
    let viewers = list_example("viewers");
    let st = state.to_string();
    let vw = viewers.to_string();
    let (url, seen) = fake_host(move |m, p| match (m, p) {
        ("GET", "/control/state") | ("POST", "/control/sharing/start") | ("PUT", "/control/source") => (200, st.clone()),
        ("POST", "/control/sharing/stop") => (409, r#"{"error":"not sharing"}"#.into()),
        ("POST", p) if p.ends_with("/kick") && p.contains("nope") => (404, r#"{"error":"no viewer has that id (it may have just left)"}"#.into()),
        ("POST", p) if p.ends_with("/kick") => (200, r#"{"viewers":[]}"#.into()),
        ("PUT", "/control/settings") => (400, r#"{"error":"fps: auto or 1 to 240"}"#.into()),
        ("PUT", "/control/admin/policy") => (
            400,
            r#"{"error":"policy: unknown field `view_ony`, expected one of `name` at line 2 column 1"}"#.into(),
        ),
        ("GET", "/control/events") => (200, format!("event: state\ndata: {st}\n\n: keepalive\n\nevent: viewers\ndata: {vw}\n\n")),
        ("GET", "/control/broken") => (500, "oops".into()),
        _ => (404, r#"{"error":"no such endpoint"}"#.into()),
    });
    let c = Client::with_url(&format!("{url}/"), "t0k");

    let s = c.state().unwrap();
    assert_eq!(s.links.len(), 4);
    c.start().unwrap();
    c.set_source("window:4242").unwrap();
    let e = c.stop(Some("lunch")).unwrap_err();
    assert_eq!((e.status, e.message.as_str()), (409, "not sharing"));
    assert!(c.kick("5f1c2a9b0e7d4c31", None).unwrap().is_empty());
    let e = c.kick("nope", Some("bye")).unwrap_err();
    assert_eq!((e.status, e.message.as_str()), (404, "no viewer has that id (it may have just left)"));
    let mut ch = Map::new();
    ch.insert("fps".into(), json!(999));
    assert_eq!(c.put_settings(&ch).unwrap_err().message, "fps: auto or 1 to 240");
    let e = c.put_policy(Some("[[rule]]\nview_ony = true\n"), "toml").unwrap_err();
    assert!(e.message.starts_with("policy: unknown field `view_ony`"), "{e}");
    assert_eq!(c.metrics().unwrap_err().message, "no such endpoint");

    let mut got = Vec::new();
    c.events(|e| {
        got.push(e);
        true
    })
    .unwrap();
    assert!(matches!(&got[..], [Event::State(_), Event::Viewers(v)] if v.len() == 1), "{got:?}");

    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|s| s.auth.as_deref() == Some("Bearer t0k")), "{seen:?}");
    assert!(seen.iter().all(|s| !s.path.contains("token")));
    let find = |m: &str, p: &str| seen.iter().find(|s| s.method == m && s.path == p).unwrap().clone();
    assert_eq!(find("PUT", "/control/source").body, r#"{"source":"window:4242"}"#);
    assert_eq!(find("POST", "/control/sharing/stop").body, r#"{"reason":"lunch"}"#);
    assert_eq!(find("POST", "/control/sharing/start").body, "");
    assert_eq!(find("POST", "/control/viewers/nope/kick").body, r#"{"reason":"bye"}"#);
    assert_eq!(find("PUT", "/control/settings").body, r#"{"fps":999}"#);
    let pol: Value = serde_json::from_str(&find("PUT", "/control/admin/policy").body).unwrap();
    assert_eq!(pol, json!({"text": "[[rule]]\nview_ony = true\n", "format": "toml"}));
}

#[test]
fn a_host_that_is_not_there() {
    // A port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let c = Client::with_url(&format!("http://127.0.0.1:{port}"), "t");
    let e = c.state().unwrap_err();
    assert_eq!(e.status, 0);
    assert!(e.message.starts_with("can't reach rein-remote host"), "{e}");
    // A wrong token: the host's 401 without a body still says something.
    let (url, _) = fake_host(|_, _| (401, String::new()));
    let e = Client::with_url(&url, "bad").state().unwrap_err();
    assert_eq!(e.status, 401);
    assert!(e.message.contains("token"), "{e}");
}

fn temp_home(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("rr-host-gtk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn attaches_to_a_running_host_and_follows_it_going() {
    let state = example(&["sharing", "links", "pairing"]).to_string();
    let (url, _) = fake_host(move |_, p| match p {
        "/control/events" => (200, format!("event: state\ndata: {state}\n\n")),
        _ => (200, state.clone()),
    });
    let home = temp_home("attach");
    std::fs::write(
        home.join("control.json"),
        json!({"url": url, "token": "t", "pid": 1, "version": "0.1.1", "api": 1}).to_string(),
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let opts = Options {
        home,
        start: false,
        binary: None,
        args: vec![],
    };
    let t = thread::spawn(move || launch::supervise(opts, Started::default(), stop2, move |m| tx.send(m).is_ok()));
    let next = || rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(next(), Msg::Link(Link::Connecting)));
    assert!(matches!(next(), Msg::Link(Link::Connected { state, .. }) if state.sharing));
    assert!(matches!(next(), Msg::Event(Event::State(_))));
    // The fake's stream ends after one event: the app is told the host went, and connects again.
    assert!(matches!(next(), Msg::Link(Link::Gone(why)) if why.contains("stopped")));
    assert!(matches!(next(), Msg::Link(Link::Connected { .. })));
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    t.join().unwrap();
}

#[test]
fn says_why_when_no_host_runs_and_none_may_start() {
    let home = temp_home("none");
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let opts = Options {
        home,
        start: false,
        binary: None,
        args: vec![],
    };
    let t = thread::spawn(move || launch::supervise(opts, Started::default(), stop2, move |m| tx.send(m).is_ok()));
    assert!(matches!(rx.recv().unwrap(), Msg::Link(Link::Connecting)));
    assert!(matches!(rx.recv().unwrap(), Msg::Link(Link::Gone(why)) if why.contains("isn't running") && why.contains("control.json")));
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    t.join().unwrap();
}

/// No host running: the app starts one (here a script standing in for `rein-remote`, which writes
/// `control.json` for a fake API as the real host does), connects to it, and stops it on quit.
#[cfg(unix)]
#[test]
fn starts_the_host_when_none_runs() {
    use std::os::unix::fs::PermissionsExt;
    let state = example(&["sharing", "links", "pairing"]).to_string();
    let (url, _) = fake_host(move |_, p| match p {
        "/control/events" => (200, ": keepalive\n\n".into()),
        _ => (200, state.clone()),
    });
    let home = temp_home("start");
    let bin = home.join("rein-remote");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n[ \"$1\" = host ] || exit 3\necho \"args: $*\"\nprintf '{{\"url\":\"{url}\",\"token\":\"t\",\"pid\":%s,\"version\":\"0.1.1\",\"api\":1}}' $$ > \"$REIN_REMOTE_HOME/control.json\"\nexec sleep 30\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let started = Started::default();
    let (stop2, started2) = (stop.clone(), started.clone());
    let opts = Options {
        home: home.clone(),
        start: true,
        binary: Some(bin),
        args: vec!["--test-pattern".into()],
    };
    let t = thread::spawn(move || launch::supervise(opts, started2, stop2, move |m| tx.send(m).is_ok()));
    let mut saw_starting = false;
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Msg::Link(Link::Starting(_)) => saw_starting = true,
            Msg::Link(Link::Connected { info, .. }) => {
                assert_eq!(Some(info.pid), started.pid());
                break;
            }
            Msg::Link(Link::Gone(why)) => panic!("{why}"),
            _ => {}
        }
    }
    assert!(saw_starting);
    stop.store(true, Ordering::Relaxed);
    started.stop();
    assert_eq!(started.pid(), None);
    drop(rx);
    t.join().unwrap();
    let log = std::fs::read_to_string(home.join("host.log")).unwrap();
    assert!(log.contains("args: host --test-pattern"), "{log}");
}
