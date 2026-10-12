//! Keys and where they're kept, the same on both ends: the config folder (`REIN_REMOTE_HOME`, else
//! `~/.rein-remote`, `%APPDATA%\rein-remote` on Windows), files only their owner reads, and a key's
//! fingerprint (what discovery shows and what a client pins). ECDSA P-256 with SHA-256 throughout, the
//! kind every browser's WebCrypto has, so the web client uses the same.

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// The config folder (made when first needed).
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("REIN_REMOTE_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    #[cfg(windows)]
    if let Some(a) = std::env::var_os("APPDATA") {
        return PathBuf::from(a).join("rein-remote");
    }
    let base = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join(".rein-remote")
}

/// Writes `bytes` to `path` readable by its owner only (the file is made that way, never widened).
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        use std::io::Write;
        o.open(&tmp)?.write_all(bytes)?;
    }
    std::fs::rename(&tmp, path)
}

/// Whether `sig` (r‖s) is `public`'s signature over `msg` (ECDSA P-256 with SHA-256).
pub fn verify(public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    let (Ok(k), Ok(s)) = (VerifyingKey::from_sec1_bytes(public), Signature::from_slice(sig)) else {
        return false;
    };
    k.verify(msg, &s).is_ok()
}

/// A public key's fingerprint: its SHA-256's first 10 bytes in base32, grouped (`abcd-efgh-…`, 16
/// letters and digits): short enough to read out, long enough that no one makes a key to match it.
pub fn fingerprint(public: &[u8]) -> String {
    let h = Sha256::digest(public);
    let s = base32(&h[..10]);
    s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("-")
}

/// RFC 4648 base32, lower case, no padding.
pub fn base32(b: &[u8]) -> String {
    const A: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let (mut out, mut acc, mut bits) = (String::new(), 0u32, 0);
    for &x in b {
        acc = (acc << 8) | x as u32;
        bits += 8;
        while bits >= 5 {
            out.push(A[((acc >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(A[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
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

/// `n` bytes from the system's randomness.
pub fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("the system's randomness");
    b
}

/// Randomness from the system (getrandom), for keys.
pub struct OsRng;

impl p256::elliptic_curve::rand_core::RngCore for OsRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        getrandom::fill(dest).expect("the system's randomness");
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), p256::elliptic_curve::rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl p256::elliptic_curve::rand_core::CryptoRng for OsRng {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_hex_and_fingerprints() {
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
        let f = fingerprint(&[4; 65]);
        assert_eq!(f.len(), 19);
        assert_eq!(f.matches('-').count(), 3);
        assert_eq!(unhex(&hex(&[0, 1, 0xfe])), Some(vec![0, 1, 0xfe]));
        assert_eq!(unhex("abc"), None);
    }
}
