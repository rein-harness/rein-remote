//! OpenID Connect sign-in: the authorization-code flow with PKCE (S256), as a confidential client
//! (`client_secret_basic` or `client_secret_post`, whichever the provider says it takes) or a public
//! one (no secret). The provider is found by its issuer (`/.well-known/openid-configuration`); its
//! ID token is checked here: the signature against its published keys (RS256 or ES256), then the
//! issuer, the audience (and `azp` when there are several), the times (with a minute's leeway for
//! clocks) and the nonce this login sent.

use super::jwt;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use serde::Deserialize;
use sha2::Digest;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Clocks may differ this much between here and the provider.
const LEEWAY: i64 = 60;
/// The provider's keys are fetched again for an unknown key id at most this often.
const REFETCH: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Config {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Where the provider sends the browser back: `<public url>/sso/oidc/callback`.
    pub redirect_uri: String,
    pub scopes: String,
    /// The claim that names the user (`email` by default; then `preferred_username`, then `sub`).
    pub user_claim: String,
    /// The claim with the user's groups (`groups`), a list of strings (or one string).
    pub groups_claim: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

pub struct Oidc {
    pub cfg: Config,
    disc: Discovery,
    keys: Mutex<(Vec<jwt::Jwk>, Instant)>,
    http: ureq::Agent,
}

/// Who signed in, from the ID token.
#[derive(Debug, Clone, PartialEq)]
pub struct Claims {
    pub user: String,
    pub groups: Vec<String>,
    pub subject: String,
}

/// An HTTP client for talking to the provider: TLS checked against the system's roots (an IdP inside
/// a company may have the company's own CA there) and the usual public ones; no redirects followed.
pub fn http() -> ureq::Agent {
    use ureq::tls::{Certificate, RootCerts, TlsConfig};
    let native: Vec<Certificate<'static>> = rustls_native_certs::load_native_certs()
        .certs
        .iter()
        .map(|c| Certificate::from_der(c.as_ref()).to_owned())
        .collect();
    let roots = if native.is_empty() {
        RootCerts::WebPki
    } else {
        RootCerts::new_with_certs(&native)
    };
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .tls_config(TlsConfig::builder().root_certs(roots).build())
        .build()
        .into()
}

pub fn get_json<T: serde::de::DeserializeOwned>(http: &ureq::Agent, url: &str) -> Result<T, String> {
    let mut r = http.get(url).header("Accept", "application/json").call().map_err(|e| format!("{url}: {e}"))?;
    let status = r.status().as_u16();
    let body = r.body_mut().read_to_string().map_err(|e| format!("{url}: {e}"))?;
    if status != 200 {
        return Err(format!("{url}: HTTP {status}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url}: {e}"))
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// A random string for state, nonce or a PKCE verifier (32 bytes, base64url: 43 characters).
pub fn random_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("the system's randomness");
    B64.encode(b)
}

/// PKCE S256: the challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    B64.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn form_encode(v: &str) -> String {
    rr_client::http::encode(v)
}

impl Oidc {
    /// Finds the provider (its discovery document and keys): at start, so a wrong issuer fails then.
    pub fn discover(cfg: Config) -> Result<Self, String> {
        let http = http();
        let issuer = cfg.issuer.trim_end_matches('/');
        if !issuer.starts_with("https://") {
            if issuer.starts_with("http://") {
                eprintln!("rein-remote: the identity provider over plain HTTP ({issuer}): for a test setup only");
            } else {
                return Err(format!("--oidc-issuer {issuer}: not an https:// URL"));
            }
        }
        let disc: Discovery = get_json(&http, &format!("{issuer}/.well-known/openid-configuration"))?;
        // OpenID Connect Discovery 1.0, 4.3: the issuer it says must be the one asked for.
        if disc.issuer.trim_end_matches('/') != issuer {
            return Err(format!(
                "the identity provider at {issuer} says it's {} (give that as --oidc-issuer)",
                disc.issuer
            ));
        }
        if !disc.code_challenge_methods_supported.is_empty() && !disc.code_challenge_methods_supported.iter().any(|m| m == "S256") {
            return Err(format!("{issuer} doesn't do PKCE with S256"));
        }
        let keys: jwt::JwkSet = get_json(&http, &disc.jwks_uri)?;
        Ok(Self {
            cfg,
            disc,
            keys: Mutex::new((keys.keys, Instant::now())),
            http,
        })
    }

    pub fn issuer(&self) -> &str {
        &self.disc.issuer
    }

    /// Where to send the browser to sign in.
    pub fn login_url(&self, state: &str, nonce: &str, verifier: &str) -> String {
        let sep = if self.disc.authorization_endpoint.contains('?') { '&' } else { '?' };
        format!(
            "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={state}&nonce={nonce}&code_challenge={}&code_challenge_method=S256",
            self.disc.authorization_endpoint,
            form_encode(&self.cfg.client_id),
            form_encode(&self.cfg.redirect_uri),
            form_encode(&self.cfg.scopes),
            challenge(verifier)
        )
    }

    /// The code the provider sent back, exchanged for an ID token, checked; who it says signed in.
    pub fn finish(&self, code: &str, verifier: &str, nonce: &str) -> Result<Claims, String> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &self.cfg.redirect_uri),
            ("code_verifier", verifier),
            ("client_id", &self.cfg.client_id),
        ];
        let mut req = self.http.post(&self.disc.token_endpoint).header("Accept", "application/json");
        if let Some(secret) = &self.cfg.client_secret {
            // client_secret_basic unless the provider takes only client_secret_post.
            let methods = &self.disc.token_endpoint_auth_methods_supported;
            if methods.iter().any(|m| m == "client_secret_basic") || methods.is_empty() {
                let pair = format!("{}:{}", form_encode(&self.cfg.client_id), form_encode(secret));
                req = req.header("Authorization", &format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)));
            } else {
                form.push(("client_secret", secret));
            }
        }
        let mut r = req.send_form(form).map_err(|e| format!("the token endpoint: {e}"))?;
        let status = r.status().as_u16();
        let body = r.body_mut().read_to_string().map_err(|e| format!("the token endpoint: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| format!("the token endpoint answered HTTP {status}, not JSON"))?;
        if status != 200 {
            // The provider's own error code and description (nothing secret in them).
            return Err(format!(
                "the token endpoint refused the code: {} {}",
                v["error"].as_str().unwrap_or("?"),
                v["error_description"].as_str().unwrap_or("")
            ));
        }
        let id_token = v["id_token"]
            .as_str()
            .ok_or("the token endpoint gave no id_token (is `openid` in the scopes?)")?;
        self.check(id_token, nonce, now())
    }

    /// An ID token's signature and claims (RFC: OpenID Connect Core 1.0, 3.1.3.7).
    pub fn check(&self, id_token: &str, nonce: &str, now: i64) -> Result<Claims, String> {
        let claims = {
            let keys = self.keys.lock().unwrap().0.clone();
            match jwt::verify(id_token, &keys) {
                Err(jwt::Error::UnknownKey(_)) => {
                    // Keys rotated: fetch them again (not more than once a minute).
                    let mut k = self.keys.lock().unwrap();
                    if k.1.elapsed() > REFETCH || k.0.is_empty() {
                        let set: jwt::JwkSet = get_json(&self.http, &self.disc.jwks_uri)?;
                        *k = (set.keys, Instant::now());
                    }
                    jwt::verify(id_token, &k.0)
                }
                other => other,
            }
            .map_err(|e| e.to_string())?
        };
        check_claims(&claims, &self.disc.issuer, &self.cfg, nonce, now)
    }
}

fn check_claims(c: &serde_json::Value, issuer: &str, cfg: &Config, nonce: &str, now: i64) -> Result<Claims, String> {
    if c["iss"].as_str() != Some(issuer) {
        return Err(format!("the token is from {:?}, not {issuer}", c["iss"]));
    }
    let aud: Vec<&str> = match &c["aud"] {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => vec![],
    };
    if !aud.contains(&cfg.client_id.as_str()) {
        return Err(format!("the token is for {aud:?}, not this client ({})", cfg.client_id));
    }
    if aud.len() > 1 && c["azp"].as_str().is_some_and(|azp| azp != cfg.client_id) {
        return Err("the token was issued to another client (azp)".into());
    }
    let exp = c["exp"].as_i64().ok_or("the token has no expiry")?;
    if exp + LEEWAY < now {
        return Err("the token has expired".into());
    }
    if let Some(iat) = c["iat"].as_i64() {
        if iat > now + LEEWAY {
            return Err("the token was issued in the future (check this machine's clock)".into());
        }
    }
    if let Some(nbf) = c["nbf"].as_i64() {
        if nbf > now + LEEWAY {
            return Err("the token isn't valid yet".into());
        }
    }
    let given = c["nonce"].as_str().unwrap_or("");
    if !rr_proto::pair::confirm_eq(given.as_bytes(), nonce.as_bytes()) {
        return Err("the token's nonce isn't this sign-in's".into());
    }
    let subject = c["sub"].as_str().ok_or("the token has no subject")?.to_string();
    let user = [cfg.user_claim.as_str(), "email", "preferred_username"]
        .iter()
        .find_map(|k| c[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or(&subject)
        .to_string();
    let groups = match &c[cfg.groups_claim.as_str()] {
        serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        serde_json::Value::String(s) => vec![s.clone()],
        _ => vec![],
    };
    Ok(Claims { user, groups, subject })
}

/// A provider that runs in the test process: discovery, keys and a token endpoint on 127.0.0.1 that
/// hands out whatever ID token the test queued for the next code.
#[cfg(test)]
pub mod mock {
    use super::super::jwt::testing::Signer;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub struct Idp {
        pub issuer: String,
        /// The token endpoint's next answer (status, body), and the forms it was sent.
        pub next: Arc<Mutex<(u16, String)>>,
        pub seen: Arc<Mutex<Vec<String>>>,
    }

    pub fn start(signers: Vec<Signer>) -> Idp {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", l.local_addr().unwrap());
        let next = Arc::new(Mutex::new((500u16, String::new())));
        let seen = Arc::new(Mutex::new(vec![]));
        let (iss, n, s) = (issuer.clone(), next.clone(), seen.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let mut buf = vec![0u8; 16384];
                let mut got = 0;
                // The head and (for a POST) its body.
                loop {
                    let k = c.read(&mut buf[got..]).unwrap_or(0);
                    got += k;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if got >= end + 4 + len {
                            break;
                        }
                    }
                    if k == 0 {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..got]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = match path.as_str() {
                    "/.well-known/openid-configuration" => (
                        200,
                        serde_json::json!({
                            "issuer": iss, "authorization_endpoint": format!("{iss}/authorize"),
                            "token_endpoint": format!("{iss}/token"), "jwks_uri": format!("{iss}/jwks"),
                            "token_endpoint_auth_methods_supported": ["client_secret_basic"],
                            "code_challenge_methods_supported": ["S256"]
                        })
                        .to_string(),
                    ),
                    "/jwks" => (
                        200,
                        serde_json::json!({"keys": signers.iter().map(|s| s.jwk()).collect::<Vec<_>>()}).to_string(),
                    ),
                    "/token" => {
                        s.lock().unwrap().push(req.clone());
                        n.lock().unwrap().clone()
                    }
                    _ => (404, "{}".into()),
                };
                let _ = c.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        Idp { issuer, next, seen }
    }
}

#[cfg(test)]
mod tests {
    use super::super::jwt::testing::Signer;
    use super::*;

    fn cfg(issuer: &str) -> Config {
        Config {
            issuer: issuer.into(),
            client_id: "rein-remote".into(),
            client_secret: Some("s3cret".into()),
            redirect_uri: "https://rr.example/sso/oidc/callback".into(),
            scopes: "openid email profile".into(),
            user_claim: "email".into(),
            groups_claim: "groups".into(),
        }
    }

    #[test]
    fn the_code_flow_against_a_provider() {
        let rsa = Signer::rsa("k1");
        let idp = mock::start(vec![rsa]);
        let rsa = Signer::rsa("k1");
        let oidc = Oidc::discover(cfg(&idp.issuer)).unwrap();
        let (state, nonce, verifier) = (random_b64(16), random_b64(16), random_b64(32));
        let url = oidc.login_url(&state, &nonce, &verifier);
        assert!(url.starts_with(&format!("{}/authorize?response_type=code&client_id=rein-remote&", idp.issuer)));
        assert!(url.contains(&format!("code_challenge={}", challenge(&verifier))) && url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Frr.example%2Fsso%2Foidc%2Fcallback"));
        assert!(
            !url.contains(&verifier) && !url.contains("s3cret"),
            "neither the verifier nor the secret goes in a URL"
        );
        let t = now();
        let claims = serde_json::json!({"iss": idp.issuer, "aud": "rein-remote", "sub": "u-1", "email": "alice@corp.example",
            "groups": ["rr-admins", "staff"], "nonce": nonce, "iat": t, "exp": t + 300});
        *idp.next.lock().unwrap() = (200, serde_json::json!({"id_token": rsa.sign(&claims), "token_type": "Bearer"}).to_string());
        let who = oidc.finish("the-code", &verifier, &nonce).unwrap();
        assert_eq!(
            who,
            Claims {
                user: "alice@corp.example".into(),
                groups: vec!["rr-admins".into(), "staff".into()],
                subject: "u-1".into()
            }
        );
        // The provider got the code, the verifier and the secret (in the Authorization header).
        let sent = idp.seen.lock().unwrap().last().unwrap().clone();
        assert!(sent.contains("code=the-code") && sent.contains(&format!("code_verifier={verifier}")));
        assert!(sent.contains(&format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("rein-remote:s3cret"))));
        // The provider refusing the code.
        *idp.next.lock().unwrap() = (400, r#"{"error":"invalid_grant","error_description":"Code not valid"}"#.into());
        assert!(oidc.finish("old", &verifier, &nonce).unwrap_err().contains("invalid_grant"));
    }

    #[test]
    fn id_tokens_are_checked() {
        let (rsa, ec) = (Signer::rsa("k1"), Signer::ec("k2"));
        let idp = mock::start(vec![Signer::rsa("k1")]);
        let oidc = Oidc::discover(cfg(&idp.issuer)).unwrap();
        let t = now();
        let good = serde_json::json!({"iss": idp.issuer, "aud": ["rein-remote", "other"], "azp": "rein-remote", "sub": "u-2",
            "preferred_username": "bob", "nonce": "n1", "iat": t, "exp": t + 60});
        assert_eq!(oidc.check(&rsa.sign(&good), "n1", t).unwrap().user, "bob");
        let with = |k: &str, v: serde_json::Value| {
            let mut c = good.clone();
            c[k] = v;
            rsa.sign(&c)
        };
        assert!(oidc.check(&with("iss", "https://evil.example".into()), "n1", t).unwrap_err().contains("not"));
        assert!(oidc
            .check(&with("aud", "someone-else".into()), "n1", t)
            .unwrap_err()
            .contains("not this client"));
        assert!(oidc.check(&with("azp", "other".into()), "n1", t).unwrap_err().contains("azp"));
        assert!(oidc.check(&with("exp", (t - 120).into()), "n1", t).unwrap_err().contains("expired"));
        // Within the minute's leeway: fine.
        assert!(oidc.check(&with("exp", (t - 30).into()), "n1", t).is_ok());
        assert!(oidc.check(&with("iat", (t + 600).into()), "n1", t).unwrap_err().contains("future"));
        assert!(oidc.check(&with("nbf", (t + 600).into()), "n1", t).unwrap_err().contains("valid yet"));
        assert!(oidc.check(&rsa.sign(&good), "n2", t).unwrap_err().contains("nonce"));
        // Signed by a key the provider doesn't have (k2): it looks again, then refuses.
        assert!(oidc.check(&ec.sign(&good), "n1", t).is_err());
        // With no user claim at all, the subject names the user.
        let mut bare = good.clone();
        bare.as_object_mut().unwrap().remove("preferred_username");
        assert_eq!(oidc.check(&rsa.sign(&bare), "n1", t).unwrap().user, "u-2");
    }

    #[test]
    fn a_provider_that_names_another_issuer_is_refused() {
        let idp = mock::start(vec![Signer::ec("k")]);
        let mut c = cfg(&idp.issuer);
        c.issuer = format!("{}/", idp.issuer);
        assert!(Oidc::discover(c).is_ok(), "a trailing slash is the same issuer");
        assert!(Oidc::discover(cfg("ftp://x")).is_err());
    }
}
