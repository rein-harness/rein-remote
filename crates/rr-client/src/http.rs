//! The few plain requests a client makes before it streams (`/info`, pairing, signing in): HTTP/1.1
//! over TCP, or over TLS checked against the system's roots (a server with a real certificate, such
//! as Tailscale's). Small on purpose: one request a connection, the server says how long the body is.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// A server's address as a link gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Origin {
    /// `host:port`, IPv6 in brackets, as it goes in a URL.
    pub fn authority(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The host without brackets (to connect to).
    pub fn bare_host(&self) -> &str {
        self.host.trim_start_matches('[').trim_end_matches(']')
    }
}

/// A link split up: its scheme's security, where it points, its path, query and fragment (the part
/// after `#`, which stays here: it never goes to a server).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub origin: Origin,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

pub fn parse(url: &str) -> Result<Url, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url}: not a link (http://, https://, ws:// or wss://)"))?;
    let tls = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => false,
        "https" | "wss" => true,
        s => return Err(format!("{url}: a {s}:// link (http, https, ws or wss)")),
    };
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, p) = v6.split_once(']').ok_or("an IPv6 address without its ]")?;
        (format!("[{h}]"), p.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => p.parse().map_err(|_| format!("{url}: the port"))?,
        None if tls => 443,
        None => 80,
    };
    if host.is_empty() {
        return Err(format!("{url}: no host"));
    }
    Ok(Url {
        origin: Origin { tls, host, port },
        path: path.to_string(),
        query: query.to_string(),
        fragment: fragment.to_string(),
    })
}

/// A query string's or fragment's value for `key` (as it is: the values here are codes and hex).
pub fn param<'a>(q: &'a str, key: &str) -> Option<&'a str> {
    q.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// Percent-encodes a form value.
pub fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

/// The roots a real certificate is checked against: the system's. On iOS, where an app can't read
/// the system's (no file holds them), and on Android, where they aren't where rustls-native-certs
/// looks, Mozilla's (webpki-roots), which the system's match.
pub fn root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(c);
    }
    #[cfg(any(target_os = "ios", target_os = "android"))]
    if roots.is_empty() {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    roots
}

/// TLS checked against the system's roots.
pub fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots = root_store();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("TLS 1.2 and 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(cfg)
}

fn open(o: &Origin) -> Result<Box<dyn Stream>, String> {
    use std::net::ToSocketAddrs;
    let addr = (o.bare_host(), o.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", o.host))?
        .next()
        .ok_or_else(|| format!("{}: no address", o.host))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("{}: {e}", o.authority()))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
    if !o.tls {
        return Ok(Box::new(tcp));
    }
    let name = rustls::pki_types::ServerName::try_from(o.bare_host().to_string()).map_err(|e| format!("{}: {e}", o.host))?;
    let conn = rustls::ClientConnection::new(tls_config(), name).map_err(|e| e.to_string())?;
    Ok(Box::new(rustls::StreamOwned::new(conn, tcp)))
}

/// One request; the status code and the body.
pub fn request(o: &Origin, method: &str, path: &str, form: Option<&[(&str, &str)]>) -> Result<(u16, String), String> {
    let body = form
        .map(|f| f.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&"))
        .unwrap_or_default();
    let mut s = open(o)?;
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n",
        o.authority(),
        body.len()
    );
    s.write_all([head.as_bytes(), body.as_bytes()].concat().as_slice()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        // A body that's all here (by its Content-Length) is the end, whether or not the TLS closed cleanly.
        if let Some((status, len, at)) = head_of(&buf) {
            if buf.len() >= at + len {
                return Ok((status, String::from_utf8_lossy(&buf[at..at + len]).into_owned()));
            }
        }
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(format!("{}{path}: {e}", o.authority())),
        }
    }
    let (status, _, at) = head_of(&buf).ok_or_else(|| format!("{}{path}: no answer", o.authority()))?;
    Ok((status, String::from_utf8_lossy(&buf[at..]).into_owned()))
}

/// The status, the Content-Length (all that came, without one) and where the body starts.
fn head_of(buf: &[u8]) -> Option<(u16, usize, usize)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&buf[..end]);
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let len = head
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
        })
        .unwrap_or(usize::MAX - end);
    Some((status, len, end))
}

/// A string field of a flat JSON object (the server's answers are small and flat).
pub fn json_str(body: &str, key: &str) -> Option<String> {
    let at = body.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let mut out = String::new();
    let mut chars = body[at..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

/// The server's `/info`: where WebTransport is and its certificate's hash, and its fingerprint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub webtransport: Option<(u16, Option<[u8; 32]>)>,
    pub id: Option<String>,
}

pub fn info(o: &Origin) -> Result<Info, String> {
    let (status, body) = request(o, "GET", "/info", None)?;
    if status != 200 {
        return Ok(Info::default());
    }
    Ok(parse_info(&body))
}

fn parse_info(body: &str) -> Info {
    // {"webtransport":{"port":7979,"hash":[1, 2, …]},"id":"abcd-…"} (or null for either).
    let webtransport = body.find("\"webtransport\":{").and_then(|at| {
        let rest = &body[at..];
        let port = rest.split("\"port\":").nth(1)?.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
        let hash = rest.split("\"hash\":[").nth(1).and_then(|h| {
            let list: Vec<u8> = h.split(']').next()?.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            list.try_into().ok()
        });
        Some((port, hash))
    });
    Info {
        webtransport,
        id: json_str(body, "id"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_split_up() {
        let u = parse("https://box.tail1.ts.net:7979/?token=abc#k=xyz").unwrap();
        assert_eq!(
            (
                u.origin.tls,
                u.origin.host.as_str(),
                u.origin.port,
                u.path.as_str(),
                u.query.as_str(),
                u.fragment.as_str()
            ),
            (true, "box.tail1.ts.net", 7979, "/", "token=abc", "k=xyz")
        );
        let u = parse("ws://[::1]:80/stream").unwrap();
        assert_eq!(
            (u.origin.authority(), u.origin.bare_host(), u.path.as_str()),
            ("[::1]:80".to_string(), "::1", "/stream")
        );
        assert_eq!(parse("wss://relay.example").unwrap().origin.port, 443);
        assert_eq!(parse("http://h?room=r").unwrap().query, "room=r");
        assert!(parse("ftp://x").is_err() && parse("nothing").is_err() && parse("http://:1/").is_err());
        assert_eq!(param("a=1&token=t&b", "token"), Some("t"));
        assert_eq!(encode("my phone!"), "my%20phone%21");
    }

    #[test]
    fn info_reads() {
        let i = parse_info(
            r#"{"webtransport":{"port":7979,"hash":[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32]},"id":"abcd-efgh"}"#,
        );
        assert_eq!(i.webtransport.unwrap().0, 7979);
        assert_eq!(i.webtransport.unwrap().1.unwrap()[31], 32);
        assert_eq!(i.id.as_deref(), Some("abcd-efgh"));
        let i = parse_info(r#"{"webtransport":{"port":7979,"hash":null},"id":null}"#);
        assert_eq!((i.webtransport, i.id), (Some((7979, None)), None));
        assert_eq!(parse_info(r#"{"webtransport":null}"#), Info::default());
        assert_eq!(json_str(r#"{"error":"a \"b\""}"#, "error").as_deref(), Some("a \"b\""));
        assert_eq!(head_of(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\n\r\n{}"), Some((403, 2, 45)));
    }
}
