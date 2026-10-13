//! What the control API answers (docs/control-api.md), as types. Every field has a default, so a host
//! that adds keys, or leaves one out, still reads.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

/// `control.json`: where the API is and its token.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ControlInfo {
    pub url: String,
    pub token: String,
    pub pid: u32,
    pub version: String,
    pub api: u32,
}

/// `GET /control/state`, and the `state` event.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct State {
    pub api: u32,
    pub version: String,
    pub pid: u32,
    pub sharing: bool,
    pub error: Option<String>,
    pub source: String,
    pub listen: String,
    pub links: Vec<Link>,
    pub pairing: Pairing,
    pub viewers: u32,
    pub restart_needed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Link {
    pub kind: String,
    pub label: String,
    pub url: String,
    pub qr: String,
}

/// `state.pairing`, and `GET /control/pairing` (which adds `expires_in_s` and `fingerprint`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Pairing {
    pub enabled: bool,
    pub code: Option<String>,
    pub expires_at: Option<String>,
    pub expires_in_s: Option<u64>,
    pub url: Option<String>,
    pub qr: Option<String>,
    pub fingerprint: Option<String>,
}

/// `GET /control/sources`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sources {
    pub displays: Vec<SourceDisplay>,
    pub windows: Vec<SourceWindow>,
    pub apps: Vec<SourceApp>,
    pub other: Vec<SourceOther>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceDisplay {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceWindow {
    pub id: String,
    pub app: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceApp {
    pub id: String,
    pub name: String,
    pub windows: u32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SourceOther {
    pub id: String,
    pub name: String,
}

/// One row of `GET /control/viewers` (and, with the policy's view, of `/control/admin/sessions`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Viewer {
    pub id: String,
    pub device: String,
    pub user: String,
    pub via: String,
    pub transport: String,
    pub addr: String,
    pub fps: f64,
    pub rtt_ms: Option<f64>,
    pub kbps: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub since: String,
    pub view_only: bool,
    // Live sessions only.
    pub groups: Vec<String>,
    pub rule: Option<String>,
    pub clipboard: Option<bool>,
    pub audio: Option<bool>,
    pub recorded: Option<bool>,
    pub ends_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Viewers {
    pub viewers: Vec<Viewer>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sessions {
    pub sessions: Vec<Viewer>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub since: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Devices {
    pub devices: Vec<Device>,
}

/// `GET /control/settings` (and `PUT`, which adds `applied`), and the `settings` event. The settings
/// themselves are kept as the host sent them (keys are `host.toml`'s, with dashes), read through
/// [`Settings::str`], [`Settings::bool`], [`Settings::list`].
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub settings: Map<String, Value>,
    pub sharing: bool,
    pub source: String,
    pub path: String,
    pub overridden: Vec<String>,
    pub restart_needed: bool,
    pub applied: Option<String>,
}

impl Settings {
    /// A setting as text: a string as it is, a number written out, `null` as "".
    pub fn str(&self, key: &str) -> String {
        value_text(self.settings.get(key))
    }

    pub fn bool(&self, key: &str) -> bool {
        self.settings.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        match self.settings.get(key) {
            Some(Value::Array(a)) => a.iter().map(|v| value_text(Some(v))).collect(),
            Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    pub fn is_overridden(&self, key: &str) -> bool {
        self.overridden.iter().any(|k| k == key)
    }
}

/// What a PUT's `applied` means, to show.
pub fn applied_text(applied: Option<&str>) -> Option<&'static str> {
    match applied? {
        "saved" => Some("Saved. Used the next time sharing starts."),
        "restarted" => Some("Saved. Sharing restarted to apply it."),
        "on_restart" => Some("Saved. Viewers are connected: it applies when sharing restarts."),
        _ => Some("Saved."),
    }
}

/// A JSON value as one line of text: strings as they are, `null` as "".
pub fn value_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct StartAtLogin {
    pub enabled: bool,
    pub kind: Option<String>,
    pub name: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Permissions {
    pub platform: String,
    pub screen_recording: String,
    pub accessibility: String,
}

/// `GET /control/admin/signin`: kept as sent (dashed keys); `kind` and `saml_available` read out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signin {
    pub fields: Map<String, Value>,
}

impl<'de> Deserialize<'de> for Signin {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Signin { fields: Map::deserialize(d)? })
    }
}

impl Signin {
    pub fn kind(&self) -> Option<String> {
        self.fields.get("kind").and_then(Value::as_str).map(str::to_owned)
    }
    pub fn str(&self, key: &str) -> String {
        value_text(self.fields.get(key))
    }
    pub fn bool(&self, key: &str) -> bool {
        self.fields.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn applied(&self) -> Option<String> {
        self.fields.get("applied").and_then(Value::as_str).map(str::to_owned)
    }
}

/// `GET /control/admin/policy`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Policy {
    pub enabled: bool,
    pub path: Option<String>,
    pub format: Option<String>,
    pub rules: u32,
    pub text: Option<String>,
    pub applied: Option<String>,
}

/// `GET /control/admin/audit`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AuditPage {
    pub path: Option<String>,
    pub entries: Vec<Map<String, Value>>,
    #[serde(deserialize_with = "text_or_number")]
    pub next: Option<String>,
}

fn text_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Null => None,
        Value::String(s) => Some(s),
        v => Some(v.to_string()),
    })
}

/// The audit log's event types (for the filter).
pub const AUDIT_EVENTS: &[&str] = &[
    "signin",
    "signin_denied",
    "signout",
    "session_start",
    "session_end",
    "session_refused",
    "session_kicked",
    "pair",
    "pair_failed",
    "device_signin",
    "device_signin_failed",
    "unpair",
    "server_start",
    "sharing_start",
    "sharing_stop",
    "settings_changed",
    "policy_changed",
    "signin_changed",
];

/// An audit query (`GET /control/admin/audit`'s parameters).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuditQuery {
    pub events: Vec<String>,
    pub user: String,
    pub since: String,
    pub limit: u32,
    pub before: Option<String>,
}

impl AuditQuery {
    /// The query string, `?…`, or "" with nothing to ask.
    pub fn query_string(&self) -> String {
        let mut q = Vec::new();
        if !self.events.is_empty() {
            q.push(format!("event={}", encode(&self.events.join(","))));
        }
        if !self.user.trim().is_empty() {
            q.push(format!("user={}", encode(self.user.trim())));
        }
        if !self.since.trim().is_empty() {
            q.push(format!("since={}", encode(self.since.trim())));
        }
        if self.limit > 0 {
            q.push(format!("limit={}", self.limit));
        }
        if let Some(b) = &self.before {
            q.push(format!("before={}", encode(b)));
        }
        if q.is_empty() {
            String::new()
        } else {
            format!("?{}", q.join("&"))
        }
    }
}

/// Percent-encoding for a query value or a path segment (everything but unreserved characters).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `GET /control/admin/metrics`: shown as rows of names and values, so kept as sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    pub fields: Map<String, Value>,
}

impl<'de> Deserialize<'de> for Metrics {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Metrics { fields: Map::deserialize(d)? })
    }
}

impl Metrics {
    /// One `(name, value)` per counter, nested objects flattened (`viewers.direct`), each relay a
    /// line of its own.
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        for (k, v) in &self.fields {
            flatten(k, v, &mut rows);
        }
        rows
    }
}

fn flatten(prefix: &str, v: &Value, rows: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                flatten(&format!("{prefix}.{k}"), v, rows);
            }
        }
        Value::Array(a) if a.iter().all(Value::is_object) && !a.is_empty() => {
            for (i, item) in a.iter().enumerate() {
                let name = item.get("relay").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| i.to_string());
                let text = item
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .filter(|(k, _)| k.as_str() != "relay")
                            .map(|(k, v)| format!("{k} {}", value_text(Some(v))))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                rows.push((format!("{prefix} {name}"), text));
            }
        }
        Value::Array(a) if a.is_empty() => rows.push((prefix.to_owned(), "none".into())),
        v => rows.push((prefix.to_owned(), value_text(Some(v)))),
    }
}

/// A live event from `GET /control/events`.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    State(State),
    Viewers(Vec<Viewer>),
    Devices(Vec<Device>),
    Settings(Settings),
    /// An event this app doesn't know (a newer host): ignored.
    Other(String),
}

impl Event {
    /// An SSE event's name and data, read. `Err` with why when the data isn't what the doc says.
    pub fn parse(name: &str, data: &str) -> Result<Event, String> {
        let bad = |e: serde_json::Error| format!("event {name}: {e}");
        Ok(match name {
            "state" => Event::State(serde_json::from_str(data).map_err(bad)?),
            "viewers" => Event::Viewers(serde_json::from_str::<Viewers>(data).map_err(bad)?.viewers),
            "devices" => Event::Devices(serde_json::from_str::<Devices>(data).map_err(bad)?.devices),
            "settings" => Event::Settings(serde_json::from_str(data).map_err(bad)?),
            other => Event::Other(other.to_owned()),
        })
    }
}

/// A source id, said for a person ("Test pattern 1280×720", "Window 4242").
pub fn source_label(id: &str) -> String {
    let (kind, rest) = id.split_once(':').unwrap_or((id, ""));
    match (kind, rest) {
        ("display", "") => "The display".into(),
        ("display", n) => format!("Display {n}"),
        ("window", n) => format!("Window {n}"),
        ("app", n) => format!("App {n}"),
        ("test-pattern", "") => "Test pattern".into(),
        ("test-pattern", size) => format!("Test pattern {}", size.replace('x', "×")),
        ("vnc", addr) => format!("VNC {addr}"),
        ("", _) => "Nothing chosen".into(),
        _ => id.to_owned(),
    }
}
