//! The host's control API: HTTP + JSON on this machine (docs/control-api.md is the contract), a
//! bearer token compared in constant time, and Server-Sent Events for what changes.

use crate::host::{sources, Answer, Fail, Host};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The most a request's body may be.
const MAX_BODY: usize = 1 << 20;

/// The API on `listen` (loopback unless `remote`; then HTTPS only from elsewhere), answering requests
/// with `token`. Returns where it listens.
pub fn serve(host: Arc<Host>, listen: &str, remote: bool, tls: Option<Arc<rustls::ServerConfig>>, token: String) -> Result<SocketAddr, String> {
    let listener = TcpListener::bind(listen).map_err(|e| format!("the control API can't listen on {listen}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    if !addr.ip().is_loopback() && !remote {
        return Err(format!(
            "--control-listen {listen}: the control API is for this computer (127.0.0.1); from elsewhere it needs --control-remote on"
        ));
    }
    let token = Arc::new(token);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let (host, tls, token) = (host.clone(), tls.clone(), token.clone());
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, &host, tls.as_ref(), &token, addr.port(), remote) {
                    if !e.is_empty() {
                        eprintln!("rein-remote: control API: {e}");
                    }
                }
            });
        }
    });
    Ok(addr)
}

/// A header's value (the first by that name).
fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    }
}

fn send(conn: &mut impl Write, code: u16, body: &Value) -> Result<(), String> {
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{}\r\n",
        status_text(code),
        body.len(),
        if code == 401 { "WWW-Authenticate: Bearer\r\n" } else { "" }
    );
    conn.write_all(head.as_bytes())
        .and_then(|_| conn.write_all(body.as_bytes()))
        .and_then(|_| conn.flush())
        .map_err(|e| e.to_string())
}

fn error(conn: &mut impl Write, Fail(code, why): Fail) -> Result<(), String> {
    send(conn, code, &json!({"error": why}))
}

fn handle(stream: TcpStream, host: &Host, tls: Option<&Arc<rustls::ServerConfig>>, token: &str, port: u16, remote: bool) -> Result<(), String> {
    let peer = stream.peer_addr().map_err(|e| e.to_string())?;
    // From elsewhere, HTTPS only (the token would otherwise cross the network in the clear).
    if remote && !peer.ip().is_loopback() {
        let mut first = [0u8; 1];
        if stream.peek(&mut first).map_err(|e| e.to_string())? != 1 || first[0] != 0x16 {
            return Ok(());
        }
    }
    let mut conn = crate::conn::Conn::accept(stream, tls)?;
    let head = conn.read_head()?;
    let req = String::from_utf8_lossy(&head).to_string();
    let mut first = req.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("/").to_string());
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    // This computer's names only: a web page can't reach the API through a name it controls (DNS rebinding).
    if !remote {
        let ok = header(&req, "host")
            .is_some_and(|h| [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].contains(&h.to_ascii_lowercase()));
        if !ok {
            return error(&mut conn, Fail(403, "the control API answers requests to 127.0.0.1 or localhost only".into()));
        }
    }
    // The token as a bearer, compared in full (never from the URL).
    let authorized = crate::metrics::bearer(&req).is_some_and(|t| rr_proto::pair::confirm_eq(t.as_bytes(), token.as_bytes()));
    if !authorized {
        return error(
            &mut conn,
            Fail(401, "the control API needs its token (Authorization: Bearer, from control.json)".into()),
        );
    }
    // The body (JSON), after the head and up to its length.
    let len: usize = header(&req, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return error(&mut conn, Fail(413, "a request body over 1 MB".into()));
    }
    let at = head.windows(4).position(|w| w == b"\r\n\r\n").map_or(head.len(), |p| p + 4);
    let mut body = head[at..].to_vec();
    let mut b = [0u8; 8192];
    while body.len() < len {
        let n = conn.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&b[..n]);
    }
    body.truncate(len);
    let body: Value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return error(&mut conn, Fail(400, format!("the body isn't JSON: {e}"))),
        }
    };
    let segs: Vec<String> = path
        .trim_end_matches('/')
        .split('/')
        .skip(1)
        .map(|s| crate::pairing::form(&format!("s={s}")).remove("s").unwrap_or_default())
        .collect();
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
    if method == "GET" && segs == ["control", "events"] {
        return events(&mut conn, host);
    }
    let q = crate::pairing::form(query);
    match route(host, &method, &segs, &body, &q) {
        Ok(v) => send(&mut conn, 200, &v),
        Err(f) => error(&mut conn, f),
    }
}

fn reason(body: &Value) -> Option<&str> {
    body.get("reason").and_then(Value::as_str).filter(|r| !r.trim().is_empty())
}

fn route(host: &Host, method: &str, segs: &[&str], body: &Value, q: &std::collections::HashMap<String, String>) -> Answer {
    let Some(("control", rest)) = segs.split_first().map(|(a, b)| (*a, b)) else {
        return Err(Fail(404, "no such endpoint (they're under /control/: docs/control-api.md)".into()));
    };
    match (method, rest) {
        ("GET", ["state"]) => Ok(host.state()),
        ("POST", ["sharing", "start"]) => host.start(),
        ("POST", ["sharing", "stop"]) => host.stop(reason(body)),
        ("POST", ["sharing", "restart"]) => host.restart(),
        ("GET", ["sources"]) => Ok(sources()),
        ("PUT", ["source"]) => match body.get("source").and_then(Value::as_str) {
            Some(id) => host.switch(id),
            None => Err(Fail(400, "{\"source\": an id from GET /control/sources}".into())),
        },
        ("GET", ["pairing"]) => Ok(host.pairing()),
        ("POST", ["pairing", "code"]) => host.new_code(),
        ("GET", ["devices"]) => Ok(host.devices()),
        ("DELETE", ["devices", id]) => host.unpair(id),
        ("GET", ["viewers"]) => Ok(host.viewers()),
        ("POST", ["viewers", id, "kick"]) => host.kick(id, reason(body)),
        ("GET", ["settings"]) => Ok(host.settings()),
        ("PUT", ["settings"]) => host.put_settings(body),
        ("GET", ["start-at-login"]) => host.login(),
        ("PUT", ["start-at-login"]) => host.set_login(body),
        ("GET", ["permissions"]) => Ok(crate::host::permissions::get()),
        ("POST", ["permissions", "open"]) => {
            let which = body.get("which").and_then(Value::as_str).unwrap_or("");
            let open = body.get("open").and_then(Value::as_bool).unwrap_or(true);
            crate::host::permissions::open(which, open).map_err(|e| Fail(400, e))
        }
        ("GET", ["admin", "signin"]) => Ok(host.signin()),
        ("PUT", ["admin", "signin"]) => host.put_signin(body),
        ("GET", ["admin", "policy"]) => Ok(host.policy()),
        ("PUT", ["admin", "policy"]) => host.put_policy(body),
        ("GET", ["admin", "sessions"]) => Ok(host.admin_sessions()),
        ("GET", ["admin", "audit"]) => host.audit(q),
        ("GET", ["admin", "metrics"]) => Ok(crate::metrics::serve_json()),
        (_, path) => {
            let known = matches!(
                path,
                ["state" | "events" | "sources" | "source" | "pairing" | "devices" | "viewers" | "settings" | "start-at-login" | "permissions"]
                    | ["sharing", "start" | "stop" | "restart"]
                    | ["pairing", "code"]
                    | ["devices", _]
                    | ["viewers", _, "kick"]
                    | ["permissions", "open"]
                    | ["admin", "signin" | "policy" | "sessions" | "audit" | "metrics"]
            );
            if known {
                Err(Fail(405, format!("{method} isn't how this endpoint is used (docs/control-api.md)")))
            } else {
                Err(Fail(404, "no such endpoint (docs/control-api.md lists them)".into()))
            }
        }
    }
}

/// `GET /control/events`: Server-Sent Events until the app goes (a comment every 15 s meanwhile).
fn events(conn: &mut crate::conn::Conn, host: &Host) -> Result<(), String> {
    let rx = host.subscribe();
    conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\n\r\n")
        .map_err(|e| e.to_string())?;
    loop {
        let text = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(t) => t,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => ": keepalive\n\n".to_string(),
            Err(_) => return Ok(()),
        };
        if conn.write_all(text.as_bytes()).and_then(|_| conn.flush()).is_err() {
            return Ok(());
        }
    }
}

/// `control.json`: where the API is and its token, readable by this user only.
pub fn write_file(path: &Path, url: &str, token: &str) -> Result<(), String> {
    let v = json!({"url": url, "token": token, "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"), "api": crate::host::API});
    crate::identity::write_private(path, v.to_string().as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    // Windows: no permission inherited from the folder; full control for its owner only ("Owner
    // Rights"). The folder (%APPDATA%) is the user's, SYSTEM's and Administrators' already.
    #[cfg(windows)]
    {
        let out = std::process::Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", "*S-1-3-4:F"])
            .output();
        if !out.as_ref().is_ok_and(|o| o.status.success()) {
            eprintln!(
                "rein-remote: couldn't narrow who can read {} (icacls); it keeps its folder's permissions",
                path.display()
            );
        }
    }
    Ok(())
}

/// Refuses to start while another host for this home folder answers (its `control.json`).
pub fn refuse_second(path: &Path) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(()) };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Ok(()) };
    let (Some(url), Some(token)) = (v["url"].as_str(), v["token"].as_str()) else {
        return Ok(());
    };
    let Some(at) = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) else {
        return Ok(());
    };
    let Ok(addr) = at.parse::<SocketAddr>() else { return Ok(()) };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return Ok(());
    };
    let answers = if url.starts_with("https://") {
        true
    } else {
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = write!(
            s,
            "GET /control/state HTTP/1.1\r\nHost: {at}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        );
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out.starts_with("HTTP/1.1 200")
    };
    if answers {
        return Err(format!(
            "another rein-remote host is running for {} (pid {}, its control API at {url})",
            path.parent().map_or(String::new(), |p| p.display().to_string()),
            v["pid"]
        ));
    }
    Ok(())
}
