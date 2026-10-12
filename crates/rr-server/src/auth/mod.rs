//! Sign-in with an identity provider (OIDC, and SAML with `--features saml`), the browser's session
//! after it, and who a viewer is ([`Principal`]) for the policy and the audit log.
//!
//!   GET  /sso/login?next=/…        → the provider's sign-in page
//!   GET  /sso/oidc/callback        ← the provider sends the browser back here (code, state)
//!   POST /sso/saml/acs             ← or here (a signed SAML response, HTTP-POST binding)
//!   GET  /sso/saml/metadata        → this service provider's metadata, for the IdP's admin
//!   POST /sso/ticket               → (with the session cookie) a ticket for /stream, for a minute
//!   GET  /sso/me                   → who this browser is signed in as
//!   POST /sso/logout
//!
//! After signing in the browser holds a session cookie (`rr_session`: HttpOnly, SameSite=Lax, Secure
//! over HTTPS) that names a session kept here (only its SHA-256 is kept). The page trades it for a
//! ticket on the stream's URL, because a browser sends no cookies with WebTransport; the WebSocket
//! takes the cookie too, from this server's own origin only.

pub mod jwt;
pub mod oidc;
#[cfg(feature = "saml")]
pub mod saml;

use crate::policy::{Decision, Perms, Who};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// How long a sign-in lasts in the browser.
pub const SESSION_LIFE: Duration = Duration::from_secs(8 * 3600);
/// How long a sign-in at the provider may take.
const LOGIN_LIFE: Duration = Duration::from_secs(600);
/// Sign-ins in progress at once (each is a few hundred bytes): more are refused.
const MAX_PENDING: usize = 1000;

/// Who a viewer is.
#[derive(Debug, Clone, PartialEq)]
pub struct Principal {
    pub user: String,
    pub groups: Vec<String>,
    /// How they got in: `oidc`, `saml`, `token`, `paired`, `relay` or `open` (no gate at all).
    pub via: &'static str,
    /// When they proved it (the sign-in; for a token or a ticket, when they connected).
    pub since: SystemTime,
}

impl Principal {
    pub fn new(user: impl Into<String>, via: &'static str) -> Self {
        Self {
            user: user.into(),
            groups: vec![],
            via,
            since: SystemTime::now(),
        }
    }

    pub fn who(&self) -> Who<'_> {
        Who {
            user: &self.user,
            groups: &self.groups,
            via: self.via,
        }
    }
}

/// A viewer let in, and what they may do in this session.
#[derive(Debug, Clone)]
pub struct Admission {
    /// This session's id in the audit log (random; not a secret, never a token).
    pub id: String,
    pub principal: Principal,
    pub rule: String,
    pub perms: Perms,
    pub transport: &'static str,
    pub addr: String,
}

impl Admission {
    /// When the session must end by policy: its limit counted from the sign-in.
    pub fn deadline(&self) -> Option<Instant> {
        let max = self.perms.max_session?;
        let since = self.principal.since.elapsed().unwrap_or_default();
        Some(Instant::now() + max.saturating_sub(since))
    }

    pub fn audit_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "session": self.id,
            "user": self.principal.user,
            "groups": self.principal.groups,
            "via": self.principal.via,
            "addr": self.addr,
            "transport": self.transport,
            "rule": self.rule,
            "view_only": !self.perms.input,
            "clipboard": self.perms.clipboard,
            "audio": self.perms.audio,
            "max_session_s": self.perms.max_session.map(|d| d.as_secs()),
        })
    }
}

pub fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("the system's randomness");
    crate::pairing::hex(&b)
}

fn digest(s: &str) -> [u8; 32] {
    sha2::Sha256::digest(s.as_bytes()).into()
}

/// A request to one of the routes, as much of it as they need.
pub struct Req<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    /// A POST's body (the SAML response).
    #[cfg_attr(not(feature = "saml"), allow(dead_code))]
    pub body: &'a str,
    /// The Cookie header.
    pub cookies: &'a str,
    /// The Origin header, when the browser sent one.
    pub origin: Option<&'a str>,
    pub host: Option<&'a str>,
    pub peer: String,
}

/// An answer: status, content type, extra headers, body.
pub struct Resp {
    pub status: &'static str,
    pub kind: &'static str,
    pub headers: Vec<String>,
    pub body: String,
}

impl Resp {
    pub fn json(status: &'static str, v: serde_json::Value) -> Self {
        Self {
            status,
            kind: "application/json",
            headers: vec![],
            body: v.to_string(),
        }
    }

    pub fn redirect(to: &str) -> Self {
        Self {
            status: "302 Found",
            kind: "text/plain",
            headers: vec![format!("Location: {to}")],
            body: String::new(),
        }
    }

    /// A small page that says what happened (sign-in refused, signed out).
    pub fn page(status: &'static str, title: &str, text: &str) -> Self {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
        Self {
            status,
            kind: "text/html; charset=utf-8",
            headers: vec![],
            body: format!(
                "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>Rein Remote</title>\
<body style=\"font:15px system-ui;background:#0b0c0e;color:#e8ecf2;display:grid;place-items:center;height:90vh\">\
<main style=\"max-width:420px\"><h2 style=\"color:#4cc9ff\">{}</h2><p id=why>{}</p><p><a style=\"color:#4cc9ff\" href=\"/sso/login\">Sign in again</a></p></main>",
                esc(title),
                esc(text)
            ),
        }
    }
}

/// A cookie's value from a Cookie header.
pub fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|c| c.trim().strip_prefix(name)?.strip_prefix('='))
}

/// `next=` after signing in: a path on this server only (never another site).
fn local_path(next: &str) -> String {
    if next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') && !next.contains("://") && !next.starts_with("/sso/") {
        next.to_string()
    } else {
        "/".into()
    }
}

struct Pending {
    nonce: String,
    verifier: String,
    /// The SHA-256 of the browser's `rr_login` cookie: the callback must come from the same browser.
    binding: [u8; 32],
    next: String,
    at: Instant,
}

/// Sign-in with an identity provider, and the browsers signed in.
pub struct SignIn {
    /// How this server is reached (`https://rr.example`): the provider sends browsers back there.
    pub public_url: String,
    pub oidc: Option<oidc::Oidc>,
    #[cfg(feature = "saml")]
    pub saml: Option<saml::Saml>,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<[u8; 32], (Principal, Instant)>>,
}

impl SignIn {
    pub fn new(public_url: &str) -> Self {
        Self {
            public_url: public_url.trim_end_matches('/').to_string(),
            oidc: None,
            #[cfg(feature = "saml")]
            saml: None,
            pending: Mutex::default(),
            sessions: Mutex::default(),
        }
    }

    /// `oidc` or `saml`: what the page signs in with.
    pub fn kind(&self) -> &'static str {
        #[cfg(feature = "saml")]
        if self.oidc.is_none() && self.saml.is_some() {
            return "saml";
        }
        "oidc"
    }

    fn secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    fn set_cookie(&self, name: &str, value: &str, path: &str, max_age: u64) -> String {
        format!(
            "Set-Cookie: {name}={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
            if self.secure() { "; Secure" } else { "" }
        )
    }

    /// The signed-in principal for a session cookie (from a Cookie header), while it lasts.
    pub fn session(&self, cookies: &str) -> Option<Principal> {
        let v = cookie(cookies, "rr_session")?;
        let mut s = self.sessions.lock().unwrap();
        let now = Instant::now();
        s.retain(|_, (_, exp)| *exp > now);
        // By the cookie's hash: nothing here depends on how much of the secret matched.
        s.get(&digest(v)).map(|(p, _)| p.clone())
    }

    /// A new session for `p`: its cookie header.
    fn start_session(&self, p: Principal) -> String {
        let v = random_hex(32);
        let mut s = self.sessions.lock().unwrap();
        let now = Instant::now();
        s.retain(|_, (_, exp)| *exp > now);
        s.insert(digest(&v), (p, now + SESSION_LIFE));
        self.set_cookie("rr_session", &v, "/", SESSION_LIFE.as_secs())
    }

    fn end_session(&self, cookies: &str) -> Option<Principal> {
        let v = cookie(cookies, "rr_session")?;
        self.sessions.lock().unwrap().remove(&digest(v)).map(|(p, _)| p)
    }

    /// Starts an OIDC sign-in: where to send the browser, and its binding cookie.
    fn oidc_login(&self, o: &oidc::Oidc, next: &str) -> Result<Resp, Resp> {
        let (state, nonce, verifier, binding) = (oidc::random_b64(24), oidc::random_b64(24), oidc::random_b64(32), random_hex(24));
        let mut p = self.pending.lock().unwrap();
        p.retain(|_, x| x.at.elapsed() < LOGIN_LIFE);
        if p.len() >= MAX_PENDING {
            return Err(Resp::page("503 Service Unavailable", "Too many sign-ins at once", "Try again in a minute."));
        }
        let url = o.login_url(&state, &nonce, &verifier);
        p.insert(
            state,
            Pending {
                nonce,
                verifier,
                binding: digest(&binding),
                next: local_path(next),
                at: Instant::now(),
            },
        );
        let mut r = Resp::redirect(&url);
        r.headers.push(self.set_cookie("rr_login", &binding, "/sso/", LOGIN_LIFE.as_secs()));
        Ok(r)
    }

    /// The provider sent the browser back: the principal it signed in as, and where to go next.
    fn oidc_callback(&self, o: &oidc::Oidc, q: &HashMap<String, String>, cookies: &str) -> Result<(Principal, String), String> {
        if let Some(e) = q.get("error") {
            return Err(format!(
                "the identity provider said {e}{}",
                q.get("error_description").map(|d| format!(": {d}")).unwrap_or_default()
            ));
        }
        let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else {
            return Err("no code or state from the identity provider".into());
        };
        let pending = self.pending.lock().unwrap().remove(state);
        let pending = pending
            .filter(|p| p.at.elapsed() < LOGIN_LIFE)
            .ok_or("this sign-in is unknown or took too long")?;
        // The browser that started it (login CSRF: someone else's code can't sign this browser in).
        let bound = cookie(cookies, "rr_login").is_some_and(|b| rr_proto::pair::confirm_eq(&digest(b), &pending.binding));
        if !bound {
            return Err("this sign-in was started in another browser".into());
        }
        let c = o.finish(code, &pending.verifier, &pending.nonce)?;
        Ok((
            Principal {
                user: c.user,
                groups: c.groups,
                via: "oidc",
                since: SystemTime::now(),
            },
            pending.next,
        ))
    }
}

impl crate::pairing::Gate {
    /// The sign-in routes (`/sso/…`): `Some` when `req.path` is one of them.
    pub fn sso(&self, req: &Req) -> Option<Resp> {
        if !req.path.starts_with("/sso/") {
            return None;
        }
        let Some(s) = self.signin.as_deref() else {
            return Some(Resp::json("404 Not Found", serde_json::json!({"error": "sign-in is off on this machine"})));
        };
        let q = crate::pairing::form(req.query);
        Some(match (req.method, req.path) {
            ("GET", "/sso/login") => {
                let next = q.get("next").map_or("/", String::as_str);
                #[cfg(feature = "saml")]
                if let Some(saml) = s
                    .saml
                    .as_ref()
                    .filter(|_| s.oidc.is_none() || q.get("with").map(String::as_str) == Some("saml"))
                {
                    return Some(
                        saml.login(next)
                            .unwrap_or_else(|e| Resp::page("500 Internal Server Error", "Can't sign in", &e)),
                    );
                }
                match &s.oidc {
                    Some(o) => s.oidc_login(o, next).unwrap_or_else(|r| r),
                    None => Resp::page("404 Not Found", "No sign-in", "No identity provider is set up."),
                }
            }
            ("GET", "/sso/oidc/callback") => match &s.oidc {
                Some(o) => {
                    let r = s.oidc_callback(o, &q, req.cookies);
                    let mut resp = self.signed_in(s, r, req);
                    // The binding cookie is spent either way.
                    resp.headers.push(s.set_cookie("rr_login", "", "/sso/", 0));
                    resp
                }
                None => Resp::page("404 Not Found", "No sign-in", "OIDC isn't set up."),
            },
            #[cfg(feature = "saml")]
            ("POST", "/sso/saml/acs") => match &s.saml {
                Some(saml) => {
                    let f = crate::pairing::form(req.body);
                    let r = saml.acs(&f);
                    self.signed_in(s, r, req)
                }
                None => Resp::page("404 Not Found", "No sign-in", "SAML isn't set up."),
            },
            #[cfg(feature = "saml")]
            ("GET", "/sso/saml/metadata") => match &s.saml {
                Some(saml) => Resp {
                    status: "200 OK",
                    kind: "application/samlmetadata+xml",
                    headers: vec![],
                    body: saml.metadata(),
                },
                None => Resp::page("404 Not Found", "No sign-in", "SAML isn't set up."),
            },
            ("POST", "/sso/ticket") => self.sso_ticket(s, req),
            ("GET", "/sso/me") => match s.session(req.cookies) {
                Some(p) => Resp::json("200 OK", serde_json::json!({"user": p.user, "groups": p.groups, "via": p.via})),
                None => Resp::json("401 Unauthorized", serde_json::json!({"error": "not signed in", "login": "/sso/login"})),
            },
            ("POST", "/sso/logout") | ("GET", "/sso/logout") => {
                if let Some(p) = s.end_session(req.cookies) {
                    self.audit.log("signout", serde_json::json!({"user": p.user, "via": p.via, "addr": req.peer}));
                }
                let mut r = Resp::page("200 OK", "Signed out", "This browser is signed out of Rein Remote.");
                r.headers.push(s.set_cookie("rr_session", "", "/", 0));
                r
            }
            _ => Resp::json("404 Not Found", serde_json::json!({"error": "no such sign-in route"})),
        })
    }

    /// After the provider: the policy's say, the session cookie, the audit log, and on to the page.
    fn signed_in(&self, s: &SignIn, r: Result<(Principal, String), String>, req: &Req) -> Resp {
        let via = s.kind();
        match r {
            Err(e) => {
                self.audit.log("signin_failed", serde_json::json!({"via": via, "addr": req.peer, "reason": e}));
                Resp::page("403 Forbidden", "Sign-in failed", &e)
            }
            Ok((p, next)) => match self.decide(&p) {
                Decision::Deny { rule, reason } => {
                    self.audit.log(
                        "signin_denied",
                        serde_json::json!({"user": p.user, "groups": p.groups, "via": p.via, "addr": req.peer, "rule": rule, "reason": reason}),
                    );
                    Resp::page("403 Forbidden", "Not allowed", &reason)
                }
                Decision::Allow { rule, .. } => {
                    self.audit.log(
                        "signin",
                        serde_json::json!({"user": p.user, "groups": p.groups, "via": p.via, "addr": req.peer, "rule": rule}),
                    );
                    eprintln!("rein-remote: {} signed in ({})", p.user, p.via);
                    let mut r = Resp::redirect(&next);
                    r.headers.push(s.start_session(p));
                    r
                }
            },
        }
    }

    /// A ticket for the stream, for a signed-in browser, from this server's own page.
    fn sso_ticket(&self, s: &SignIn, req: &Req) -> Resp {
        // From this origin only (a page elsewhere can't use this browser's sign-in).
        if !same_origin(req.origin, req.host, &s.public_url) {
            return Resp::json("403 Forbidden", serde_json::json!({"error": "not from this server's page"}));
        }
        let Some(p) = s.session(req.cookies) else {
            return Resp::json("401 Unauthorized", serde_json::json!({"error": "not signed in", "login": "/sso/login"}));
        };
        if let Decision::Deny { reason, .. } = self.decide(&p) {
            return Resp::json("403 Forbidden", serde_json::json!({"error": reason, "user": p.user}));
        }
        let user = p.user.clone();
        Resp::json("200 OK", serde_json::json!({"ticket": self.ticket_for(p), "user": user}))
    }
}

/// Whether a request's Origin is this server's own (its Host, or the public URL it was given). No
/// Origin at all is a request no page made (a native client, curl), which a cookie doesn't come with
/// unless the user put it there.
pub fn same_origin(origin: Option<&str>, host: Option<&str>, public_url: &str) -> bool {
    let Some(o) = origin else { return true };
    let o = o.trim_end_matches('/');
    let authority = o.split_once("://").map_or("", |(_, a)| a);
    o == public_url.trim_end_matches('/') || host.is_some_and(|h| h.eq_ignore_ascii_case(authority))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_stays_on_this_server() {
        assert_eq!(local_path("/?measure"), "/?measure");
        assert_eq!(local_path("//evil.example/x"), "/");
        assert_eq!(local_path("https://evil.example"), "/");
        assert_eq!(local_path("/\\evil.example"), "/");
        assert_eq!(local_path("/sso/logout"), "/");
        assert_eq!(local_path(""), "/");
    }

    #[test]
    fn cookies_and_origins() {
        assert_eq!(cookie("a=1; rr_session=abc; b=2", "rr_session"), Some("abc"));
        assert_eq!(cookie("xrr_session=abc", "rr_session"), None);
        assert!(same_origin(Some("https://rr.example"), Some("other"), "https://rr.example/"));
        assert!(same_origin(Some("http://10.0.0.2:7979"), Some("10.0.0.2:7979"), "https://rr.example"));
        assert!(!same_origin(Some("http://localhost:8080"), Some("localhost:7979"), "https://rr.example"));
        assert!(!same_origin(Some("null"), Some("localhost:7979"), "https://rr.example"));
        assert!(same_origin(None, None, "https://rr.example"));
    }

    #[test]
    fn sessions_are_kept_by_their_cookies_hash() {
        let s = SignIn::new("https://rr.example/");
        let set = s.start_session(Principal::new("alice", "oidc"));
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax") && set.contains("; Secure") && set.contains("Path=/;"));
        let value = set.split_once("rr_session=").unwrap().1.split(';').next().unwrap();
        assert_eq!(s.session(&format!("rr_session={value}")).unwrap().user, "alice");
        assert!(s.session("rr_session=00").is_none());
        assert!(
            !s.sessions.lock().unwrap().keys().any(|k| crate::pairing::hex(k) == value),
            "the cookie itself isn't kept"
        );
        assert_eq!(s.end_session(&format!("rr_session={value}")).unwrap().user, "alice");
        assert!(s.session(&format!("rr_session={value}")).is_none());
        // Over plain HTTP, no Secure (the browser wouldn't keep it).
        assert!(!SignIn::new("http://rr.test:7979").start_session(Principal::new("a", "oidc")).contains("Secure"));
    }
}
