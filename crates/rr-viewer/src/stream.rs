//! The stream's own thread: events from the session in, frames decoded, the latest picture handed to
//! whoever shows it (a window, or nothing when headless), and the server told what arrived and when it
//! was decoded (feedback: what its bitrate control runs on). A decoder that loses its place asks for a
//! keyframe and skips frames until one comes, as the web client does.

use crate::decode::{self, Decoder, Picture, Prefer};
use crate::measure;
use rr_client::{now_us, Event, Session};
use rr_proto::{Codec, Config, Cursor, Feedback, Hello};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the stream's thread tells whoever shows it.
pub enum Update {
    /// A new picture is in `Shared::latest`.
    Frame,
    Cursor(Cursor),
    /// The status changed (connected, a new path, a decoder chosen, closed).
    Status,
}

/// Where things are now: read for the title bar, the overlay and the report.
#[derive(Clone, Default)]
pub struct Status {
    /// "connecting", "streaming", "closed: why".
    pub state: String,
    pub server: String,
    /// The transport's label ("webtransport-datagrams", "relay", "direct-after-punch").
    pub transport: String,
    pub decoder: String,
    pub hardware: bool,
    pub config: Option<Config>,
    /// The server's input kinds (`INPUT_*`), from its Hello.
    pub server_input: u8,
    /// Frames decoded, and keyframes asked for after an error.
    pub decoded: u64,
    pub keyframe_requests: u64,
    pub errors: u64,
    /// Capture to decoded, ms (the session's clock estimate, plus decoding), the last one.
    pub latency_ms: Option<f64>,
    pub notes: Vec<String>,
    pub closed: bool,
}

/// Shared between the stream's thread and the window.
#[derive(Default)]
pub struct Shared {
    /// The newest picture not yet shown (a newer one replaces it: never a queue of old frames).
    pub latest: Mutex<Option<Picture>>,
    /// Pictures shown, given back to decode into (no allocation per frame).
    pub spare: Mutex<Vec<Picture>>,
    pub status: Mutex<Status>,
}

pub struct Options {
    pub prefer: Prefer,
    /// Read the test pattern's barcode from each picture.
    pub measure: bool,
}

/// Runs the stream until the session ends (or `stop` says so), calling `update` as things change.
pub fn run(session: Session, shared: Arc<Shared>, opts: Options, stop: impl Fn() -> bool, update: impl Fn(Update)) {
    let control = session.control();
    let mut decoder: Option<Box<dyn Decoder>> = None;
    let mut codec: Option<Codec> = None;
    let mut waiting_key = true;
    let mut last_ask = Instant::now() - Duration::from_secs(1);
    let mut lost_reported = 0u64;
    // Decode errors in a row (a hardware decoder that keeps failing gives way to software).
    let mut failures = 0u32;
    let set = |f: &mut dyn FnMut(&mut Status)| {
        f(&mut shared.status.lock().unwrap());
        update(Update::Status);
    };
    set(&mut |s| s.state = "connecting".into());
    while !stop() {
        let Some(ev) = session.next_event(Duration::from_millis(100)) else {
            continue;
        };
        match ev {
            Event::Hello(Hello { name, input, .. }) => {
                let t = session.stats().transport;
                set(&mut |s| {
                    s.server = name.clone();
                    s.server_input = input;
                    s.transport = t.into();
                    s.state = "connected".into();
                });
            }
            Event::Config(c) => {
                // A new codec or size: a decoder for it (the old one can't carry on), from a keyframe.
                let same = codec == Some(c.codec) && shared.status.lock().unwrap().config.is_some_and(|o| (o.width, o.height) == (c.width, c.height));
                if !same || decoder.is_none() {
                    let mut notes = Vec::new();
                    decoder = None;
                    match decode::open(c.codec, c.width as usize, c.height as usize, opts.prefer, &mut notes) {
                        Ok(d) => {
                            eprintln!("rein-remote-viewer: decoding with {}", d.name());
                            let (name, hw) = (d.name(), d.hardware());
                            decoder = Some(d);
                            set(&mut |s| {
                                s.decoder = name.clone();
                                s.hardware = hw;
                            });
                        }
                        Err(e) => {
                            eprintln!("rein-remote-viewer: can't decode {:?}: {e}", c.codec);
                            notes.push(format!("can't decode {:?}: {e}", c.codec));
                        }
                    }
                    for n in &notes {
                        eprintln!("rein-remote-viewer: {n}");
                    }
                    codec = Some(c.codec);
                    waiting_key = true;
                    set(&mut |s| s.notes.extend(notes.iter().cloned()));
                }
                set(&mut |s| {
                    s.config = Some(c);
                    s.state = "streaming".into();
                });
            }
            Event::Video {
                frame,
                keyframe,
                received_us,
                latency_us,
                data,
                ..
            } => {
                let Some(dec) = decoder.as_mut() else { continue };
                if waiting_key && !keyframe {
                    continue;
                }
                let mut pic = shared.spare.lock().unwrap().pop().unwrap_or_default();
                let started = Instant::now();
                let result = dec.decode(&data, keyframe, &mut pic);
                if result.is_ok() {
                    waiting_key = false;
                }
                match result {
                    Ok(true) => {
                        failures = 0;
                        let decoded_us = now_us();
                        pic.frame = frame;
                        pic.received_us = received_us;
                        pic.barcode = if opts.measure { measure::barcode(&pic) } else { None };
                        // What arrived and when it was decoded (the server takes the decoding time out
                        // of the round trip it measures), with frames lost since the last.
                        let stats = control.stats();
                        let lost = stats.frames_lost - lost_reported;
                        lost_reported = stats.frames_lost;
                        control.feedback(Feedback {
                            frame,
                            received_us,
                            decoded_us,
                            lost: lost.min(u16::MAX as u64) as u16,
                            loss_permille: stats.loss_permille,
                        });
                        let decode_ms = started.elapsed().as_secs_f64() * 1000.0;
                        let transport = stats.transport;
                        set(&mut |s| {
                            s.decoded += 1;
                            s.latency_ms = latency_us.map(|l| l as f64 / 1000.0 + decode_ms);
                            s.transport = transport.into();
                        });
                        // The newest picture replaces one not yet shown (which goes back to be reused).
                        if let Some(old) = shared.latest.lock().unwrap().replace(pic) {
                            shared.spare.lock().unwrap().push(old);
                        }
                        update(Update::Frame);
                    }
                    Ok(false) => shared.spare.lock().unwrap().push(pic),
                    Err(e) => {
                        shared.spare.lock().unwrap().push(pic);
                        waiting_key = true;
                        // A hardware decoder that fails keyframe after keyframe can't take this
                        // stream: software from here on, when there's one for it.
                        failures += 1;
                        if failures >= 3 && dec.hardware() && opts.prefer == Prefer::Auto {
                            let mut notes = Vec::new();
                            if let Some(c) = codec {
                                if let Ok(sw) = decode::open(c, 0, 0, Prefer::Software, &mut notes) {
                                    let note = format!("{} failed {failures} times ({e}): {} instead", dec.name(), sw.name());
                                    eprintln!("rein-remote-viewer: {note}");
                                    let (name, hw) = (sw.name(), sw.hardware());
                                    *dec = sw;
                                    failures = 0;
                                    set(&mut |s| {
                                        s.decoder = name.clone();
                                        s.hardware = hw;
                                        s.notes.push(note.clone());
                                    });
                                }
                            }
                        }
                        let ask = last_ask.elapsed() > Duration::from_millis(250);
                        if ask {
                            last_ask = Instant::now();
                            control.keyframe();
                        }
                        eprintln!("rein-remote-viewer: {e}{}", if ask { ": asked for a keyframe" } else { "" });
                        set(&mut |s| {
                            s.errors += 1;
                            s.keyframe_requests += ask as u64;
                        });
                    }
                }
            }
            Event::Cursor(c) => update(Update::Cursor(c)),
            Event::Transport(t) => {
                eprintln!("rein-remote-viewer: now on {t}");
                set(&mut |s| {
                    s.transport = t.into();
                    s.notes.push(format!("now on {t}"));
                });
            }
            Event::Note(n) => {
                eprintln!("rein-remote-viewer: {n}");
                set(&mut |s| s.notes.push(n.clone()));
            }
            Event::Closed(why) => {
                let why = why.unwrap_or_else(|| "the server closed the session".into());
                eprintln!("rein-remote-viewer: closed: {why}");
                set(&mut |s| {
                    s.state = format!("closed: {why}");
                    s.closed = true;
                });
                return;
            }
            Event::Audio(_) | Event::Clipboard(_) => {}
        }
    }
}
