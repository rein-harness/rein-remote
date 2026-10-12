//! The audit log (`--audit FILE`): one JSON object a line, appended and flushed as each thing happens:
//! sign-ins (and refusals), sessions starting and ending (who, from where, over what, which policy
//! rule, recorded where), pairing. Never a token, ticket, cookie, code or secret: a session is named
//! by an id of its own.
//!
//! ```json
//! {"ts":"2026-10-12T09:30:00.123Z","event":"session_start","session":"5f1c…","user":"alice@corp.example","via":"oidc","addr":"10.0.0.7:51234","transport":"websocket","source":"display","rule":"admins","view_only":false,…}
//! ```

use serde_json::{Map, Value};
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct Audit {
    file: Option<Mutex<std::fs::File>>,
}

/// `2026-10-12T09:30:00.123Z` for a time since the Unix epoch (UTC; no time zone database needed).
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (secs, ms) = (d.as_secs() as i64, d.subsec_millis());
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

impl Audit {
    /// Appends to `path` (made if missing; only this user may read it on Unix).
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let file = o.open(path).map_err(|e| format!("--audit {}: {e}", path.display()))?;
        Ok(Self { file: Some(Mutex::new(file)) })
    }

    pub fn on(&self) -> bool {
        self.file.is_some()
    }

    /// One event and its fields (an object), as a line.
    pub fn log(&self, event: &str, fields: Value) {
        let Some(file) = &self.file else { return };
        let mut line = Map::new();
        line.insert("ts".into(), Value::String(rfc3339(SystemTime::now())));
        line.insert("event".into(), Value::String(event.into()));
        if let Value::Object(f) = fields {
            for (k, v) in f {
                if !v.is_null() {
                    line.insert(k, v);
                }
            }
        }
        let mut text = Value::Object(line).to_string();
        text.push('\n');
        let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = f.write_all(text.as_bytes()).and_then(|_| f.flush()) {
            eprintln!("rein-remote: the audit log: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn times_are_utc_rfc3339() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_millis(951_782_400_250)), "2000-02-29T00:00:00.250Z");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_secs(1_791_712_245)), "2026-10-11T09:50:45.000Z");
    }

    #[test]
    fn events_are_json_lines() {
        let path = std::env::temp_dir().join(format!("rr-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let a = Audit::open(&path).unwrap();
        a.log("signin", serde_json::json!({"user": "alice", "ok": true, "reason": null}));
        a.log("session_end", serde_json::json!({"user": "a \"quoted\"\nname", "seconds": 1.5}));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "signin");
        assert_eq!(lines[0]["user"], "alice");
        assert!(lines[0].get("reason").is_none(), "nulls are left out");
        assert_eq!(lines[1]["user"], "a \"quoted\"\nname");
        let _ = std::fs::remove_file(&path);
        // No --audit: nothing, and nothing fails.
        Audit::default().log("x", Value::Null);
    }
}
