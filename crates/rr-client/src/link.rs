//! The connections a session runs over, each with a pump that hands what arrives to the session's one
//! inbox (so the session waits in one place, for the network and the app alike):
//!
//! - **WebTransport** (QUIC over UDP, HTTP/3, the same endpoint browsers use): control and input on one
//!   stream the client opens, each video frame on a stream of its own or, when the server sends them so,
//!   as datagrams with forward error correction (put back together here with rr-proto's `fec`).
//! - **WebSocket** (TCP, ws:// or wss://): through anything that passes web traffic.
//!
//! A relay's link is a WebSocket whose messages are sealed end to end (the session seals and opens
//! them, see `Seal`).

use crate::e2e::{salt_of, E2e};
use crate::Transport;
use rr_proto::fec::Reassembler;
use std::net::{TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tungstenite::{Message as Ws, WebSocket};

/// What a link's pump hands the session.
pub(crate) enum In {
    /// One message's bytes (one or more protocol frames; sealed through a relay).
    Data(Vec<u8>),
    Closed(Option<String>),
}

/// The session's inbox: what came on link `n` (an older link's late words are told apart and
/// dropped), and what the app asked.
pub(crate) enum Ev {
    Net(u64, In),
    Cmd(crate::Cmd),
}

/// Numbers links, so a message from one that's been replaced is known as such.
static NEXT_LINK: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Link {
    pub id: u64,
    pub transport: Transport,
    out: Out,
    /// Through a relay: the key, and the session's keys once the machine's salt came.
    seal: Option<Seal>,
    /// WebTransport: the datagrams put back together (loss, frames rebuilt), and whether any came.
    pub fec: Option<Arc<Mutex<Reassembler>>>,
    pub datagrams: Option<Arc<AtomicBool>>,
    /// Keeps WebTransport's tasks running for as long as the link is kept.
    _rt: Option<Arc<tokio::runtime::Runtime>>,
    closer: Option<Box<dyn FnOnce() + Send>>,
}

enum Out {
    Ws(mpsc::Sender<Vec<u8>>),
    Wt(tokio::sync::mpsc::UnboundedSender<Vec<u8>>),
}

pub(crate) struct Seal {
    key: [u8; 32],
    e2e: Option<E2e>,
    /// What the session sent before the salt came (its Hello): sealed and sent once it has.
    waiting: Vec<Vec<u8>>,
}

impl Link {
    /// Sends one message's bytes (sealed, through a relay).
    pub fn send(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let bytes = match &mut self.seal {
            Some(Seal { e2e: Some(e), .. }) => e.seal(&bytes),
            Some(Seal { waiting, .. }) => {
                waiting.push(bytes);
                return Ok(());
            }
            None => bytes,
        };
        self.raw(bytes)
    }

    fn raw(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let ok = match &self.out {
            Out::Ws(tx) => tx.send(bytes).is_ok(),
            Out::Wt(tx) => tx.send(bytes).is_ok(),
        };
        ok.then_some(()).ok_or_else(|| "the connection closed".to_string())
    }

    /// What came, opened (through a relay); `None` for what isn't for the session (the salt).
    pub fn open(&mut self, bytes: Vec<u8>) -> Result<Option<Vec<u8>>, String> {
        let Some(seal) = &mut self.seal else { return Ok(Some(bytes)) };
        if let Some(e) = &mut seal.e2e {
            return e.open(&bytes).map(Some);
        }
        let salt = salt_of(&bytes).ok_or("the relay's machine didn't start a sealed session (an older rein-remote?)")?;
        let mut e2e = E2e::viewer(&seal.key, &salt);
        let sealed: Vec<Vec<u8>> = std::mem::take(&mut seal.waiting).iter().map(|b| e2e.seal(b)).collect();
        seal.e2e = Some(e2e);
        for b in sealed {
            self.raw(b)?;
        }
        Ok(None)
    }

    /// Sealed end to end with `key` (a relay's link).
    pub fn sealed(mut self, key: [u8; 32]) -> Self {
        self.seal = Some(Seal {
            key,
            e2e: None,
            waiting: Vec::new(),
        });
        self
    }

    /// The share of datagrams lost since asked last, in thousandths (0 without datagrams).
    pub fn loss_permille(&self) -> u16 {
        self.fec.as_ref().map_or(0, |f| f.lock().unwrap().loss_permille())
    }

    pub fn close(&mut self) {
        if let Some(c) = self.closer.take() {
            c();
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.close();
    }
}

// ---- WebSocket ----

fn tcp(s: &tungstenite::stream::MaybeTlsStream<TcpStream>) -> &TcpStream {
    match s {
        tungstenite::stream::MaybeTlsStream::Rustls(t) => t.get_ref(),
        tungstenite::stream::MaybeTlsStream::Plain(s) => s,
        _ => unreachable!("only plain and rustls streams are made"),
    }
}

/// A WebSocket to `url` (ws:// or wss://), its pump on a thread of its own: it reads with a short
/// wait, and sends what the session queued in between (a WebSocket is one stream both ways).
pub(crate) fn websocket(url: &str, inbox: mpsc::Sender<Ev>, transport: Transport) -> Result<Link, String> {
    let (mut ws, _) = tungstenite::connect(url).map_err(|e| format!("{}: {e}", redact(url)))?;
    let _ = tcp(ws.get_ref()).set_nodelay(true);
    let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || pump(&mut ws, rx, inbox, id));
    Ok(Link {
        id,
        transport,
        out: Out::Ws(tx),
        seal: None,
        fec: None,
        datagrams: None,
        _rt: None,
        closer: None,
    })
}

fn pump(ws: &mut WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>, rx: mpsc::Receiver<Vec<u8>>, inbox: mpsc::Sender<Ev>, id: u64) {
    // Input waits at most this long for a read to give way.
    let _ = tcp(ws.get_ref()).set_read_timeout(Some(Duration::from_millis(2)));
    let end = 'pump: loop {
        loop {
            match rx.try_recv() {
                Ok(b) => {
                    if let Err(e) = ws.send(Ws::Binary(b.into())) {
                        break 'pump Some(e.to_string());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // The session let the link go: close it.
                Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        match ws.read() {
            Ok(Ws::Binary(b)) => {
                if inbox.send(Ev::Net(id, In::Data(b.to_vec()))).is_err() {
                    return;
                }
            }
            Ok(Ws::Close(_)) => break None,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => break None,
            Err(e) => break Some(e.to_string()),
        }
    };
    let _ = inbox.send(Ev::Net(id, In::Closed(end)));
}

/// A link without its secrets (a token or ticket in its query), for messages.
pub fn redact(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?…"),
        None => url.to_string(),
    }
}

// ---- WebTransport ----

/// How to check the server's certificate: by its SHA-256 (a self-signed one the server gave the hash
/// of, as browsers take it), or against the system's roots (a real one).
#[derive(Clone, Copy)]
pub(crate) enum Cert {
    Hash([u8; 32]),
    Roots,
}

/// The client side of WebTransport: on `socket` when given (the one a direct path was punched from),
/// else a fresh one.
fn endpoint(
    rt: &tokio::runtime::Runtime,
    cert: Cert,
    socket: Option<UdpSocket>,
) -> Result<wtransport::Endpoint<wtransport::endpoint::endpoint_side::Client>, String> {
    use wtransport::ClientConfig;
    let bind = ClientConfig::builder();
    let roots = match socket {
        Some(s) => {
            // QUIC reads it on its own task: not blocking, no timeout left from punching.
            let _ = s.set_read_timeout(None);
            s.set_nonblocking(true).map_err(|e| e.to_string())?;
            bind.with_bind_socket(s)
        }
        None => bind.with_bind_default(),
    };
    let cfg = match cert {
        Cert::Hash(h) => roots.with_server_certificate_hashes([wtransport::tls::Sha256Digest::new(h)]),
        Cert::Roots => roots.with_native_certs(),
    }
    .keep_alive_interval(Some(Duration::from_secs(3)))
    .max_idle_timeout(Some(Duration::from_secs(15)))
    .map_err(|e| e.to_string())?
    .build();
    let _g = rt.enter();
    wtransport::Endpoint::client(cfg).map_err(|e| format!("WebTransport: {e}"))
}

/// WebTransport to the first of `urls` that answers within `timeout` (all tried at once, from one
/// socket: a direct path's candidates; or one URL).
pub(crate) fn webtransport(
    rt: Arc<tokio::runtime::Runtime>,
    urls: &[String],
    cert: Cert,
    socket: Option<UdpSocket>,
    timeout: Duration,
    inbox: mpsc::Sender<Ev>,
    transport: Transport,
) -> Result<Link, String> {
    let ep = Arc::new(endpoint(&rt, cert, socket)?);
    let conn = rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for url in urls {
            let (ep, tx, url) = (ep.clone(), tx.clone(), url.clone());
            tokio::spawn(async move {
                let r = ep.connect(&url).await.map_err(|e| format!("{}: {e}", redact(&url)));
                let _ = tx.send(r);
            });
        }
        drop(tx);
        let mut why = Vec::new();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Some(Ok(c)) => {
                        // Any other that gets through later is closed: one session.
                        tokio::spawn(async move {
                            while let Some(r) = rx.recv().await {
                                if let Ok(c) = r {
                                    c.close(0u32.into(), b"another path won");
                                }
                            }
                        });
                        return Ok(c);
                    }
                    Some(Err(e)) => why.push(e),
                    None => return Err(why.join("; ")),
                },
                _ = &mut deadline => {
                    why.push(format!("no answer in {} s", timeout.as_secs_f32()));
                    return Err(why.join("; "));
                }
            }
        }
    })?;
    let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let fec = Arc::new(Mutex::new(Reassembler::new(8)));
    let datagrams = Arc::new(AtomicBool::new(false));
    let opened = rt.block_on(async { conn.open_bi().await.map_err(|e| e.to_string())?.await.map_err(|e| e.to_string()) });
    let (mut ctl_tx, mut ctl_rx) = opened?;
    // Control out: whatever the session queued, in order.
    rt.spawn(async move {
        while let Some(b) = out_rx.recv().await {
            if ctl_tx.write_all(&b).await.is_err() {
                break;
            }
        }
    });
    // Control in: whole protocol frames, from however the stream cut them.
    let tx = inbox.clone();
    rt.spawn(async move {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        while let Ok(Some(n)) = ctl_rx.read(&mut chunk).await {
            buf.extend_from_slice(&chunk[..n]);
            let mut at = 0;
            while let Some(len) = whole_frame(&buf[at..]) {
                if tx.send(Ev::Net(id, In::Data(buf[at..at + len].to_vec()))).is_err() {
                    return;
                }
                at += len;
            }
            buf.drain(..at);
        }
        let _ = tx.send(Ev::Net(id, In::Closed(None)));
    });
    // Video, a stream a frame.
    let (c, tx) = (conn.clone(), inbox.clone());
    rt.spawn(async move {
        while let Ok(mut s) = c.accept_uni().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut frame = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                while let Ok(Some(n)) = s.read(&mut chunk).await {
                    frame.extend_from_slice(&chunk[..n]);
                }
                let _ = tx.send(Ev::Net(id, In::Data(frame)));
            });
        }
    });
    // Video as datagrams: put back together, lost shards rebuilt from parity.
    let (c, tx, f, d) = (conn.clone(), inbox.clone(), fec.clone(), datagrams.clone());
    rt.spawn(async move {
        while let Ok(dg) = c.receive_datagram().await {
            d.store(true, Ordering::Relaxed);
            let frame = f.lock().unwrap().push(&dg.payload());
            if let Some((_, frame)) = frame {
                if tx.send(Ev::Net(id, In::Data(frame))).is_err() {
                    return;
                }
            }
        }
    });
    let (c, tx) = (conn.clone(), inbox);
    rt.spawn(async move {
        let why = c.closed().await;
        let _ = tx.send(Ev::Net(id, In::Closed(Some(why.to_string()))));
    });
    Ok(Link {
        id,
        transport,
        out: Out::Wt(out_tx),
        seal: None,
        fec: Some(fec),
        datagrams: Some(datagrams),
        _rt: Some(rt),
        // (The endpoint is kept with it: its socket goes when both do.)
        closer: Some(Box::new(move || {
            conn.close(0u32.into(), b"bye");
            drop(ep);
        })),
    })
}

/// The length of the first whole protocol frame in `b`, if it's all there.
fn whole_frame(b: &[u8]) -> Option<usize> {
    let h = rr_proto::Header::read(b).ok()?;
    let len = rr_proto::HEADER_LEN + h.len;
    (b.len() >= len).then_some(len)
}
