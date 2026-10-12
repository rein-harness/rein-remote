//! A JWS (compact form) checked against an identity provider's published keys: RS256 (RSA PKCS#1
//! v1.5, SHA-256, 2048 bits or more) and ES256 (ECDSA P-256, SHA-256). Nothing else is accepted: no
//! `none`, no HMAC (an `HS256` token "signed" with a public key is the classic confusion), no other
//! curves. The claims come back as JSON for the caller to check (issuer, audience, times, nonce).

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ring::signature::{RsaPublicKeyComponents, UnparsedPublicKey, ECDSA_P256_SHA256_FIXED, RSA_PKCS1_2048_8192_SHA256};
use serde::Deserialize;

/// One key of a JWK set (RFC 7517), as much of it as matters here.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    /// `sig` or `enc`; only signing keys are used.
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Malformed(&'static str),
    Algorithm(String),
    /// No key with the token's `kid` (the IdP may have rotated its keys: fetch them again, once).
    UnknownKey(Option<String>),
    BadSignature,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Malformed(w) => write!(f, "a malformed token ({w})"),
            Error::Algorithm(a) => write!(f, "a token signed with {a:?} (only RS256 and ES256 are accepted)"),
            Error::UnknownKey(k) => write!(f, "no key {k:?} among the identity provider's"),
            Error::BadSignature => write!(f, "the token's signature doesn't verify"),
        }
    }
}

fn b64(s: &str) -> Result<Vec<u8>, Error> {
    B64.decode(s.trim_end_matches('=')).map_err(|_| Error::Malformed("base64url"))
}

/// The claims of `token` when one of `keys` signed it.
pub fn verify(token: &str, keys: &[Jwk]) -> Result<serde_json::Value, Error> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(Error::Malformed("not three parts"));
    };
    let header: Header = serde_json::from_slice(&b64(h)?).map_err(|_| Error::Malformed("header"))?;
    let (kty, alg) = match header.alg.as_str() {
        "RS256" => ("RSA", "RS256"),
        "ES256" => ("EC", "ES256"),
        other => return Err(Error::Algorithm(other.to_string())),
    };
    let sig = b64(s)?;
    let signed = &token.as_bytes()[..h.len() + 1 + p.len()];
    // The keys it may be: the one named, of the right type, for signing, not for another algorithm.
    let candidates: Vec<&Jwk> = keys
        .iter()
        .filter(|k| k.kty == kty)
        .filter(|k| k.use_.as_deref().is_none_or(|u| u == "sig"))
        .filter(|k| k.alg.as_deref().is_none_or(|a| a == alg))
        .filter(|k| header.kid.is_none() || k.kid == header.kid)
        .collect();
    if candidates.is_empty() {
        return Err(Error::UnknownKey(header.kid));
    }
    for k in candidates {
        let ok = match alg {
            "RS256" => {
                let (Some(n), Some(e)) = (k.n.as_deref(), k.e.as_deref()) else { continue };
                let (n, e) = (b64(n)?, b64(e)?);
                RsaPublicKeyComponents { n: &n, e: &e }
                    .verify(&RSA_PKCS1_2048_8192_SHA256, signed, &sig)
                    .is_ok()
            }
            _ => {
                let (Some("P-256"), Some(x), Some(y)) = (k.crv.as_deref(), k.x.as_deref(), k.y.as_deref()) else {
                    continue;
                };
                let (x, y) = (b64(x)?, b64(y)?);
                if x.len() != 32 || y.len() != 32 {
                    continue;
                }
                let point = [&[4u8][..], &x, &y].concat();
                UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point).verify(signed, &sig).is_ok()
            }
        };
        if ok {
            return serde_json::from_slice(&b64(p)?).map_err(|_| Error::Malformed("claims"));
        }
    }
    Err(Error::BadSignature)
}

/// Test keys and a signer, for this module's tests and the sign-in tests (a mock identity provider).
#[cfg(test)]
pub mod testing {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, RsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING, RSA_PKCS1_SHA256};

    /// A 2048-bit RSA key (PKCS#8, made once with `openssl genpkey`; for tests only).
    pub const RSA_PKCS8_B64: &str = include_str!("test-rsa.pk8.b64");

    pub struct Signer {
        pub kid: String,
        key: Key,
    }

    enum Key {
        Rsa(RsaKeyPair),
        Ec(EcdsaKeyPair),
    }

    impl Signer {
        pub fn rsa(kid: &str) -> Self {
            let der = base64::engine::general_purpose::STANDARD
                .decode(RSA_PKCS8_B64.split_whitespace().collect::<String>())
                .unwrap();
            Self {
                kid: kid.into(),
                key: Key::Rsa(RsaKeyPair::from_pkcs8(&der).unwrap()),
            }
        }

        pub fn ec(kid: &str) -> Self {
            let rng = SystemRandom::new();
            let pk8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            Self {
                kid: kid.into(),
                key: Key::Ec(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pk8.as_ref(), &rng).unwrap()),
            }
        }

        pub fn jwk(&self) -> serde_json::Value {
            match &self.key {
                Key::Rsa(k) => {
                    let p = k.public().into();
                    let c: ring::rsa::PublicKeyComponents<Vec<u8>> = p;
                    serde_json::json!({"kty": "RSA", "kid": self.kid, "use": "sig", "alg": "RS256", "n": B64.encode(&c.n), "e": B64.encode(&c.e)})
                }
                Key::Ec(k) => {
                    let pt = k.public_key().as_ref();
                    serde_json::json!({"kty": "EC", "kid": self.kid, "crv": "P-256", "x": B64.encode(&pt[1..33]), "y": B64.encode(&pt[33..65])})
                }
            }
        }

        pub fn sign(&self, claims: &serde_json::Value) -> String {
            let alg = match self.key {
                Key::Rsa(_) => "RS256",
                Key::Ec(_) => "ES256",
            };
            self.sign_with_header(&serde_json::json!({"alg": alg, "kid": self.kid, "typ": "JWT"}), claims)
        }

        pub fn sign_with_header(&self, header: &serde_json::Value, claims: &serde_json::Value) -> String {
            let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
            let rng = SystemRandom::new();
            let sig = match &self.key {
                Key::Rsa(k) => {
                    let mut sig = vec![0; k.public().modulus_len()];
                    k.sign(&RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut sig).unwrap();
                    sig
                }
                Key::Ec(k) => k.sign(&rng, input.as_bytes()).unwrap().as_ref().to_vec(),
            };
            format!("{input}.{}", B64.encode(sig))
        }
    }

    pub fn jwks(signers: &[&Signer]) -> Vec<Jwk> {
        signers.iter().map(|s| serde_json::from_value(s.jwk()).unwrap()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn rfc7515_es256_example_verifies() {
        // RFC 7515, appendix A.3: an ES256 JWS and its key.
        let token = "eyJhbGciOiJFUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.DtEhU3ljbEg8L38VWAfUAqOyKAM6-Xx-F4GawxaepmXFCgfTjDxw5djxLa8ISlSApmWQxfKTUJqPP3-Kg6NU1Q";
        let key: Jwk = serde_json::from_value(serde_json::json!({
            "kty": "EC", "crv": "P-256",
            "x": "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU",
            "y": "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0"
        }))
        .unwrap();
        let claims = verify(token, std::slice::from_ref(&key)).unwrap();
        assert_eq!(claims["iss"], "joe");
        // One bit off in the signed part: refused.
        let bad = token.replacen("eyJpc3Mi", "eyJpc3Ni", 1);
        assert!(verify(&bad, &[key]).is_err());
    }

    #[test]
    fn rs256_and_es256_verify_and_everything_else_is_refused() {
        let (rsa, ec) = (Signer::rsa("r1"), Signer::ec("e1"));
        let keys = jwks(&[&rsa, &ec]);
        let claims = serde_json::json!({"sub": "alice", "n": 1});
        assert_eq!(verify(&rsa.sign(&claims), &keys).unwrap()["sub"], "alice");
        assert_eq!(verify(&ec.sign(&claims), &keys).unwrap()["sub"], "alice");
        // Signed by a key the IdP doesn't publish (same kid): refused.
        let other = Signer::ec("e1");
        assert_eq!(verify(&other.sign(&claims), &keys), Err(Error::BadSignature));
        // A kid that isn't there.
        let stranger = Signer::ec("e9");
        assert_eq!(verify(&stranger.sign(&claims), &keys), Err(Error::UnknownKey(Some("e9".into()))));
        // alg none, and HS256 (a token "signed" with the public key as an HMAC secret).
        let t = rsa.sign(&claims);
        let body = t.split('.').nth(1).unwrap();
        for alg in ["none", "HS256", "RS512", "PS256"] {
            let h = B64.encode(serde_json::json!({"alg": alg, "kid": "r1"}).to_string());
            assert_eq!(verify(&format!("{h}.{body}."), &keys), Err(Error::Algorithm(alg.into())));
        }
        // An RSA key can't verify an ES256 header and the other way round (the kid says which key).
        let h = B64.encode(serde_json::json!({"alg": "ES256", "kid": "r1"}).to_string());
        let sig = t.split('.').nth(2).unwrap();
        assert_eq!(verify(&format!("{h}.{body}.{sig}"), &keys), Err(Error::UnknownKey(Some("r1".into()))));
        // Changed claims: refused.
        let forged = format!("{}.{}.{}", t.split('.').next().unwrap(), B64.encode(r#"{"sub":"mallory"}"#), sig);
        assert_eq!(verify(&forged, &keys), Err(Error::BadSignature));
        // Not a token at all.
        assert!(verify("a.b", &keys).is_err());
        assert!(verify("", &keys).is_err());
    }
}
