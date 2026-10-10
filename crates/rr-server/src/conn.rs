//! A connection to the page's port: plain HTTP, or HTTPS on the same port (its first byte says which:
//! 0x16 begins a TLS handshake). HTTPS matters: a browser gives a page its video decoder (WebCodecs),
//! its crypto and WebTransport only when it's secure (HTTPS, or localhost).

use crate::link::Sock;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

pub enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Conn {
    /// HTTPS when the client starts a TLS handshake and there's a certificate, else plain.
    pub fn accept(stream: TcpStream, tls: Option<&Arc<rustls::ServerConfig>>) -> Result<Self, String> {
        let mut first = [0u8; 1];
        let n = stream.peek(&mut first).map_err(|e| e.to_string())?;
        match tls {
            Some(cfg) if n == 1 && first[0] == 0x16 => {
                let conn = rustls::ServerConnection::new(cfg.clone()).map_err(|e| e.to_string())?;
                Ok(Self::Tls(Box::new(rustls::StreamOwned::new(conn, stream))))
            }
            _ => Ok(Self::Plain(stream)),
        }
    }

    /// The request's head (up to the blank line).
    pub fn read_head(&mut self) -> Result<Vec<u8>, String> {
        let mut head = Vec::with_capacity(1024);
        let mut b = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = self.read(&mut b).map_err(|e| e.to_string())?;
            // Nothing at all: a connection a browser opened ahead and never used (not worth a word).
            if n == 0 && head.is_empty() {
                return Err(String::new());
            }
            if n == 0 || head.len() > 16 * 1024 {
                return Err("an incomplete request".into());
            }
            head.extend_from_slice(&b[..n]);
        }
        Ok(head)
    }
}

impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(b),
            Self::Tls(s) => s.read(b),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(b),
            Self::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

impl Sock for Conn {
    fn tcp(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => s.get_ref(),
        }
    }
}

/// A connection whose first bytes were already read (the request's head): read again first, so the
/// WebSocket handshake sees the whole request.
pub struct Replay {
    head: Vec<u8>,
    at: usize,
    inner: Conn,
}

impl Replay {
    pub fn new(head: Vec<u8>, inner: Conn) -> Self {
        Self { head, at: 0, inner }
    }
}

impl Read for Replay {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        if self.at < self.head.len() {
            let n = b.len().min(self.head.len() - self.at);
            b[..n].copy_from_slice(&self.head[self.at..self.at + n]);
            self.at += n;
            return Ok(n);
        }
        self.inner.read(b)
    }
}

impl Write for Replay {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.inner.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Sock for Replay {
    fn tcp(&self) -> &TcpStream {
        self.inner.tcp()
    }
}

/// A TLS configuration from PEM files (a certificate chain and its key).
pub fn tls_from_pem(cert: &std::path::Path, key: &std::path::Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(cert).map_err(|e| format!("{}: {e}", cert.display()))?,
    ))
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| format!("{}: {e}", cert.display()))?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(key).map_err(|e| format!("{}: {e}", key.display()))?,
    ))
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("no private key in {}", key.display()))?;
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("the certificate: {e}"))?;
    Ok(Arc::new(cfg))
}

/// A real certificate for this machine's tailnet name, from Tailscale (`tailscale cert`; the tailnet
/// needs HTTPS turned on). Its files, kept in the user's cache folder and renewed by Tailscale.
pub fn tailscale_cert(name: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(if cfg!(target_os = "macos") { "Library/Caches" } else { ".cache" })))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from))?
        .join("rein-remote");
    std::fs::create_dir_all(&base).ok()?;
    let (cert, key) = (base.join(format!("{name}.crt")), base.join(format!("{name}.key")));
    let ok = std::process::Command::new("tailscale")
        .args(["cert", "--cert-file"])
        .arg(&cert)
        .arg("--key-file")
        .arg(&key)
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some((cert, key))
}
