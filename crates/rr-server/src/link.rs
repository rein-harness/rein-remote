//! The connection a session runs over. A WebSocket (TCP: works through any proxy, Rein's web UI
//! uses it) or WebTransport (QUIC, over UDP: each video frame on a stream of its own, so a late one
//! doesn't hold up the ones after it, and no head-of-line blocking on a lossy link; or, for a client
//! that takes them, video as datagrams with forward error correction, so a lost packet costs no
//! retransmission at all). The session loop is the same for both.

use rr_proto::Message;
use std::io::ErrorKind;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tungstenite::{Message as Ws, WebSocket};

pub enum Recv {
    /// One message's bytes (one or more protocol frames).
    Data(Vec<u8>),
    /// Nothing waiting now.
    Nothing,
    /// The client went.
    Closed,
}

pub trait Link {
    /// Queue a message's bytes without waiting (`video`: a frame, which WebTransport sends apart).
    fn send_bytes(&mut self, bytes: Vec<u8>, video: bool) -> Result<(), String>;
    /// Queue a message without waiting.
    fn send(&mut self, m: &Message) -> Result<(), String> {
        self.send_bytes(m.encode(), matches!(m, Message::Video(_)))
    }
    /// Whether what was queued has gone out (false: the link is backed up).
    fn flushed(&mut self) -> Result<bool, String>;
    /// What the client sent, without waiting.
    fn recv(&mut self) -> Result<Recv, String>;
    /// Wait up to `d` for the client to send something (returns early when it does).
    fn wait(&mut self, d: Duration);
    /// What the client sent, waiting up to `d` (the handshake).
    fn recv_wait(&mut self, d: Duration) -> Result<Recv, String> {
        self.wait(d);
        self.recv()
    }
    /// Video from now on as datagrams with forward error correction (the client said it takes
    /// them). False: this link has no datagrams, video goes as before.
    fn use_datagrams(&mut self) -> bool {
        false
    }
    /// The share of parity shards to send with each frame's data shards (0.0–1.0).
    fn set_parity(&mut self, _parity: f32) {}
    /// Queue an audio packet (a whole `Audio` frame): in order with the control messages, or as a
    /// datagram of its own once `use_audio_datagrams` said yes.
    fn send_audio(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.send_bytes(bytes, false)
    }
    /// Audio from now on as datagrams (the client said it takes them). False: no datagrams here.
    fn use_audio_datagrams(&mut self) -> bool {
        false
    }
}

// ---- WebSocket ----

/// A socket a WebSocket runs on: plain TCP, or TLS over TCP (a relay at wss://). Waiting looks at
/// the TCP socket underneath.
pub trait Sock: Read + Write {
    fn tcp(&self) -> &TcpStream;
}

impl Sock for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl Sock for tungstenite::stream::MaybeTlsStream<TcpStream> {
    fn tcp(&self) -> &TcpStream {
        match self {
            tungstenite::stream::MaybeTlsStream::Rustls(t) => t.get_ref(),
            tungstenite::stream::MaybeTlsStream::Plain(s) => s,
            _ => unreachable!("only plain and rustls streams are made"),
        }
    }
}

pub struct WsLink<S: Sock>(pub WebSocket<S>);

impl<S: Sock> Link for WsLink<S> {
    fn send_bytes(&mut self, bytes: Vec<u8>, _video: bool) -> Result<(), String> {
        match self.0.write(Ws::Binary(bytes.into())) {
            Ok(()) => Ok(()),
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(()), // buffered
            Err(e) => Err(e.to_string()),
        }
    }

    fn flushed(&mut self) -> Result<bool, String> {
        match self.0.flush() {
            Ok(()) => Ok(true),
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    fn recv(&mut self) -> Result<Recv, String> {
        let _ = self.0.get_ref().tcp().set_nonblocking(true);
        loop {
            return match self.0.read() {
                Ok(Ws::Binary(b)) => Ok(Recv::Data(b.to_vec())),
                Ok(Ws::Close(_)) => Ok(Recv::Closed),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(Recv::Nothing),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => Ok(Recv::Closed),
                Err(e) => Err(e.to_string()),
            };
        }
    }

    fn wait(&mut self, d: Duration) {
        if d.is_zero() {
            return;
        }
        // Blocks until a byte comes or the time's up, without taking it.
        let sock = self.0.get_ref().tcp();
        let _ = sock.set_nonblocking(false);
        let _ = sock.set_read_timeout(Some(d));
        let _ = sock.peek(&mut [0u8; 1]);
        let _ = sock.set_nonblocking(true);
    }
}

// ---- WebTransport ----

/// What the session hands the connection's task: control messages go on the control stream, each
/// video frame on a new stream of its own.
pub enum Out {
    Control(Vec<u8>),
    Frame(Vec<u8>),
    /// A frame as datagrams (data and parity shards).
    Datagrams(Vec<Vec<u8>>),
    /// An audio packet, a datagram of its own.
    Datagram(Vec<u8>),
}

pub struct WtLink {
    out: tokio::sync::mpsc::UnboundedSender<Out>,
    inbox: mpsc::Receiver<Vec<u8>>,
    /// Frames handed over that QUIC hasn't taken yet: the link's backlog.
    in_flight: Arc<AtomicUsize>,
    pending: Option<Vec<u8>>,
    closed: bool,
    /// Video as datagrams: on, the parity share, the next frame's number, and the most a datagram
    /// may be (QUIC's path MTU, set by the connection's task).
    datagrams: bool,
    parity: f32,
    seq: u32,
    datagram_seq: u16,
    max_datagram: Arc<AtomicUsize>,
    audio_datagrams: bool,
}

/// Frames that may be on their way at once before the link counts as backed up.
const MAX_IN_FLIGHT: usize = 3;

impl Link for WtLink {
    fn send_bytes(&mut self, bytes: Vec<u8>, video: bool) -> Result<(), String> {
        let out = if video && self.datagrams {
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            self.seq = self.seq.wrapping_add(1);
            let max = self.max_datagram.load(Ordering::Relaxed).clamp(200, 1400);
            Out::Datagrams(rr_proto::fec::split(self.seq, &bytes, max, self.parity, &mut self.datagram_seq))
        } else if video {
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            Out::Frame(bytes)
        } else {
            Out::Control(bytes)
        };
        self.out.send(out).map_err(|_| "the WebTransport session ended".to_string())
    }

    fn flushed(&mut self) -> Result<bool, String> {
        Ok(self.in_flight.load(Ordering::SeqCst) < MAX_IN_FLIGHT)
    }

    fn recv(&mut self) -> Result<Recv, String> {
        if let Some(b) = self.pending.take() {
            return Ok(Recv::Data(b));
        }
        match self.inbox.try_recv() {
            Ok(b) => Ok(Recv::Data(b)),
            Err(mpsc::TryRecvError::Empty) if !self.closed => Ok(Recv::Nothing),
            _ => Ok(Recv::Closed),
        }
    }

    fn wait(&mut self, d: Duration) {
        if self.pending.is_some() || d.is_zero() {
            return;
        }
        match self.inbox.recv_timeout(d) {
            Ok(b) => self.pending = Some(b),
            Err(mpsc::RecvTimeoutError::Disconnected) => self.closed = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    fn use_datagrams(&mut self) -> bool {
        // Zero: the connection has no datagrams (the peer didn't offer them).
        self.datagrams = self.max_datagram.load(Ordering::Relaxed) > 0;
        self.datagrams
    }

    fn set_parity(&mut self, parity: f32) {
        self.parity = parity.clamp(0.0, 1.0);
    }

    fn send_audio(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let fits = bytes.len() <= self.max_datagram.load(Ordering::Relaxed);
        let out = if self.audio_datagrams && fits {
            Out::Datagram(bytes)
        } else {
            Out::Control(bytes)
        };
        self.out.send(out).map_err(|_| "the WebTransport session ended".to_string())
    }

    fn use_audio_datagrams(&mut self) -> bool {
        self.audio_datagrams = self.max_datagram.load(Ordering::Relaxed) > 0;
        self.audio_datagrams
    }
}

/// A WebTransport endpoint on `addr` (UDP), with a certificate made for it (valid 14 days, as browsers
/// require for a certificate they're given by hash). Each session runs `session` on a thread of its
/// own; returns the certificate's SHA-256 for the page.
/// With `tls` (a real certificate), the browser checks it as for any site and there's no hash.
pub fn webtransport<F>(
    addr: std::net::SocketAddr,
    gate: Arc<crate::pairing::Gate>,
    tls: Option<(std::path::PathBuf, std::path::PathBuf)>,
    session: F,
) -> Result<Option<[u8; 32]>, String>
where
    F: Fn(Box<dyn Link + Send>) + Send + Sync + 'static,
{
    use wtransport::{Endpoint, Identity, ServerConfig};
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    if let Ok(h) = std::env::var("HOSTNAME") {
        names.push(h);
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let (identity, hash) = match tls {
        Some((cert, key)) => (
            rt.block_on(Identity::load_pemfiles(cert, key))
                .map_err(|e| format!("the certificate for WebTransport: {e}"))?,
            None,
        ),
        None => {
            let id = Identity::self_signed(names).map_err(|e| format!("a certificate for WebTransport: {e}"))?;
            let h = *id.certificate_chain().as_slice()[0].hash().as_ref();
            (id, Some(h))
        }
    };
    let config = ServerConfig::builder()
        .with_bind_address(addr)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .build();
    let endpoint = rt
        .block_on(async { Endpoint::server(config) })
        .map_err(|e| format!("WebTransport on {addr}: {e}"))?;
    let session = Arc::new(session);
    std::thread::spawn(move || {
        rt.block_on(async move {
            loop {
                let incoming = endpoint.accept().await;
                let (session, gate) = (session.clone(), gate.clone());
                tokio::spawn(async move {
                    let Ok(request) = incoming.await else { return };
                    // The same gate as the WebSocket's: the token, or a paired device's ticket.
                    let query = request.path().split_once('?').map_or("", |(_, q)| q).to_string();
                    if !gate.allows(&query) {
                        request.forbidden().await;
                        return;
                    }
                    let Ok(conn) = request.accept().await else { return };
                    // The client opens the control stream first.
                    let Ok((mut ctl_tx, mut ctl_rx)) = conn.accept_bi().await else { return };
                    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Out>();
                    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>();
                    let in_flight = Arc::new(AtomicUsize::new(0));
                    let max_datagram = Arc::new(AtomicUsize::new(conn.max_datagram_size().unwrap_or(0)));
                    let link = WtLink {
                        out: out_tx,
                        inbox: in_rx,
                        in_flight: in_flight.clone(),
                        pending: None,
                        closed: false,
                        datagrams: false,
                        parity: 0.1,
                        seq: 0,
                        datagram_seq: 0,
                        max_datagram: max_datagram.clone(),
                        audio_datagrams: false,
                    };
                    std::thread::spawn(move || session(Box::new(link)));
                    // From the client: whole protocol frames, from however the stream cut them.
                    let reader = tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = vec![0u8; 64 * 1024];
                        while let Ok(Some(n)) = ctl_rx.read(&mut chunk).await {
                            buf.extend_from_slice(&chunk[..n]);
                            let mut at = 0;
                            while let Some(len) = whole_frame(&buf[at..]) {
                                if in_tx.send(buf[at..at + len].to_vec()).is_err() {
                                    return;
                                }
                                at += len;
                            }
                            buf.drain(..at);
                        }
                    });
                    // To the client.
                    while let Some(out) = out_rx.recv().await {
                        match out {
                            Out::Control(b) => {
                                if ctl_tx.write_all(&b).await.is_err() {
                                    break;
                                }
                            }
                            Out::Datagrams(list) => {
                                // Each at once: QUIC drops what it can't send (a late datagram is no use).
                                for d in list {
                                    let _ = conn.send_datagram(d);
                                }
                                if let Some(m) = conn.max_datagram_size() {
                                    max_datagram.store(m, Ordering::Relaxed);
                                }
                                in_flight.fetch_sub(1, Ordering::SeqCst);
                            }
                            Out::Datagram(b) => {
                                let _ = conn.send_datagram(b);
                            }
                            Out::Frame(b) => {
                                let (conn, in_flight) = (conn.clone(), in_flight.clone());
                                tokio::spawn(async move {
                                    if let Ok(opening) = conn.open_uni().await {
                                        if let Ok(mut s) = opening.await {
                                            let _ = s.write_all(&b).await;
                                            let _ = s.finish().await;
                                        }
                                    }
                                    in_flight.fetch_sub(1, Ordering::SeqCst);
                                });
                            }
                        }
                    }
                    reader.abort();
                });
            }
        });
    });
    Ok(hash)
}

/// The length of the first whole protocol frame in `b`, if it's all there (header: kind, version,
/// flags, then the body's length as u32 little-endian).
fn whole_frame(b: &[u8]) -> Option<usize> {
    if b.len() < 7 {
        return None;
    }
    let len = 7 + u32::from_le_bytes([b[3], b[4], b[5], b[6]]) as usize;
    (b.len() >= len).then_some(len)
}

// ---- end to end ----

/// Another link with every message sealed (e2e.rs): what's between the two ends (a relay) passes
/// messages it can't read or change.
pub struct E2eLink {
    inner: Box<dyn Link + Send>,
    e2e: crate::e2e::E2e,
}

impl E2eLink {
    /// The machine's end: sends the session's salt first, in the clear.
    pub fn host(mut inner: Box<dyn Link + Send>, key: &[u8; 32]) -> Result<Self, String> {
        let (e2e, salt) = crate::e2e::E2e::host(key);
        inner.send_bytes([crate::e2e::SALT_PREFIX, &salt].concat(), false)?;
        Ok(Self { inner, e2e })
    }
}

impl Link for E2eLink {
    fn send_bytes(&mut self, bytes: Vec<u8>, video: bool) -> Result<(), String> {
        let sealed = self.e2e.seal(&bytes);
        self.inner.send_bytes(sealed, video)
    }

    fn flushed(&mut self) -> Result<bool, String> {
        self.inner.flushed()
    }

    fn recv(&mut self) -> Result<Recv, String> {
        match self.inner.recv()? {
            Recv::Data(b) => Ok(Recv::Data(self.e2e.open(&b)?)),
            other => Ok(other),
        }
    }

    fn wait(&mut self, d: Duration) {
        self.inner.wait(d);
    }

    fn send_audio(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let sealed = self.e2e.seal(&bytes);
        self.inner.send_audio(sealed)
    }
}
