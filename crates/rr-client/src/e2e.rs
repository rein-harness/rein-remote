//! End-to-end encryption through a relay: the relay passes sealed messages it can't read. The key
//! is made by the machine and travels in the viewer's link after `#` (browsers never send that part
//! of an address to a server, so the relay never sees it). Each session derives its own keys (HKDF
//! with a random salt the machine sends first), one a direction, and every message is sealed with
//! AES-256-GCM under a counter nonce that never repeats within a key; a replayed or altered message
//! is refused.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// The first message of a sealed session: this, then the salt (16 bytes), in the clear.
pub const SALT_PREFIX: &[u8] = b"RRE2E1";

pub struct E2e {
    send: Aes256Gcm,
    recv: Aes256Gcm,
    sent: u64,
    received: Option<u64>,
}

fn key(k: &[u8; 32], salt: &[u8; 16], info: &[u8]) -> Aes256Gcm {
    let mut okm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), k).expand(info, &mut okm).expect("32 bytes is a valid length");
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&okm))
}

fn nonce(n: u64) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_be_bytes());
    b
}

impl E2e {
    /// The machine's end: its keys for a session, and the salt to send first.
    pub fn host(k: &[u8; 32]) -> (Self, [u8; 16]) {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).expect("the system's randomness");
        (
            Self::with(k, &salt, b"rein-remote e2e: machine to viewer", b"rein-remote e2e: viewer to machine"),
            salt,
        )
    }

    /// The viewer's end, with the salt the machine sent first (native clients; the browser does the
    /// same in rein-remote.js).
    pub fn viewer(k: &[u8; 32], salt: &[u8; 16]) -> Self {
        Self::with(k, salt, b"rein-remote e2e: viewer to machine", b"rein-remote e2e: machine to viewer")
    }

    fn with(k: &[u8; 32], salt: &[u8; 16], send: &[u8], recv: &[u8]) -> Self {
        Self {
            send: key(k, salt, send),
            recv: key(k, salt, recv),
            sent: 0,
            received: None,
        }
    }

    /// The counter (8 bytes) and the sealed message.
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        self.sent += 1;
        let ct = self.send.encrypt(Nonce::from_slice(&nonce(self.sent)), plain).expect("sealing doesn't fail");
        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(&self.sent.to_be_bytes());
        out.extend_from_slice(&ct);
        out
    }

    /// A sealed message opened, if it's genuine and newer than the last one.
    pub fn open(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 8 + 16 {
            return Err("a sealed message that's too short".into());
        }
        let n = u64::from_be_bytes(data[..8].try_into().unwrap());
        if self.received.is_some_and(|last| n <= last) {
            return Err("a sealed message seen before (a replay)".into());
        }
        let plain = self
            .recv
            .decrypt(Nonce::from_slice(&nonce(n)), &data[8..])
            .map_err(|_| "a sealed message that doesn't open (the wrong key, or altered on the way)".to_string())?;
        self.received = Some(n);
        Ok(plain)
    }
}

/// Base64url without padding (the key in the viewer's link).
pub fn b64url(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in b.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(A[(v >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The key from a viewer's link (`#k=…`, base64url without padding): 32 bytes, or none.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.chunks(4) {
        let mut v = 0u32;
        for (i, &x) in c.iter().enumerate() {
            v |= val(x)? << (18 - 6 * i);
        }
        if c.len() == 1 {
            return None;
        }
        out.extend_from_slice(&v.to_be_bytes()[1..c.len()]);
    }
    Some(out)
}

/// The salt, if `first` is a sealed session's first message (the prefix, then 16 bytes).
pub fn salt_of(first: &[u8]) -> Option<[u8; 16]> {
    first.strip_prefix(SALT_PREFIX)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens_both_ways_and_refuses_replays_and_tampering() {
        let k = [7u8; 32];
        let (mut host, salt) = E2e::host(&k);
        let mut viewer = E2e::viewer(&k, &salt);
        let a = host.seal(b"hello viewer");
        assert_eq!(viewer.open(&a).unwrap(), b"hello viewer");
        assert!(viewer.open(&a).is_err(), "a replay is refused");
        let b = viewer.seal(b"hello machine");
        assert_eq!(host.open(&b).unwrap(), b"hello machine");
        let mut c = host.seal(b"frame");
        c[12] ^= 1;
        assert!(viewer.open(&c).is_err(), "an altered message is refused");
        // Another key (the relay guessing): nothing opens.
        let mut stranger = E2e::viewer(&[8u8; 32], &salt);
        assert!(stranger.open(&host.seal(b"secret")).is_err());
    }

    #[test]
    fn base64url_without_padding() {
        assert_eq!(b64url(b"hello"), "aGVsbG8");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        for n in 0..40u8 {
            let b: Vec<u8> = (0..n).map(|i| i.wrapping_mul(97)).collect();
            assert_eq!(b64url_decode(&b64url(&b)).unwrap(), b);
        }
        assert_eq!(b64url_decode("a"), None);
        assert_eq!(b64url_decode("a*bc"), None);
        let (_, salt) = E2e::host(&[1; 32]);
        assert_eq!(salt_of(&[SALT_PREFIX, &salt].concat()), Some(salt));
        assert_eq!(salt_of(b"RRE2E1short"), None);
    }
}
