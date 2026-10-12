//! This machine's identity: a P-256 key made once and kept (ECDSA, which every browser's WebCrypto
//! has, so the web client uses the same kind). Paired clients know it by its public key; its
//! fingerprint (a short hash of that key) is what discovery shows and what a client pins.
//!
//! Kept in the config folder: `REIN_REMOTE_HOME`, else `~/.rein-remote` (`%APPDATA%\rein-remote` on
//! Windows), readable only by its owner.

use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
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

pub struct Identity {
    key: SigningKey,
}

impl Identity {
    /// This machine's key, made the first time.
    pub fn load_or_create() -> Result<Self, String> {
        let path = home().join("identity.pem");
        if let Ok(pem) = std::fs::read_to_string(&path) {
            let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| format!("{}: not a key ({e})", path.display()))?;
            return Ok(Self { key });
        }
        let key = SigningKey::random(&mut OsRng);
        let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(|e| e.to_string())?;
        write_private(&path, pem.as_bytes()).map_err(|e| format!("can't keep this machine's key in {}: {e}", path.display()))?;
        Ok(Self { key })
    }

    /// A key that isn't kept (tests).
    #[cfg(test)]
    pub fn ephemeral() -> Self {
        Self {
            key: SigningKey::random(&mut OsRng),
        }
    }

    /// The public key, SEC1 uncompressed (65 bytes: 0x04, x, y), as WebCrypto's "raw" format.
    pub fn public(&self) -> Vec<u8> {
        self.key.verifying_key().to_encoded_point(false).as_bytes().to_vec()
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public())
    }

    /// A signature over `msg`: r and s, 32 bytes each (WebCrypto's ECDSA format).
    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        let s: Signature = self.key.sign(msg);
        s.to_bytes().to_vec()
    }
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

/// Randomness from the system (getrandom), for keys.
struct OsRng;

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

/// Tests that point REIN_REMOTE_HOME somewhere hold this (the environment is the process's).
#[cfg(test)]
pub static TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies_and_keeps_its_key() {
        let _env = TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("rr-id-{}", std::process::id()));
        std::env::set_var("REIN_REMOTE_HOME", &dir);
        let a = Identity::load_or_create().unwrap();
        let b = Identity::load_or_create().unwrap();
        assert_eq!(a.public(), b.public(), "the same key the second time");
        assert_eq!(a.public().len(), 65);
        let sig = a.sign(b"hello");
        assert_eq!(sig.len(), 64);
        assert!(verify(&a.public(), b"hello", &sig));
        assert!(!verify(&a.public(), b"hellO", &sig));
        assert!(!verify(&Identity::ephemeral().public(), b"hello", &sig));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("identity.pem")).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "only its owner reads it");
        }
        std::env::remove_var("REIN_REMOTE_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn base32_and_fingerprints() {
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
        let f = fingerprint(&[4; 65]);
        assert_eq!(f.len(), 19);
        assert_eq!(f.matches('-').count(), 3);
    }
}
