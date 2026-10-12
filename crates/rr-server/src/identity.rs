//! This machine's identity: a P-256 key made once and kept (ECDSA, which every browser's WebCrypto
//! has, so the web client uses the same kind). Paired clients know it by its public key; its
//! fingerprint (a short hash of that key) is what discovery shows and what a client pins.
//!
//! Kept in the config folder: `REIN_REMOTE_HOME`, else `~/.rein-remote` (`%APPDATA%\rein-remote` on
//! Windows), readable only by its owner.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};

use rr_client::keys::OsRng;
pub use rr_client::keys::{fingerprint, home, verify, write_private};

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
}
