//! Rein Remote's wire protocol (v0): frames and the messages in them.
//!
//! Every frame is `kind: u8, version: u8, flags: u8, len: u32 (little-endian), body`. The same
//! frames go over every transport (QUIC, WebTransport, WebSocket). No I/O here and no dependencies:
//! the web client uses this crate as WebAssembly.
//!
//! # Compatibility
//!
//! Peers of different versions keep working together, whichever was updated first:
//!
//! - **A new message** gets a new kind. A reader skips kinds it doesn't know, by their length.
//! - **A new field** goes at the end of its message, and the message's version (in [`version`]) goes
//!   up. A newer reader fills in a default when an older peer's frame stops before the field
//!   ([`Reader`]'s `or_*` helpers); an older reader reads what it knows and ignores the rest.
//! - **Anything else** (a field that changes meaning or type, one removed) is a new kind, next to
//!   the old one, which is kept as long as peers may need it. `Hello`'s version range says which
//!   both speak ([`negotiate`]).
//!
//! So bodies only ever grow at the end, byte blobs are length-prefixed (so fields can follow them),
//! and values a reader doesn't know (a newer input kind, a newer codec) are skipped, never fatal.

#![forbid(unsafe_code)]

pub mod fec;
pub mod pair;
pub mod punch;
mod wire;

pub use wire::{Reader, Writer};

/// The protocol versions this build speaks (both ends announce theirs in `Hello`).
pub const VERSION_MIN: u16 = 0;
pub const VERSION_MAX: u16 = 0;

/// Frame header: kind, version, flags, body length.
pub const HEADER_LEN: usize = 7;
/// Largest body accepted: big enough for a 4K keyframe, small enough to bound memory.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    H264 = 1,
    H265 = 2,
    Av1 = 3,
    Vp9 = 4,
    Vp8 = 5,
}

impl Codec {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::H264,
            2 => Self::H265,
            3 => Self::Av1,
            4 => Self::Vp9,
            5 => Self::Vp8,
            _ => return None,
        })
    }
}

/// One codec a side can encode (server) or decode (client), and whether it's in hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecCap {
    pub codec: Codec,
    pub hardware: bool,
}

/// What a side can do: sent once in `Hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub version_min: u16,
    pub version_max: u16,
    /// A name to show ("build-box", "Gary's iPad").
    pub name: String,
    pub codecs: Vec<CodecCap>,
    pub max_width: u16,
    pub max_height: u16,
    pub max_fps: u16,
    /// A server: it can stream the sound the machine plays. A client: it plays Opus (48 kHz,
    /// stereo); a server sends `Audio` only to a client that says so.
    pub audio: bool,
    /// Bit set of `INPUT_*`: the input kinds a server accepts, or a client can send.
    pub input: u8,
    /// The client's display refresh rate as it measured it, in millihertz (59940 for 59.94 Hz); 0
    /// when unknown (a server, or a client older than Hello version 1). The server paces to it.
    pub refresh_mhz: u32,
    /// Bit set of `FEATURE_*` (Hello version 2; 0 from older peers).
    pub features: u32,
}

/// A client that takes video as datagrams with forward error correction (see [`fec`]), when the
/// transport has datagrams (WebTransport, QUIC).
pub const FEATURE_VIDEO_DATAGRAMS: u32 = 1;
/// A viewer that can take a direct path when it came through a relay: it can punch through NAT and
/// open QUIC itself (native clients; a browser can't, it has no UDP of its own). The host may then
/// send a [`DirectOffer`].
pub const FEATURE_DIRECT: u32 = 2;
/// A client that takes `Audio` as datagrams of their own (one whole frame each, told apart from
/// video's by the first byte: the frame's kind), when the transport has datagrams: a lost packet is a
/// gap, never a wait. Without it, audio goes in order with the control messages.
pub const FEATURE_AUDIO_DATAGRAMS: u32 = 4;

pub const INPUT_POINTER: u8 = 1;
pub const INPUT_KEYBOARD: u8 = 2;
pub const INPUT_TOUCH: u8 = 4;
pub const INPUT_GAMEPAD: u8 = 8;

/// The stream as the server sends it; sent again whenever any of it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    /// Target bitrate, kbit/s (it moves with the link).
    pub bitrate_kbps: u32,
}

/// One encoded video frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub keyframe: bool,
    pub frame: u32,
    /// Capture time, microseconds on the server's clock.
    pub timestamp_us: u64,
    pub data: Vec<u8>,
}

/// One Opus packet (48 kHz, stereo; 10 or 20 ms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    /// Capture time of its first sample, microseconds on the server's clock: the same clock as
    /// `Video`'s, so a client plays sound and picture at the same delay.
    pub timestamp_us: u64,
    pub data: Vec<u8>,
}

/// The pointer, drawn by the client (so moving it costs no video). `shape` only when it changed:
/// RGBA, `w`×`h`, hotspot at `hx`,`hy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub shape: Option<CursorShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub w: u16,
    pub h: u16,
    pub hx: u16,
    pub hy: u16,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left = 0,
    Right = 1,
    Middle = 2,
    Back = 3,
    Forward = 4,
}

/// Input from the client. Coordinates are in the stream's pixels (`Config.width`/`height`).
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    PointerMove {
        x: u16,
        y: u16,
    },
    /// Relative motion (games with the pointer locked).
    PointerDelta {
        dx: i16,
        dy: i16,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// Wheel, in pixels (positive: down / right).
    Wheel {
        dx: i16,
        dy: i16,
    },
    /// A key, by its position (USB HID usage), and its meaning (a key name such as "a" or "Enter").
    Key {
        hid: u16,
        down: bool,
        key: String,
    },
    /// Text typed (an IME, a phone keyboard, a paste).
    Text {
        text: String,
    },
    /// Touch points (id, x, y); empty when the last finger lifts.
    Touch {
        points: Vec<(u8, u16, u16)>,
    },
    /// A gamepad's state: buttons as a bit set, then sticks and triggers.
    ///
    /// Bit `i` of `buttons` is button `i` of the W3C "standard gamepad" layout: 0 A (bottom face), 1 B
    /// (right), 2 X (left), 3 Y (top), 4 left shoulder, 5 right shoulder, 6 left trigger, 7 right
    /// trigger, 8 back/select/view, 9 start/menu, 10 left stick press, 11 right stick press, 12 up,
    /// 13 down, 14 left, 15 right (the d-pad), 16 home/guide. Sticks from -32767 to 32767, positive
    /// right and down; triggers 0 (released) to 255 (fully pressed). `pad` tells gamepads apart.
    Gamepad {
        pad: u8,
        buttons: u32,
        lx: i16,
        ly: i16,
        rx: i16,
        ry: i16,
        lt: u8,
        rt: u8,
    },
}

/// What arrived and when: drives the bitrate, frame rate and resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feedback {
    pub frame: u32,
    /// When it arrived and when it was on screen, microseconds on the client's clock.
    pub received_us: u64,
    pub decoded_us: u64,
    /// Frames lost since the last feedback.
    pub lost: u16,
    /// Of the datagrams the client expected since the last feedback, the share that never came, in
    /// thousandths (Feedback version 1; 0 from older clients, or without datagrams). It sets how much
    /// parity goes with the video.
    pub loss_permille: u16,
}

/// A way around the relay, offered by the host inside a sealed relayed session (so the relay, which
/// can't read it, can't use or change any of it): where its QUIC socket may be reached, the
/// certificate to expect there, and a ticket good for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectOffer {
    /// The host's WebTransport socket as the relay saw it (its address on the far side of its NAT),
    /// then its addresses on its own networks.
    pub candidates: Vec<std::net::SocketAddr>,
    /// The SHA-256 of the host's WebTransport certificate: the viewer accepts that one and no other.
    pub cert_sha256: Vec<u8>,
    /// What the host's gate lets in once (`?ticket=`), for a minute.
    pub ticket: String,
}

/// The viewer's side of it: where its own QUIC socket may be reached (as the relay saw it, then on its
/// networks). Both then send punch packets ([`punch`]) to the other's candidates, and the viewer opens
/// WebTransport straight to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectAnswer {
    pub candidates: Vec<std::net::SocketAddr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello(Hello),
    Config(Config),
    Video(Video),
    Audio(Audio),
    Cursor(Cursor),
    Input(Input),
    Feedback(Feedback),
    KeyframeRequest,
    Clipboard { text: String },
    Resize { width: u16, height: u16 },
    Ping { id: u32, sent_us: u64 },
    Pong { id: u32, sent_us: u64 },
    DirectOffer(DirectOffer),
    DirectAnswer(DirectAnswer),
}

/// Frame kinds on the wire.
pub mod kind {
    pub const HELLO: u8 = 1;
    pub const CONFIG: u8 = 2;
    pub const VIDEO: u8 = 3;
    pub const AUDIO: u8 = 4;
    pub const CURSOR: u8 = 5;
    pub const INPUT: u8 = 6;
    pub const FEEDBACK: u8 = 7;
    pub const KEYFRAME_REQUEST: u8 = 8;
    pub const CLIPBOARD: u8 = 9;
    pub const RESIZE: u8 = 10;
    pub const PING: u8 = 11;
    pub const PONG: u8 = 12;
    pub const DIRECT_OFFER: u8 = 13;
    pub const DIRECT_ANSWER: u8 = 14;
}

/// Each message's version, written in its frame header: raised when a field is added at the end.
pub mod version {
    /// 1: `refresh_mhz`. 2: `features`.
    pub const HELLO: u8 = 2;
    pub const CONFIG: u8 = 0;
    pub const VIDEO: u8 = 0;
    pub const AUDIO: u8 = 0;
    pub const CURSOR: u8 = 0;
    pub const INPUT: u8 = 0;
    /// 1: `loss_permille`.
    pub const FEEDBACK: u8 = 1;
    pub const KEYFRAME_REQUEST: u8 = 0;
    pub const CLIPBOARD: u8 = 0;
    pub const RESIZE: u8 = 0;
    pub const PING: u8 = 0;
    pub const PONG: u8 = 0;
    pub const DIRECT_OFFER: u8 = 0;
    pub const DIRECT_ANSWER: u8 = 0;

    /// The version this build writes for a kind.
    pub fn of(kind: u8) -> u8 {
        use super::kind as k;
        match kind {
            k::HELLO => HELLO,
            k::CONFIG => CONFIG,
            k::VIDEO => VIDEO,
            k::AUDIO => AUDIO,
            k::CURSOR => CURSOR,
            k::INPUT => INPUT,
            k::FEEDBACK => FEEDBACK,
            k::KEYFRAME_REQUEST => KEYFRAME_REQUEST,
            k::CLIPBOARD => CLIPBOARD,
            k::RESIZE => RESIZE,
            k::PING => PING,
            k::DIRECT_OFFER => DIRECT_OFFER,
            k::DIRECT_ANSWER => DIRECT_ANSWER,
            _ => PONG,
        }
    }
}

/// One frame's header, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    /// The version of the message the sender wrote (it may be newer or older than this build's).
    pub version: u8,
    pub flags: u8,
    pub len: usize,
}

impl Header {
    /// The header at the front of `buf` (enough to route or drop a frame without reading its body).
    pub fn read(buf: &[u8]) -> Result<Header, Error> {
        if buf.len() < HEADER_LEN {
            return Err(Error::Incomplete);
        }
        let len = u32::from_le_bytes([buf[3], buf[4], buf[5], buf[6]]) as usize;
        if len > MAX_BODY {
            return Err(Error::TooLarge(len));
        }
        Ok(Header {
            kind: buf[0],
            version: buf[1],
            flags: buf[2],
            len,
        })
    }
}

/// Frame flag: the video frame is a keyframe (lets a relay or a backed-up sender drop frames until
/// the next keyframe without parsing the body).
pub const FLAG_KEYFRAME: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not enough bytes yet (a partial frame): read more and try again.
    Incomplete,
    TooLarge(usize),
    Malformed(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Incomplete => write!(f, "incomplete frame"),
            Error::TooLarge(n) => write!(f, "frame of {n} bytes is over the limit"),
            Error::Malformed(what) => write!(f, "malformed frame: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl Message {
    /// The whole frame: header and body.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        let (kind, flags) = self.write_body(&mut w);
        let body = w.into_inner();
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.push(kind);
        out.push(version::of(kind));
        out.push(flags);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn write_body(&self, w: &mut Writer) -> (u8, u8) {
        match self {
            Message::Hello(h) => {
                w.u16(h.version_min);
                w.u16(h.version_max);
                w.str(&h.name);
                w.u8(h.codecs.len() as u8);
                for c in &h.codecs {
                    w.u8(c.codec as u8);
                    w.u8(c.hardware as u8);
                }
                w.u16(h.max_width);
                w.u16(h.max_height);
                w.u16(h.max_fps);
                w.u8(h.audio as u8);
                w.u8(h.input);
                w.u32(h.refresh_mhz);
                w.u32(h.features);
                (kind::HELLO, 0)
            }
            Message::Config(c) => {
                w.u8(c.codec as u8);
                w.u16(c.width);
                w.u16(c.height);
                w.u16(c.fps);
                w.u32(c.bitrate_kbps);
                (kind::CONFIG, 0)
            }
            Message::Video(v) => {
                w.u32(v.frame);
                w.u64(v.timestamp_us);
                w.bytes(&v.data);
                (kind::VIDEO, if v.keyframe { FLAG_KEYFRAME } else { 0 })
            }
            Message::Audio(a) => {
                w.u64(a.timestamp_us);
                w.bytes(&a.data);
                (kind::AUDIO, 0)
            }
            Message::Cursor(c) => {
                w.u16(c.x);
                w.u16(c.y);
                w.u8(c.visible as u8);
                match &c.shape {
                    Some(s) => {
                        w.u8(1);
                        w.u16(s.w);
                        w.u16(s.h);
                        w.u16(s.hx);
                        w.u16(s.hy);
                        w.bytes(&s.rgba);
                    }
                    None => w.u8(0),
                }
                (kind::CURSOR, 0)
            }
            Message::Input(i) => {
                match i {
                    Input::PointerMove { x, y } => {
                        w.u8(1);
                        w.u16(*x);
                        w.u16(*y);
                    }
                    Input::PointerDelta { dx, dy } => {
                        w.u8(2);
                        w.i16(*dx);
                        w.i16(*dy);
                    }
                    Input::Button { button, down } => {
                        w.u8(3);
                        w.u8(*button as u8);
                        w.u8(*down as u8);
                    }
                    Input::Wheel { dx, dy } => {
                        w.u8(4);
                        w.i16(*dx);
                        w.i16(*dy);
                    }
                    Input::Key { hid, down, key } => {
                        w.u8(5);
                        w.u16(*hid);
                        w.u8(*down as u8);
                        w.str(key);
                    }
                    Input::Text { text } => {
                        w.u8(6);
                        w.str(text);
                    }
                    Input::Touch { points } => {
                        w.u8(7);
                        w.u8(points.len() as u8);
                        for (id, x, y) in points {
                            w.u8(*id);
                            w.u16(*x);
                            w.u16(*y);
                        }
                    }
                    Input::Gamepad {
                        pad,
                        buttons,
                        lx,
                        ly,
                        rx,
                        ry,
                        lt,
                        rt,
                    } => {
                        w.u8(8);
                        w.u8(*pad);
                        w.u32(*buttons);
                        for s in [lx, ly, rx, ry] {
                            w.i16(*s);
                        }
                        w.u8(*lt);
                        w.u8(*rt);
                    }
                }
                (kind::INPUT, 0)
            }
            Message::Feedback(f) => {
                w.u32(f.frame);
                w.u64(f.received_us);
                w.u64(f.decoded_us);
                w.u16(f.lost);
                w.u16(f.loss_permille);
                (kind::FEEDBACK, 0)
            }
            Message::KeyframeRequest => (kind::KEYFRAME_REQUEST, 0),
            Message::Clipboard { text } => {
                w.str(text);
                (kind::CLIPBOARD, 0)
            }
            Message::Resize { width, height } => {
                w.u16(*width);
                w.u16(*height);
                (kind::RESIZE, 0)
            }
            Message::Ping { id, sent_us } | Message::Pong { id, sent_us } => {
                w.u32(*id);
                w.u64(*sent_us);
                (if matches!(self, Message::Ping { .. }) { kind::PING } else { kind::PONG }, 0)
            }
            Message::DirectOffer(o) => {
                w.addrs(&o.candidates);
                w.bytes(&o.cert_sha256);
                w.str(&o.ticket);
                (kind::DIRECT_OFFER, 0)
            }
            Message::DirectAnswer(a) => {
                w.addrs(&a.candidates);
                (kind::DIRECT_ANSWER, 0)
            }
        }
    }

    /// Read one frame from the front of `buf`. Returns the message (or `None` for a kind this build
    /// doesn't know, which is skipped) and how many bytes it used.
    ///
    /// A frame from a newer peer (a higher version) reads as far as this build knows; one from an
    /// older peer gets defaults for fields added since (see the crate's compatibility rules).
    pub fn decode(buf: &[u8]) -> Result<(Option<Message>, usize), Error> {
        let Header { kind, flags, len, .. } = Header::read(buf)?;
        if buf.len() < HEADER_LEN + len {
            return Err(Error::Incomplete);
        }
        let mut r = Reader::new(&buf[HEADER_LEN..HEADER_LEN + len]);
        let msg = match kind {
            kind::HELLO => {
                let version_min = r.u16()?;
                let version_max = r.u16()?;
                let name = r.str()?;
                let n = r.u8()? as usize;
                let mut codecs = Vec::with_capacity(n);
                for _ in 0..n {
                    let c = r.u8()?;
                    let hardware = r.u8()? != 0;
                    // A codec this build doesn't know is left out, not an error.
                    if let Some(codec) = Codec::from_u8(c) {
                        codecs.push(CodecCap { codec, hardware });
                    }
                }
                Some(Message::Hello(Hello {
                    version_min,
                    version_max,
                    name,
                    codecs,
                    max_width: r.u16()?,
                    max_height: r.u16()?,
                    max_fps: r.u16()?,
                    audio: r.u8()? != 0,
                    input: r.u8()?,
                    refresh_mhz: r.or_u32(0)?,
                    features: r.or_u32(0)?,
                }))
            }
            kind::CONFIG => Some(Message::Config(Config {
                codec: Codec::from_u8(r.u8()?).ok_or(Error::Malformed("codec"))?,
                width: r.u16()?,
                height: r.u16()?,
                fps: r.u16()?,
                bitrate_kbps: r.u32()?,
            })),
            kind::VIDEO => Some(Message::Video(Video {
                keyframe: flags & FLAG_KEYFRAME != 0,
                frame: r.u32()?,
                timestamp_us: r.u64()?,
                data: r.bytes()?.to_vec(),
            })),
            kind::AUDIO => Some(Message::Audio(Audio {
                timestamp_us: r.u64()?,
                data: r.bytes()?.to_vec(),
            })),
            kind::CURSOR => {
                let (x, y, visible) = (r.u16()?, r.u16()?, r.u8()? != 0);
                let shape = if r.u8()? == 1 {
                    Some(CursorShape {
                        w: r.u16()?,
                        h: r.u16()?,
                        hx: r.u16()?,
                        hy: r.u16()?,
                        rgba: r.bytes()?.to_vec(),
                    })
                } else {
                    None
                };
                Some(Message::Cursor(Cursor { x, y, visible, shape }))
            }
            kind::INPUT => Some(Message::Input(match r.u8()? {
                1 => Input::PointerMove { x: r.u16()?, y: r.u16()? },
                2 => Input::PointerDelta { dx: r.i16()?, dy: r.i16()? },
                3 => Input::Button {
                    button: match r.u8()? {
                        0 => Button::Left,
                        1 => Button::Right,
                        2 => Button::Middle,
                        3 => Button::Back,
                        4 => Button::Forward,
                        // A button this build doesn't know (a newer peer): the event is skipped.
                        _ => return Ok((None, HEADER_LEN + len)),
                    },
                    down: r.u8()? != 0,
                },
                4 => Input::Wheel { dx: r.i16()?, dy: r.i16()? },
                5 => Input::Key {
                    hid: r.u16()?,
                    down: r.u8()? != 0,
                    key: r.str()?,
                },
                6 => Input::Text { text: r.str()? },
                7 => {
                    let n = r.u8()? as usize;
                    let mut points = Vec::with_capacity(n);
                    for _ in 0..n {
                        points.push((r.u8()?, r.u16()?, r.u16()?));
                    }
                    Input::Touch { points }
                }
                8 => Input::Gamepad {
                    pad: r.u8()?,
                    buttons: r.u32()?,
                    lx: r.i16()?,
                    ly: r.i16()?,
                    rx: r.i16()?,
                    ry: r.i16()?,
                    lt: r.u8()?,
                    rt: r.u8()?,
                },
                // An input kind this build doesn't know (a newer peer): skipped, not an error.
                _ => return Ok((None, HEADER_LEN + len)),
            })),
            kind::FEEDBACK => Some(Message::Feedback(Feedback {
                frame: r.u32()?,
                received_us: r.u64()?,
                decoded_us: r.u64()?,
                lost: r.u16()?,
                loss_permille: r.or_u16(0)?,
            })),
            kind::KEYFRAME_REQUEST => Some(Message::KeyframeRequest),
            kind::CLIPBOARD => Some(Message::Clipboard { text: r.str()? }),
            kind::RESIZE => Some(Message::Resize {
                width: r.u16()?,
                height: r.u16()?,
            }),
            kind::PING => Some(Message::Ping {
                id: r.u32()?,
                sent_us: r.u64()?,
            }),
            kind::PONG => Some(Message::Pong {
                id: r.u32()?,
                sent_us: r.u64()?,
            }),
            kind::DIRECT_OFFER => Some(Message::DirectOffer(DirectOffer {
                candidates: r.addrs()?,
                cert_sha256: r.bytes()?.to_vec(),
                ticket: r.str()?,
            })),
            kind::DIRECT_ANSWER => Some(Message::DirectAnswer(DirectAnswer { candidates: r.addrs()? })),
            _ => None,
        };
        Ok((msg, HEADER_LEN + len))
    }
}

/// The version both sides speak (the highest they share), or none.
pub fn negotiate(a: &Hello, b: &Hello) -> Option<u16> {
    let lo = a.version_min.max(b.version_min);
    let hi = a.version_max.min(b.version_max);
    (lo <= hi).then_some(hi)
}

/// The codec to stream with: the first in the server's order of preference that the client
/// decodes, hardware decode first.
pub fn pick_codec(server: &Hello, client: &Hello) -> Option<Codec> {
    let decodes = |c: Codec, hw: bool| client.codecs.iter().any(|x| x.codec == c && (!hw || x.hardware));
    server
        .codecs
        .iter()
        .map(|c| c.codec)
        .find(|&c| decodes(c, true))
        .or_else(|| server.codecs.iter().map(|c| c.codec).find(|&c| decodes(c, false)))
}

#[cfg(test)]
mod tests;
