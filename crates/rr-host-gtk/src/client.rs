//! The control API's client: each call blocks (the app makes them off its main thread), every error
//! is a sentence to show (the host's own `{"error": …}` when it gave one).

use std::fmt;
use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use crate::model::*;
use crate::sse::SseParser;

/// A failed call: the HTTP status (0 when the host couldn't be reached) and what to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn unreachable(e: impl fmt::Display) -> ApiError {
        ApiError {
            status: 0,
            message: format!("can't reach rein-remote host: {e}"),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("base", &self.base).finish_non_exhaustive()
    }
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::config::Config::builder()
        .http_status_as_error(false)
        .proxy(None)
        .timeout_connect(Some(Duration::from_secs(3)))
        .timeout_global(timeout)
        .build()
        .new_agent()
}

impl Client {
    pub fn new(info: &ControlInfo) -> Client {
        Client::with_url(&info.url, &info.token)
    }

    pub fn with_url(url: &str, token: &str) -> Client {
        Client {
            base: url.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            // Listing windows and stopping sharing can take a moment; nothing takes this long.
            agent: agent(Some(Duration::from_secs(30))),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> ApiResult<Value> {
        let url = format!("{}{}", self.base, path);
        let auth = format!("Bearer {}", self.token);
        let res = match (method, body) {
            ("GET", _) => self.agent.get(&url).header("Authorization", &auth).call(),
            ("DELETE", _) => self.agent.delete(&url).header("Authorization", &auth).call(),
            (m, body) => {
                let b = if m == "PUT" { self.agent.put(&url) } else { self.agent.post(&url) };
                let b = b.header("Authorization", &auth);
                match body {
                    Some(v) => b.header("Content-Type", "application/json").send(v.to_string()),
                    None => b.send_empty(),
                }
            }
        };
        let mut res = res.map_err(ApiError::unreachable)?;
        let status = res.status().as_u16();
        let text = res.body_mut().read_to_string().map_err(ApiError::unreachable)?;
        let value: Option<Value> = serde_json::from_str(&text).ok();
        if (200..300).contains(&status) {
            return value.ok_or_else(|| ApiError {
                status,
                message: format!("the host answered {method} {path} with something that isn't JSON"),
            });
        }
        let message = value
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| match status {
                401 => "the host refused this app's token (restart the app to read control.json again)".into(),
                _ => format!("the host answered {status}"),
            });
        Err(ApiError { status, message })
    }

    fn typed<T: DeserializeOwned>(&self, method: &str, path: &str, body: Option<&Value>) -> ApiResult<T> {
        let v = self.request(method, path, body)?;
        serde_json::from_value(v).map_err(|e| ApiError {
            status: 200,
            message: format!("{method} {path}: unexpected answer: {e}"),
        })
    }

    pub fn state(&self) -> ApiResult<State> {
        self.typed("GET", "/control/state", None)
    }

    pub fn start(&self) -> ApiResult<State> {
        self.typed("POST", "/control/sharing/start", None)
    }

    pub fn stop(&self, reason: Option<&str>) -> ApiResult<State> {
        let body = reason.filter(|r| !r.trim().is_empty()).map(|r| json!({ "reason": r }));
        self.typed("POST", "/control/sharing/stop", body.as_ref())
    }

    pub fn restart(&self) -> ApiResult<State> {
        self.typed("POST", "/control/sharing/restart", None)
    }

    pub fn sources(&self) -> ApiResult<Sources> {
        self.typed("GET", "/control/sources", None)
    }

    pub fn set_source(&self, source: &str) -> ApiResult<State> {
        self.typed("PUT", "/control/source", Some(&json!({ "source": source })))
    }

    pub fn pairing(&self) -> ApiResult<Pairing> {
        self.typed("GET", "/control/pairing", None)
    }

    pub fn new_code(&self) -> ApiResult<Pairing> {
        self.typed("POST", "/control/pairing/code", None)
    }

    pub fn devices(&self) -> ApiResult<Vec<Device>> {
        Ok(self.typed::<Devices>("GET", "/control/devices", None)?.devices)
    }

    pub fn unpair(&self, id: &str) -> ApiResult<Vec<Device>> {
        Ok(self.typed::<Devices>("DELETE", &format!("/control/devices/{}", encode(id)), None)?.devices)
    }

    pub fn viewers(&self) -> ApiResult<Vec<Viewer>> {
        Ok(self.typed::<Viewers>("GET", "/control/viewers", None)?.viewers)
    }

    pub fn kick(&self, id: &str, reason: Option<&str>) -> ApiResult<Vec<Viewer>> {
        let body = reason.filter(|r| !r.trim().is_empty()).map(|r| json!({ "reason": r }));
        let path = format!("/control/viewers/{}/kick", encode(id));
        Ok(self.typed::<Viewers>("POST", &path, body.as_ref())?.viewers)
    }

    pub fn settings(&self) -> ApiResult<Settings> {
        self.typed("GET", "/control/settings", None)
    }

    /// Only the keys given change; `null` puts one back to its default.
    pub fn put_settings(&self, changes: &Map<String, Value>) -> ApiResult<Settings> {
        self.typed("PUT", "/control/settings", Some(&Value::Object(changes.clone())))
    }

    pub fn start_at_login(&self) -> ApiResult<StartAtLogin> {
        self.typed("GET", "/control/start-at-login", None)
    }

    pub fn set_start_at_login(&self, enabled: bool) -> ApiResult<StartAtLogin> {
        self.typed("PUT", "/control/start-at-login", Some(&json!({ "enabled": enabled })))
    }

    pub fn permissions(&self) -> ApiResult<Permissions> {
        self.typed("GET", "/control/permissions", None)
    }

    pub fn signin(&self) -> ApiResult<Signin> {
        self.typed("GET", "/control/admin/signin", None)
    }

    pub fn put_signin(&self, changes: &Map<String, Value>) -> ApiResult<Signin> {
        self.typed("PUT", "/control/admin/signin", Some(&Value::Object(changes.clone())))
    }

    pub fn policy(&self) -> ApiResult<Policy> {
        self.typed("GET", "/control/admin/policy", None)
    }

    /// A policy's text (checked by the host's parser first), or `None` to turn the policy off.
    pub fn put_policy(&self, text: Option<&str>, format: &str) -> ApiResult<Policy> {
        self.typed("PUT", "/control/admin/policy", Some(&json!({ "text": text, "format": format })))
    }

    pub fn sessions(&self) -> ApiResult<Vec<Viewer>> {
        Ok(self.typed::<Sessions>("GET", "/control/admin/sessions", None)?.sessions)
    }

    pub fn audit(&self, q: &AuditQuery) -> ApiResult<AuditPage> {
        self.typed("GET", &format!("/control/admin/audit{}", q.query_string()), None)
    }

    pub fn metrics(&self) -> ApiResult<Metrics> {
        self.typed("GET", "/control/admin/metrics", None)
    }

    /// Follows `GET /control/events`, calling `on_event` for each event, until the stream ends (the
    /// host exited) or `on_event` returns false. `Ok` when the stream ended, `Err` when it couldn't be
    /// opened or broke.
    pub fn events(&self, mut on_event: impl FnMut(Event) -> bool) -> ApiResult<()> {
        // No overall time limit: the stream stays open (a comment every 15 s keeps it so).
        let res = agent(None)
            .get(&format!("{}/control/events", self.base))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "text/event-stream")
            .call()
            .map_err(ApiError::unreachable)?;
        let status = res.status().as_u16();
        if status != 200 {
            let text = res.into_body().read_to_string().unwrap_or_default();
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| format!("the host answered {status} for its events"));
            return Err(ApiError { status, message });
        }
        let mut reader = BufReader::new(res.into_body().into_reader());
        let mut parser = SseParser::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(e) => return Err(ApiError::unreachable(e)),
            }
            if let Some(ev) = parser.line(&line) {
                match Event::parse(&ev.event, &ev.data) {
                    Ok(e) => {
                        if !on_event(e) {
                            return Ok(());
                        }
                    }
                    Err(e) => eprintln!("rein-remote-host-gtk: {e}"),
                }
            }
        }
    }
}

/// The changes between two sets of values, as a PUT body: a key whose value differs, `null` for one
/// cleared ("" where the setting was something).
pub fn changes(before: &Map<String, Value>, after: &Map<String, Value>) -> Map<String, Value> {
    after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
