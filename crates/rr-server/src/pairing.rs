//! Who may watch and control: the link's token, or a device paired once with a code. The protocol is
//! rr-proto's `pair` (SPAKE2 over P-256, then each side's long-term key); this is the server's side
//! of it over HTTP, the list of paired devices, and the tickets a paired device signs in for.
//!
//!   POST /pair/start   x=<client point>&name=…      → the server's point, its confirmation, its key
//!   POST /pair/finish  session=…&confirm=…&key=…&mac=…&name=…   → kept: the device is paired
//!   GET  /auth/challenge?cn=<client nonce>           → a nonce, the server's key, its signature
//!   POST /auth         nonce=…&key=…&sig=…          → a ticket: /stream?ticket=… for a minute
//!
//! The code is printed when the server starts (and as a QR code), lasts ten minutes, and is spent
//! after a pairing or five wrong tries: an attacker gets five guesses at a million codes.

use crate::audit::Audit;
use crate::auth::{random_hex, Admission, Principal, SignIn};
use crate::identity::{fingerprint, verify, Identity};
use crate::policy::{Decision, Perms, Policy};
use rr_proto::pair::{client_auth_message, confirm_eq, key_mac, server_auth_message, Keys, Role, Spake};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CODE_LIFE: Duration = Duration::from_secs(600);
const TRIES: u8 = 5;
const TICKET_LIFE: Duration = Duration::from_secs(60);
const CHALLENGE_LIFE: Duration = Duration::from_secs(60);

struct Code {
    code: String,
    expires: Instant,
    tries_left: u8,
    /// Exchanges started with it, by session id: the keys, until the client confirms.
    sessions: HashMap<String, (Keys, Instant)>,
}

/// The gate in front of the stream: who a viewer is (the link's token, a paired device's ticket, a
/// browser signed in with the identity provider) and what the policy lets them do.
pub struct Gate {
    token: Option<String>,
    identity: Option<Identity>,
    code: Mutex<Option<Code>>,
    /// Tickets (a minute's use), by their SHA-256, with who they're for.
    tickets: Mutex<HashMap<[u8; 32], (Instant, Principal)>>,
    /// Tickets for one session (a direct path offered inside a relayed session), by their SHA-256.
    once: Mutex<HashMap<[u8; 32], (Instant, Principal)>>,
    challenges: Mutex<HashMap<Vec<u8>, Instant>>,
    /// Sign-in with an identity provider, when it's set up.
    pub signin: Option<Arc<SignIn>>,
    pub policy: Option<Policy>,
    pub audit: Arc<Audit>,
    /// What this server shows, as a policy's `sources` name it (`display`, `window:Simulator`…).
    pub source: String,
}

/// A device that paired.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub public: Vec<u8>,
    pub name: String,
    pub since: u64,
}

fn paired_path() -> std::path::PathBuf {
    crate::identity::home().join("paired")
}

/// The paired devices (one a line: fingerprint, key, unix time, name; tab-separated).
pub fn devices() -> Vec<Device> {
    std::fs::read_to_string(paired_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(4, '\t');
            let (id, key, since, name) = (f.next()?, f.next()?, f.next()?, f.next().unwrap_or(""));
            Some(Device {
                id: id.to_string(),
                public: unhex(key)?,
                name: name.to_string(),
                since: since.parse().unwrap_or(0),
            })
        })
        .collect()
}

fn save(list: &[Device]) -> Result<(), String> {
    let text: String = list
        .iter()
        .map(|d| format!("{}\t{}\t{}\t{}\n", d.id, hex(&d.public), d.since, d.name.replace(['\t', '\n'], " ")))
        .collect();
    crate::identity::write_private(&paired_path(), text.as_bytes()).map_err(|e| e.to_string())
}

/// Forgets a paired device by its fingerprint (or the start of it). Whether one was.
pub fn unpair(id: &str) -> Result<bool, String> {
    let list = devices();
    let keep: Vec<Device> = list.iter().filter(|d| !d.id.starts_with(id) || id.is_empty()).cloned().collect();
    let gone = keep.len() != list.len();
    if gone {
        save(&keep)?;
    }
    Ok(gone)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("the system's randomness");
    b
}

/// Compared in full (no early exit).
fn same(a: &str, b: &str) -> bool {
    confirm_eq(a.as_bytes(), b.as_bytes())
}

/// `a=1&b=2` (a form body or a query string) as pairs, percent-decoded.
pub fn form(s: &str) -> HashMap<String, String> {
    let dec = |v: &str| -> String {
        let b = v.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(x) => {
                        out.push(x);
                        i += 2;
                    }
                    None => out.push(b'%'),
                },
                c => out.push(c),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    s.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (dec(k), dec(v))
        })
        .collect()
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn error(status: &'static str, why: &str) -> (&'static str, String) {
    (status, format!("{{\"error\":{}}}", json_str(why)))
}

impl Gate {
    /// `pairing`: hold a code and answer pairing and sign-in (needs this machine's key).
    pub fn new(token: Option<String>, pairing: bool) -> Self {
        let identity = if pairing {
            Identity::load_or_create().map_err(|e| eprintln!("rein-remote: no pairing ({e})")).ok()
        } else {
            None
        };
        Self {
            token,
            identity,
            code: Mutex::new(None),
            tickets: Mutex::new(HashMap::new()),
            once: Mutex::new(HashMap::new()),
            challenges: Mutex::new(HashMap::new()),
            signin: None,
            policy: None,
            audit: Arc::default(),
            source: String::new(),
        }
    }

    /// With sign-in, a policy, an audit log, and the name of what it shows.
    pub fn with_access(mut self, signin: Option<Arc<SignIn>>, policy: Option<Policy>, audit: Arc<Audit>, source: String) -> Self {
        self.signin = signin;
        self.policy = policy;
        self.audit = audit;
        self.source = source;
        self
    }

    /// Whether anything at all guards the stream (a token, pairing, or sign-in).
    pub fn guarded(&self) -> bool {
        self.token.is_some() || self.identity.is_some() || self.signin.is_some()
    }

    pub fn fingerprint(&self) -> Option<String> {
        self.identity.as_ref().map(|i| i.fingerprint())
    }

    /// A fresh one-time code (six digits, `123-456`), replacing any other.
    pub fn new_code(&self) -> Option<String> {
        self.identity.as_ref()?;
        let n = u32::from_le_bytes(random(4).try_into().unwrap()) % 1_000_000;
        let code = format!("{:03}-{:03}", n / 1000, n % 1000);
        *self.code.lock().unwrap() = Some(Code {
            code: code.clone(),
            expires: Instant::now() + CODE_LIFE,
            tries_left: TRIES,
            sessions: HashMap::new(),
        });
        Some(code)
    }

    /// Whether a request's query (`token=…` or `ticket=…`) lets it in.
    #[cfg(test)]
    pub fn allows(&self, query: &str) -> bool {
        self.admit(query, None).is_some()
    }

    /// Who a request is: by the query's `token=` or `ticket=`, or a signed-in browser's session cookie
    /// (`cookies`: the Cookie header, only from this server's own page). None: not let in.
    pub fn admit(&self, query: &str, cookies: Option<&str>) -> Option<Principal> {
        if !self.guarded() {
            return Some(Principal::new("anyone", "open"));
        }
        let q = form(query);
        if let (Some(t), Some(given)) = (&self.token, q.get("token")) {
            if same(t, given) {
                return Some(Principal::new("token", "token"));
            }
        }
        if let Some(given) = q.get("ticket") {
            // By the ticket's hash: nothing here depends on how much of the secret matched.
            let key: [u8; 32] = sha2::Sha256::digest(given.as_bytes()).into();
            let mut once = self.once.lock().unwrap();
            once.retain(|_, (exp, _)| *exp > Instant::now());
            if let Some((_, p)) = once.remove(&key) {
                return Some(p);
            }
            drop(once);
            let mut tickets = self.tickets.lock().unwrap();
            tickets.retain(|_, (exp, _)| *exp > Instant::now());
            // A minute's use (the page tries WebTransport, then the WebSocket, with the same one).
            if let Some((_, p)) = tickets.get(&key) {
                return Some(p.clone());
            }
        }
        if let (Some(s), Some(c)) = (&self.signin, cookies) {
            return s.session(c);
        }
        None
    }

    /// What the policy lets `p` do here (everything, without a policy).
    pub fn decide(&self, p: &Principal) -> Decision {
        match &self.policy {
            Some(policy) => policy.decide(&p.who(), &self.source),
            None => Decision::Allow {
                rule: "none".into(),
                perms: Perms::FULL,
            },
        }
    }

    /// `p` let in for a session over `transport` from `addr`, or why not (said in the audit log).
    pub fn admission(&self, p: Principal, transport: &'static str, addr: String) -> Result<Admission, String> {
        match self.decide(&p) {
            Decision::Allow { rule, perms } => Ok(Admission {
                id: random_hex(8),
                principal: p,
                rule,
                perms,
                transport,
                addr,
            }),
            Decision::Deny { rule, reason } => {
                self.audit.log(
                    "session_refused",
                    serde_json::json!({"user": p.user, "groups": p.groups, "via": p.via, "addr": addr, "transport": transport, "source": self.source, "rule": rule, "reason": reason}),
                );
                eprintln!("rein-remote: refused {} ({reason})", p.user);
                Err(reason)
            }
        }
    }

    /// A ticket for `p` (a minute's use): the page puts it on the stream's URL.
    pub fn ticket_for(&self, p: Principal) -> String {
        let t = random_hex(24);
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, (exp, _)| *exp > Instant::now());
        tickets.insert(sha2::Sha256::digest(t.as_bytes()).into(), (Instant::now() + TICKET_LIFE, p));
        t
    }

    /// A ticket that lets one session in, within a minute, as `p` (a direct path offered to a viewer
    /// who came through the relay: it opens WebTransport straight here with it).
    pub fn ticket_once(&self, p: Principal) -> String {
        let t = random_hex(24);
        let mut once = self.once.lock().unwrap();
        once.retain(|_, (exp, _)| *exp > Instant::now());
        once.insert(sha2::Sha256::digest(t.as_bytes()).into(), (Instant::now() + TICKET_LIFE, p));
        t
    }

    /// The pairing and sign-in routes: `Some((status, json))` when `path` is one of them.
    pub fn handle(&self, method: &str, path: &str, query: &str, body: &str) -> Option<(&'static str, String)> {
        if !matches!(path, "/pair/start" | "/pair/finish" | "/auth/challenge" | "/auth") {
            return None;
        }
        let Some(id) = &self.identity else {
            return Some(error("404 Not Found", "pairing is off on this machine"));
        };
        Some(match (method, path) {
            ("POST", "/pair/start") => self.start(id, &form(body)),
            ("POST", "/pair/finish") => self.finish(&form(body)),
            ("GET", "/auth/challenge") => self.challenge(id, &form(query)),
            ("POST", "/auth") => self.sign_in(id, &form(body)),
            _ => error("405 Method Not Allowed", "wrong method"),
        })
    }

    fn start(&self, id: &Identity, f: &HashMap<String, String>) -> (&'static str, String) {
        let mut slot = self.code.lock().unwrap();
        let Some(code) = slot.as_mut().filter(|c| c.expires > Instant::now() && c.tries_left > 0) else {
            *slot = None;
            return error(
                "403 Forbidden",
                "no pairing code is open on this machine (start rein-remote again for a new one)",
            );
        };
        let Some(x) = f.get("x").and_then(|x| unhex(x)) else {
            return error("400 Bad Request", "x?");
        };
        // Every start is a guess at the code, right or wrong.
        code.tries_left -= 1;
        let s = Spake::start(Role::Server, &code.code, random(32).try_into().unwrap());
        let Ok(keys) = s.finish(&x) else {
            return error("400 Bad Request", "not a point");
        };
        let session = hex(&random(16));
        code.sessions.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(60));
        code.sessions.insert(session.clone(), (keys.clone(), Instant::now()));
        let public = id.public();
        (
            "200 OK",
            format!(
                r#"{{"session":"{session}","y":"{}","confirm":"{}","server_key":"{}","server_key_mac":"{}","id":"{}"}}"#,
                hex(&s.message()),
                hex(&keys.confirm_mine),
                hex(&public),
                hex(&key_mac(&keys.shared, "server", &public)),
                id.fingerprint()
            ),
        )
    }

    fn finish(&self, f: &HashMap<String, String>) -> (&'static str, String) {
        let mut slot = self.code.lock().unwrap();
        let Some(code) = slot.as_mut() else {
            return error("403 Forbidden", "no pairing open");
        };
        let Some((keys, _)) = f.get("session").and_then(|s| code.sessions.remove(s)) else {
            return error("403 Forbidden", "no such pairing (it took too long?)");
        };
        let (confirm, key, mac) = (
            f.get("confirm").and_then(|x| unhex(x)).unwrap_or_default(),
            f.get("key").and_then(|x| unhex(x)).unwrap_or_default(),
            f.get("mac").and_then(|x| unhex(x)).unwrap_or_default(),
        );
        if !confirm_eq(&confirm, &keys.confirm_theirs) {
            return error("403 Forbidden", "wrong code");
        }
        if key.len() != 65 || !confirm_eq(&mac, &key_mac(&keys.shared, "client", &key)) {
            return error("403 Forbidden", "the device's key didn't check out");
        }
        // Paired: the code is spent.
        *slot = None;
        let device = Device {
            id: fingerprint(&key),
            public: key,
            name: f.get("name").cloned().unwrap_or_default().chars().take(80).collect(),
            since: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        };
        let mut list = devices();
        list.retain(|d| d.id != device.id);
        list.push(device.clone());
        if let Err(e) = save(&list) {
            return error("500 Internal Server Error", &format!("couldn't keep the pairing: {e}"));
        }
        eprintln!(
            "rein-remote: paired with {} ({})",
            if device.name.is_empty() { "a device" } else { &device.name },
            device.id
        );
        ("200 OK", format!(r#"{{"ok":true,"id":"{}"}}"#, device.id))
    }

    fn challenge(&self, id: &Identity, q: &HashMap<String, String>) -> (&'static str, String) {
        let Some(cn) = q.get("cn").and_then(|x| unhex(x)).filter(|c| (16..=64).contains(&c.len())) else {
            return error("400 Bad Request", "cn?");
        };
        let nonce = random(32);
        let mut ch = self.challenges.lock().unwrap();
        ch.retain(|_, t| t.elapsed() < CHALLENGE_LIFE);
        if ch.len() > 256 {
            return error("429 Too Many Requests", "too many sign-ins at once");
        }
        ch.insert(nonce.clone(), Instant::now());
        (
            "200 OK",
            format!(
                r#"{{"nonce":"{}","server_key":"{}","sig":"{}","id":"{}"}}"#,
                hex(&nonce),
                hex(&id.public()),
                hex(&id.sign(&server_auth_message(&cn, &nonce))),
                id.fingerprint()
            ),
        )
    }

    fn sign_in(&self, id: &Identity, f: &HashMap<String, String>) -> (&'static str, String) {
        let Some(nonce) = f.get("nonce").and_then(|x| unhex(x)) else {
            return error("400 Bad Request", "nonce?");
        };
        // Each challenge answers once.
        let fresh = self.challenges.lock().unwrap().remove(&nonce).is_some_and(|t| t.elapsed() < CHALLENGE_LIFE);
        if !fresh {
            return error("403 Forbidden", "that challenge is used or too old");
        }
        let key = f.get("key").and_then(|x| unhex(x)).unwrap_or_default();
        let sig = f.get("sig").and_then(|x| unhex(x)).unwrap_or_default();
        let known = devices().into_iter().find(|d| confirm_eq(&d.public, &key));
        let Some(device) = known.filter(|_| verify(&key, &client_auth_message(&nonce, &id.public()), &sig)) else {
            return error("403 Forbidden", "this device isn't paired with this machine");
        };
        // The device by its name (and its fingerprint, which a policy can name too).
        let user = if device.name.is_empty() {
            format!("device {}", device.id)
        } else {
            format!("{} ({})", device.name, device.id)
        };
        let ticket = self.ticket_for(Principal::new(user, "paired"));
        ("200 OK", format!(r#"{{"ticket":"{ticket}"}}"#))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer;

    /// A client, as the web client and native clients do it, against the gate.
    #[test]
    fn pairs_with_the_code_then_signs_in_with_its_key() {
        let _env = crate::identity::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rr-pair-{}", std::process::id()));
        std::env::set_var("REIN_REMOTE_HOME", &dir);
        let gate = Gate::new(Some("tok".into()), true);
        let code = gate.new_code().unwrap();
        assert!(gate.allows("token=tok") && !gate.allows("token=nope") && !gate.allows(""));

        // A wrong code: the confirmation doesn't match, and it costs a try.
        let wrong = Spake::start(Role::Client, "000-000", [7; 32]);
        let (st, body) = gate.handle("POST", "/pair/start", "", &format!("x={}", hex(&wrong.message()))).unwrap();
        assert_eq!(st, "200 OK");
        let y = unhex(&json(&body, "y")).unwrap();
        let k = wrong.finish(&y).unwrap();
        assert!(!confirm_eq(&k.confirm_theirs, &unhex(&json(&body, "confirm")).unwrap()));

        // The right code.
        let client = p256::ecdsa::SigningKey::from_slice(&[9; 32]).unwrap();
        let public = client.verifying_key().to_encoded_point(false).as_bytes().to_vec();
        let s = Spake::start(Role::Client, &code, [8; 32]);
        let (_, body) = gate.handle("POST", "/pair/start", "", &format!("x={}", hex(&s.message()))).unwrap();
        let k = s.finish(&unhex(&json(&body, "y")).unwrap()).unwrap();
        assert!(confirm_eq(&k.confirm_theirs, &unhex(&json(&body, "confirm")).unwrap()));
        let server = unhex(&json(&body, "server_key")).unwrap();
        assert!(confirm_eq(
            &key_mac(&k.shared, "server", &server),
            &unhex(&json(&body, "server_key_mac")).unwrap()
        ));
        let finish = format!(
            "session={}&confirm={}&key={}&mac={}&name=test+phone",
            json(&body, "session"),
            hex(&k.confirm_mine),
            hex(&public),
            hex(&key_mac(&k.shared, "client", &public))
        );
        let (st, _) = gate.handle("POST", "/pair/finish", "", &finish).unwrap();
        assert_eq!(st, "200 OK");
        assert_eq!(devices().iter().find(|d| d.public == public).unwrap().name, "test phone");
        // The code is spent.
        let again = Spake::start(Role::Client, &code, [6; 32]);
        assert_eq!(
            gate.handle("POST", "/pair/start", "", &format!("x={}", hex(&again.message()))).unwrap().0,
            "403 Forbidden"
        );

        // Signing in: the server proves its key, the client its own; a ticket opens the stream.
        let cn = [5u8; 32];
        let (_, ch) = gate.handle("GET", "/auth/challenge", &format!("cn={}", hex(&cn)), "").unwrap();
        let nonce = unhex(&json(&ch, "nonce")).unwrap();
        assert!(verify(&server, &server_auth_message(&cn, &nonce), &unhex(&json(&ch, "sig")).unwrap()));
        let sig: p256::ecdsa::Signature = client.sign(&client_auth_message(&nonce, &server));
        let body = format!("nonce={}&key={}&sig={}", hex(&nonce), hex(&public), hex(&sig.to_bytes()));
        let (st, t) = gate.handle("POST", "/auth", "", &body).unwrap();
        assert_eq!(st, "200 OK");
        assert!(gate.allows(&format!("ticket={}", json(&t, "ticket"))));
        let p = gate.admit(&format!("ticket={}", json(&t, "ticket")), None).unwrap();
        assert_eq!((p.user.as_str(), p.via), (format!("test phone ({})", fingerprint(&public)).as_str(), "paired"));
        assert!(!gate.allows("ticket=abc"));
        // The same challenge doesn't answer twice; an unknown key isn't let in.
        assert_eq!(gate.handle("POST", "/auth", "", &body).unwrap().0, "403 Forbidden");
        let (_, ch) = gate.handle("GET", "/auth/challenge", &format!("cn={}", hex(&cn)), "").unwrap();
        let nonce = unhex(&json(&ch, "nonce")).unwrap();
        let stranger = p256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
        let sp = stranger.verifying_key().to_encoded_point(false).as_bytes().to_vec();
        let sig: p256::ecdsa::Signature = stranger.sign(&client_auth_message(&nonce, &server));
        assert_eq!(
            gate.handle(
                "POST",
                "/auth",
                "",
                &format!("nonce={}&key={}&sig={}", hex(&nonce), hex(&sp), hex(&sig.to_bytes()))
            )
            .unwrap()
            .0,
            "403 Forbidden"
        );
        assert!(unpair(&fingerprint(&public)).unwrap());
        std::env::remove_var("REIN_REMOTE_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn five_wrong_tries_spend_the_code() {
        let _env = crate::identity::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rr-pair5-{}", std::process::id()));
        std::env::set_var("REIN_REMOTE_HOME", &dir);
        let gate = Gate::new(None, true);
        gate.new_code().unwrap();
        for i in 0..5 {
            let s = Spake::start(Role::Client, "999-999", [i + 1; 32]);
            assert_eq!(gate.handle("POST", "/pair/start", "", &format!("x={}", hex(&s.message()))).unwrap().0, "200 OK");
        }
        let s = Spake::start(Role::Client, "999-999", [9; 32]);
        assert_eq!(
            gate.handle("POST", "/pair/start", "", &format!("x={}", hex(&s.message()))).unwrap().0,
            "403 Forbidden"
        );
        std::env::remove_var("REIN_REMOTE_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_ticket_for_one_session_lets_one_in() {
        let gate = Gate::new(Some("tok".into()), false);
        let t = gate.ticket_once(Principal::new("relay viewer", "relay"));
        assert!(!gate.allows("ticket=nope"));
        assert!(gate.allows(&format!("ticket={t}&direct=1")));
        assert!(!gate.allows(&format!("ticket={t}")), "spent");
        assert!(gate.allows("token=tok"));
    }

    #[test]
    fn forms_decode() {
        let f = form("a=1&name=my+phone%21&empty=&x");
        assert_eq!(f["name"], "my phone!");
        assert_eq!(f["empty"], "");
        assert_eq!(f["x"], "");
        assert_eq!(form("k=%zz%4")["k"], "%zz%4");
    }

    fn json(body: &str, key: &str) -> String {
        let at = body.find(&format!("\"{key}\":\"")).unwrap() + key.len() + 4;
        body[at..].split('"').next().unwrap().to_string()
    }

    /// What the web client's pair.js must reproduce (clients/web/test/pair-vectors.json), and a
    /// signature it made that this side must accept (clients/web/test/js-signature.json).
    #[test]
    fn the_web_clients_pairing_vectors_are_current_and_its_signatures_verify() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../clients/web/test");
        let (cr, sr) = ([0x11u8; 32], [0x22u8; 32]);
        let code = "314-159";
        let a = Spake::start(Role::Client, code, cr);
        let b = Spake::start(Role::Server, code, sr);
        let ka = a.finish(&b.message()).unwrap();
        let key = p256::ecdsa::SigningKey::from_slice(&[0x33; 32]).unwrap();
        let public = key.verifying_key().to_encoded_point(false).as_bytes().to_vec();
        let msg = client_auth_message(&[0x44; 32], &public);
        let sig: p256::ecdsa::Signature = key.sign(&msg);
        let want = format!(
            "{{\"code\":\"{code}\",\"client_random\":\"{}\",\"x\":\"{}\",\"y\":\"{}\",\"shared\":\"{}\",\"confirm_a\":\"{}\",\"confirm_b\":\"{}\",\"key_mac\":\"{}\",\"public\":\"{}\",\"msg\":\"{}\",\"sig\":\"{}\",\"fingerprint\":\"{}\"}}\n",
            hex(&cr),
            hex(&a.message()),
            hex(&b.message()),
            hex(&ka.shared),
            hex(&ka.confirm_mine),
            hex(&ka.confirm_theirs),
            hex(&key_mac(&ka.shared, "client", &public)),
            hex(&public),
            hex(&msg),
            hex(&sig.to_bytes()),
            fingerprint(&public)
        );
        let path = format!("{dir}/pair-vectors.json");
        if std::env::var_os("RR_WRITE_VECTORS").is_some() {
            std::fs::write(&path, &want).unwrap();
        }
        assert_eq!(
            // (A Windows checkout may have turned its line ends into CRLF.)
            std::fs::read_to_string(&path).ok().map(|s| s.replace("\r\n", "\n")).as_deref(),
            Some(want.as_str()),
            "run with RR_WRITE_VECTORS=1 to update {path}"
        );
        // pair.js's own signature (written by its test with fixed entropy).
        if let Ok(js) = std::fs::read_to_string(format!("{dir}/js-signature.json")) {
            let field = |k: &str| unhex(&json(&js, k)).unwrap();
            assert!(verify(&field("public"), &field("msg"), &field("sig")), "pair.js's signature verifies here");
            assert!(!verify(&field("public"), b"something else", &field("sig")));
        } else {
            panic!("clients/web/test/js-signature.json is missing (node --test clients/web/test writes it)");
        }
    }
}
