//! Pairing and, after it, knowing each other. No I/O here: the messages and the math, shared by the
//! server, native clients, and (as a JavaScript port checked against vectors this writes) the web
//! client.
//!
//! **Pairing** turns a short one-time code the server shows into keys both sides trust. It's SPAKE2
//! (the construction of RFC 9382) over P-256: each side sends one point; someone in the middle who
//! doesn't know the code learns nothing they can test offline, and gets one guess per attempt (the
//! server allows a few, then the code is spent). Both sides then prove they derived the same key
//! (confirmations), and each sends its long-term public key MAC'd with it: those keys are what's
//! kept.
//!
//! **Knowing each other later**: the client signs the server's fresh challenge with its key, and the
//! server signs the client's (ECDSA P-256 with SHA-256, the kind every browser's WebCrypto has);
//! each checks the other's against the key it kept. Nothing to type, no token in a link.

use hmac::{Hmac, Mac};
use p256::elliptic_curve::ops::Reduce;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::elliptic_curve::PrimeField;
use p256::{AffinePoint, EncodedPoint, FieldBytes, ProjectivePoint, Scalar, U256};
use sha2::{Digest, Sha256};

/// RFC 9382's M and N for P-256 (points with no known relation to the generator).
const M: &str = "02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f";
const N: &str = "03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49";

/// The identities in the transcript (fixed: the keys that matter are exchanged after).
pub const ID_CLIENT: &[u8] = b"rein-remote client";
pub const ID_SERVER: &[u8] = b"rein-remote server";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The side that enters the code (A in RFC 9382).
    Client,
    /// The side that shows it (B).
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    /// The peer's point isn't a point on the curve (or is the identity).
    BadPoint,
    /// The confirmation didn't match: the code was wrong (or someone was in the middle).
    WrongCode,
}

fn hex_point(h: &str) -> ProjectivePoint {
    let b: Vec<u8> = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect();
    let ep = EncodedPoint::from_bytes(&b).expect("a SEC1 point");
    ProjectivePoint::from(Option::<AffinePoint>::from(AffinePoint::from_encoded_point(&ep)).expect("on the curve"))
}

fn scalar(bytes: &[u8; 32]) -> Scalar {
    <Scalar as Reduce<U256>>::reduce_bytes(FieldBytes::from_slice(bytes))
}

/// The code as a scalar: `w` in RFC 9382. (A one-time code that allows a few tries is guessed
/// online only, so a plain hash does; no slow password hashing.)
pub fn code_scalar(code: &str) -> Scalar {
    let digits: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let h = Sha256::new().chain_update(b"rein-remote pair v1\0").chain_update(digits.as_bytes()).finalize();
    scalar(&h.into())
}

/// One side of a pairing, from its first message to its keys.
pub struct Spake {
    role: Role,
    x: Scalar,
    w: Scalar,
    mine: [u8; 65],
}

/// What a finished exchange gives: the shared key, this side's confirmation to send, the one to
/// expect from the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    /// Ke: the key both sides now share (16 bytes), for MACs over what's sent next.
    pub shared: [u8; 16],
    pub confirm_mine: [u8; 32],
    pub confirm_theirs: [u8; 32],
}

impl Spake {
    /// Starts with 32 random bytes (the secret scalar) and the code.
    pub fn start(role: Role, code: &str, random: [u8; 32]) -> Self {
        let (x, w) = (scalar(&random), code_scalar(code));
        let blind = if role == Role::Client { hex_point(M) } else { hex_point(N) };
        let p = ProjectivePoint::GENERATOR * x + blind * w;
        let mut mine = [0u8; 65];
        mine.copy_from_slice(p.to_affine().to_encoded_point(false).as_bytes());
        Self { role, x, w, mine }
    }

    /// The point to send (SEC1 uncompressed, 65 bytes).
    pub fn message(&self) -> [u8; 65] {
        self.mine
    }

    /// With the peer's point: the keys (confirmations still to be checked, by the caller, in
    /// constant time: see `confirm_eq`).
    pub fn finish(&self, peer: &[u8]) -> Result<Keys, PairError> {
        let ep = EncodedPoint::from_bytes(peer).map_err(|_| PairError::BadPoint)?;
        let theirs = Option::<AffinePoint>::from(AffinePoint::from_encoded_point(&ep)).ok_or(PairError::BadPoint)?;
        let theirs = ProjectivePoint::from(theirs);
        if theirs == ProjectivePoint::IDENTITY {
            return Err(PairError::BadPoint);
        }
        let unblind = if self.role == Role::Client { hex_point(N) } else { hex_point(M) };
        let k = (theirs - unblind * self.w) * self.x;
        if k == ProjectivePoint::IDENTITY {
            return Err(PairError::BadPoint);
        }
        let k = k.to_affine().to_encoded_point(false);
        let theirs_bytes = theirs.to_affine().to_encoded_point(false);
        let (pa, pb) = match self.role {
            Role::Client => (&self.mine[..], theirs_bytes.as_bytes()),
            Role::Server => (theirs_bytes.as_bytes(), &self.mine[..]),
        };
        // TT: each part with its length (8 bytes, little-endian) first.
        let mut tt = Vec::new();
        for part in [ID_CLIENT, ID_SERVER, pa, pb, k.as_bytes(), &self.w.to_repr()[..]] {
            tt.extend_from_slice(&(part.len() as u64).to_le_bytes());
            tt.extend_from_slice(part);
        }
        let h = Sha256::digest(&tt);
        let (ke, ka) = (&h[..16], &h[16..]);
        let kc = hkdf(ka, b"ConfirmationKeys", 32);
        let (kca, kcb) = (&kc[..16], &kc[16..]);
        let ca = hmac(kca, &tt);
        let cb = hmac(kcb, &tt);
        let (confirm_mine, confirm_theirs) = match self.role {
            Role::Client => (ca, cb),
            Role::Server => (cb, ca),
        };
        Ok(Keys {
            shared: ke.try_into().unwrap(),
            confirm_mine,
            confirm_theirs,
        })
    }
}

pub fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    m.update(msg);
    m.finalize().into_bytes().into()
}

/// HKDF-SHA256 (no salt): `len` bytes for `info`.
pub fn hkdf(ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let prk = hmac(&[0u8; 32], ikm);
    let mut out = Vec::new();
    let mut t: Vec<u8> = Vec::new();
    let mut i = 1u8;
    while out.len() < len {
        let mut m = t.clone();
        m.extend_from_slice(info);
        m.push(i);
        t = hmac(&prk, &m).to_vec();
        out.extend_from_slice(&t);
        i += 1;
    }
    out.truncate(len);
    out
}

/// Equal, compared in full (no early exit on the first difference).
pub fn confirm_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// The MAC a side sends with its long-term public key once the codes agree: `label` is "client" or
/// "server".
pub fn key_mac(shared: &[u8; 16], label: &str, public: &[u8]) -> [u8; 32] {
    let mut m = format!("rein-remote pair key {label}\0").into_bytes();
    m.extend_from_slice(public);
    hmac(shared, &m)
}

/// What the server signs to prove it's the one paired: the client's nonce and its own.
pub fn server_auth_message(client_nonce: &[u8], server_nonce: &[u8]) -> Vec<u8> {
    [b"rein-remote server auth v1\0".as_slice(), client_nonce, server_nonce].concat()
}

/// What the client signs: the server's nonce and the server's key (so a signature meant for one
/// server isn't good at another).
pub fn client_auth_message(server_nonce: &[u8], server_public: &[u8]) -> Vec<u8> {
    [b"rein-remote client auth v1\0".as_slice(), server_nonce, server_public].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(seed: u8) -> [u8; 32] {
        let mut r = [0u8; 32];
        for (i, b) in r.iter_mut().enumerate() {
            *b = seed.wrapping_mul(37).wrapping_add((i as u8).wrapping_mul(11)) | 1;
        }
        r
    }

    #[test]
    fn the_same_code_gives_the_same_key_and_matching_confirmations() {
        let a = Spake::start(Role::Client, "123-456", rnd(1));
        let b = Spake::start(Role::Server, "123456", rnd(2));
        let ka = a.finish(&b.message()).unwrap();
        let kb = b.finish(&a.message()).unwrap();
        assert_eq!(ka.shared, kb.shared);
        assert!(confirm_eq(&ka.confirm_theirs, &kb.confirm_mine));
        assert!(confirm_eq(&kb.confirm_theirs, &ka.confirm_mine));
    }

    #[test]
    fn a_wrong_code_fails_the_confirmation() {
        let a = Spake::start(Role::Client, "123456", rnd(1));
        let b = Spake::start(Role::Server, "123457", rnd(2));
        let ka = a.finish(&b.message()).unwrap();
        let kb = b.finish(&a.message()).unwrap();
        assert_ne!(ka.shared, kb.shared);
        assert!(!confirm_eq(&ka.confirm_theirs, &kb.confirm_mine));
    }

    #[test]
    fn bad_points_are_refused() {
        let b = Spake::start(Role::Server, "1", rnd(2));
        assert_eq!(b.finish(&[4; 65]), Err(PairError::BadPoint));
        assert_eq!(b.finish(&[0]), Err(PairError::BadPoint));
        // M itself, sent by a client that knows no code: points that cancel to the identity are refused.
        let m = hex_point(M) * code_scalar("1");
        assert_eq!(b.finish(m.to_affine().to_encoded_point(false).as_bytes()), Err(PairError::BadPoint));
    }

    #[test]
    fn m_and_n_are_on_the_curve_and_differ() {
        assert_ne!(hex_point(M), hex_point(N));
        assert_ne!(hex_point(M), ProjectivePoint::GENERATOR);
    }

    #[test]
    fn hkdf_matches_rfc5869_case_3() {
        // RFC 5869 A.3: SHA-256, no salt, no info.
        let ikm = [0x0bu8; 22];
        let okm = hkdf(&ikm, b"", 42);
        let hex: String = okm.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8");
    }
}
