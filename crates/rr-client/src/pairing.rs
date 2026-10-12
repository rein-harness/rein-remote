//! Pairing with a server once (the code it shows), then signing in to it with keys: the client's side
//! of rr-proto's `pair`, over the server's `/pair/*` and `/auth` (see rr-server's pairing.rs), the
//! same as the web client's pair.js.
//!
//! What a pairing leaves is kept in the config folder, a file per server named by its fingerprint,
//! readable only by its owner: this client's private key for that server, and the server's public key
//! (pinned: signing in checks the server proves it holds it, so a machine pretending to be it can't
//! take the session).

use crate::http::{json_str, request, Origin};
use crate::keys::{fingerprint, hex, home, random, unhex, verify, write_private, OsRng};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use rr_proto::pair::{client_auth_message, confirm_eq, key_mac, server_auth_message, Role, Spake};
use std::path::PathBuf;

/// What's kept for a server this client paired with.
pub struct Paired {
    key: SigningKey,
    /// The server's public key (SEC1 uncompressed).
    pub server: Vec<u8>,
}

impl Paired {
    pub fn public(&self) -> Vec<u8> {
        self.key.verifying_key().to_encoded_point(false).as_bytes().to_vec()
    }
}

/// The file for the server whose fingerprint is `id` (none for something that isn't a fingerprint:
/// it names a file).
fn path(id: &str) -> Option<PathBuf> {
    let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then(|| home().join("servers").join(id))
}

/// This client's keys for the server `id`, if it paired with it.
pub fn paired(id: &str) -> Option<Paired> {
    let text = std::fs::read_to_string(path(id)?).ok()?;
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(' ')).and_then(unhex);
    let key = SigningKey::from_slice(&field("key")?).ok()?;
    let server = field("server")?;
    // The file is named by the key it pins: one that doesn't match isn't trusted.
    (fingerprint(&server) == id).then_some(Paired { key, server })
}

fn keep(id: &str, p: &Paired) -> Result<(), String> {
    let path = path(id).ok_or("the server's id isn't a fingerprint")?;
    let text = format!("key {}\nserver {}\n", hex(&p.key.to_bytes()), hex(&p.server));
    write_private(&path, text.as_bytes()).map_err(|e| format!("can't keep the pairing in {}: {e}", path.display()))
}

fn answer(status: u16, body: &str, what: &str) -> Result<(), String> {
    if status == 200 {
        return Ok(());
    }
    Err(json_str(body, "error").map_or_else(|| format!("{what}: HTTP {status}"), |e| format!("{what}: {e}")))
}

fn field(body: &str, key: &str) -> Result<Vec<u8>, String> {
    json_str(body, key)
        .and_then(|v| unhex(&v))
        .ok_or_else(|| format!("the server's answer has no {key}"))
}

/// Pairs with the server at `o` with the code it shows: both keys kept. Its fingerprint.
pub fn pair(o: &Origin, code: &str, name: &str) -> Result<String, String> {
    let s = Spake::start(Role::Client, code, random(32).try_into().unwrap());
    let (status, body) = request(o, "POST", "/pair/start", Some(&[("x", &hex(&s.message())), ("name", name)]))?;
    answer(status, &body, "pairing")?;
    let keys = s.finish(&field(&body, "y")?).map_err(|e| format!("pairing: {e:?}"))?;
    if !confirm_eq(&keys.confirm_theirs, &field(&body, "confirm")?) {
        return Err("the code didn't match".into());
    }
    let server = field(&body, "server_key")?;
    if !confirm_eq(&key_mac(&keys.shared, "server", &server), &field(&body, "server_key_mac")?) {
        return Err("the server's key didn't check out".into());
    }
    let id = fingerprint(&server);
    let session = json_str(&body, "session").ok_or("the server's answer has no session")?;
    let me = Paired {
        key: SigningKey::random(&mut OsRng),
        server,
    };
    let public = me.public();
    let (status, body) = request(
        o,
        "POST",
        "/pair/finish",
        Some(&[
            ("session", &session),
            ("confirm", &hex(&keys.confirm_mine)),
            ("key", &hex(&public)),
            ("mac", &hex(&key_mac(&keys.shared, "client", &public))),
            ("name", name),
        ]),
    )?;
    answer(status, &body, "pairing")?;
    keep(&id, &me)?;
    Ok(id)
}

/// Proves who this client is to the server `id` (and checks the server is the one it paired with): a
/// ticket for `/stream?ticket=…`, good for a minute.
pub fn sign_in(o: &Origin, id: &str) -> Result<String, String> {
    let p = paired(id).ok_or_else(|| format!("not paired with {id} (pair once with --pair CODE: the code the server shows)"))?;
    let cn = random(32);
    let (status, body) = request(o, "GET", &format!("/auth/challenge?cn={}", hex(&cn)), None)?;
    answer(status, &body, "signing in")?;
    let nonce = field(&body, "nonce")?;
    if field(&body, "server_key")? != p.server || !verify(&p.server, &server_auth_message(&cn, &nonce), &field(&body, "sig")?) {
        return Err("this isn't the machine you paired with (its key is different)".into());
    }
    let sig: Signature = p.key.sign(&client_auth_message(&nonce, &p.server));
    let (status, body) = request(
        o,
        "POST",
        "/auth",
        Some(&[("nonce", &hex(&nonce)), ("key", &hex(&p.public())), ("sig", &hex(&sig.to_bytes()))]),
    )?;
    answer(status, &body, "signing in")?;
    json_str(&body, "ticket").ok_or_else(|| "the server gave no ticket".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fingerprints_name_files() {
        assert!(path("abcd-efgh-2345-6789").is_some());
        for bad in ["", "../x", "a/b", "A", "x".repeat(65).as_str()] {
            assert!(path(bad).is_none(), "{bad}");
        }
    }
}
