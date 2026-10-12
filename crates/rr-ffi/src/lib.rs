//! Rein Remote's native client core ([`rr_client`]) behind a small C ABI, for apps written in other
//! languages: the iOS and iPadOS app (Swift) links it as a static library in an XCFramework. The
//! header is `include/rr_ffi.h`; it says what each function does and which thread may call it.
//!
//! A session's handle holds the session (whose events one thread waits for) and a
//! [`rr_client::Control`] (input and stats from any thread). Events are copied into a C struct that owns its bytes and
//! text until `rr_event_free`. Nothing here parses the wire: the protocol and the pairing stay in the
//! core, shared with every other client.

use rr_client::{Control, Event, Options};
use rr_proto::{Button, Codec, CodecCap, Input, Message, INPUT_GAMEPAD, INPUT_KEYBOARD, INPUT_POINTER, INPUT_TOUCH};
use std::ffi::{c_char, CStr, CString};
use std::sync::Mutex;
use std::time::Duration;

/// What the app wants (`RrOptions`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RrOptions {
    pub webtransport: bool,
    pub datagrams: bool,
    pub direct: bool,
    pub audio: bool,
    pub gamepad: bool,
    pub codecs: u32,
    pub hardware: bool,
    pub refresh_mhz: u32,
}

impl Default for RrOptions {
    fn default() -> Self {
        Self {
            webtransport: true,
            datagrams: true,
            direct: true,
            audio: false,
            gamepad: false,
            codecs: 1 << Codec::H264 as u32,
            hardware: false,
            refresh_mhz: 60_000,
        }
    }
}

pub const RR_BUTTON_LEFT: u8 = 0;
pub const RR_BUTTON_RIGHT: u8 = 1;
pub const RR_BUTTON_MIDDLE: u8 = 2;
pub const RR_BUTTON_BACK: u8 = 3;
pub const RR_BUTTON_FORWARD: u8 = 4;

pub const RR_EVENT_HELLO: u32 = 1;
pub const RR_EVENT_CONFIG: u32 = 2;
pub const RR_EVENT_VIDEO: u32 = 3;
pub const RR_EVENT_CURSOR: u32 = 4;
pub const RR_EVENT_AUDIO: u32 = 5;
pub const RR_EVENT_CLIPBOARD: u32 = 6;
pub const RR_EVENT_TRANSPORT: u32 = 7;
pub const RR_EVENT_NOTE: u32 = 8;
pub const RR_EVENT_CLOSED: u32 = 9;

/// One event (`RrEvent`): the fields its kind uses, the rest zero.
#[repr(C)]
#[derive(Debug)]
pub struct RrEvent {
    pub kind: u32,
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
    pub frame: u32,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub received_us: u64,
    pub has_latency: bool,
    pub latency_us: i64,
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub has_shape: bool,
    pub shape_width: u16,
    pub shape_height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub server_input: u8,
    pub server_audio: bool,
    pub data: *const u8,
    pub len: usize,
    pub text: *const c_char,
}

/// An event with what it points at: `c` first, so a pointer to it is a pointer to the whole.
#[repr(C)]
struct Owned {
    c: RrEvent,
    data: Vec<u8>,
    text: Option<CString>,
}

/// The session's numbers (`RrStats`).
#[repr(C)]
#[derive(Debug)]
pub struct RrStats {
    pub frames: u64,
    pub keyframes: u64,
    pub bytes: u64,
    pub frames_lost: u64,
    pub fec_recovered: u64,
    pub fec_lost: u64,
    pub loss_permille: u16,
    pub rtt_ms: f64,
    pub rtt_min_ms: f64,
    pub has_clock_offset: bool,
    pub clock_offset_us: i64,
    pub transport: *const c_char,
}

/// A session (`RrSession`).
pub struct RrSession {
    events: Mutex<rr_client::Session>,
    sender: Control,
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap_or_default()
}

/// A string from C (None for NULL or what isn't UTF-8).
///
/// # Safety
/// `p` is NULL or a NUL-terminated string that outlives the result.
unsafe fn string<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// Puts `msg` in `*error` (when the caller asked for it).
///
/// # Safety
/// `error` is NULL or points at a `char *` the caller owns.
unsafe fn fail(error: *mut *mut c_char, msg: &str) {
    if !error.is_null() {
        *error = cstring(msg).into_raw();
    }
}

fn codec_caps(o: &RrOptions) -> Vec<CodecCap> {
    let mut caps: Vec<CodecCap> = [Codec::H264, Codec::H265, Codec::Av1, Codec::Vp9, Codec::Vp8]
        .into_iter()
        .filter(|c| o.codecs & (1 << *c as u32) != 0)
        .map(|codec| CodecCap { codec, hardware: o.hardware })
        .collect();
    if caps.is_empty() {
        caps.push(CodecCap {
            codec: Codec::H264,
            hardware: o.hardware,
        });
    }
    caps
}

/// The core's options for these, named `name`, pairing with `pair` when not yet paired. The app sends
/// its own feedback once a frame is decoded.
pub fn options(o: &RrOptions, name: &str, pair: Option<&str>) -> Options {
    Options {
        name: if name.is_empty() { "rein-remote".into() } else { name.into() },
        pair: pair.filter(|p| !p.is_empty()).map(str::to_string),
        webtransport: o.webtransport,
        datagrams: o.datagrams,
        direct: o.direct,
        codecs: codec_caps(o),
        refresh_mhz: o.refresh_mhz,
        auto_feedback: false,
        keyframe_on_loss: true,
        input: INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH | if o.gamepad { INPUT_GAMEPAD } else { 0 },
        audio: o.audio,
    }
}

fn blank(kind: u32) -> RrEvent {
    RrEvent {
        kind,
        codec: 0,
        width: 0,
        height: 0,
        fps: 0,
        bitrate_kbps: 0,
        frame: 0,
        keyframe: false,
        timestamp_us: 0,
        received_us: 0,
        has_latency: false,
        latency_us: 0,
        x: 0,
        y: 0,
        visible: false,
        has_shape: false,
        shape_width: 0,
        shape_height: 0,
        hot_x: 0,
        hot_y: 0,
        server_input: 0,
        server_audio: false,
        data: std::ptr::null(),
        len: 0,
        text: std::ptr::null(),
    }
}

/// An event as C sees it, owning its bytes and text.
fn convert(e: Event) -> Box<Owned> {
    let (c, data, text) = match e {
        Event::Hello(h) => (
            RrEvent {
                server_input: h.input,
                server_audio: h.audio,
                ..blank(RR_EVENT_HELLO)
            },
            vec![],
            Some(h.name),
        ),
        Event::Config(c) => (
            RrEvent {
                codec: c.codec as u8,
                width: c.width,
                height: c.height,
                fps: c.fps,
                bitrate_kbps: c.bitrate_kbps,
                ..blank(RR_EVENT_CONFIG)
            },
            vec![],
            None,
        ),
        Event::Video {
            frame,
            keyframe,
            timestamp_us,
            received_us,
            latency_us,
            data,
        } => (
            RrEvent {
                frame,
                keyframe,
                timestamp_us,
                received_us,
                has_latency: latency_us.is_some(),
                latency_us: latency_us.unwrap_or(0),
                ..blank(RR_EVENT_VIDEO)
            },
            data,
            None,
        ),
        Event::Cursor(c) => {
            let mut ev = RrEvent {
                x: c.x,
                y: c.y,
                visible: c.visible,
                ..blank(RR_EVENT_CURSOR)
            };
            let data = match c.shape {
                Some(s) => {
                    ev.has_shape = true;
                    (ev.shape_width, ev.shape_height, ev.hot_x, ev.hot_y) = (s.w, s.h, s.hx, s.hy);
                    s.rgba
                }
                None => vec![],
            };
            (ev, data, None)
        }
        Event::Audio(a) => (
            RrEvent {
                timestamp_us: a.timestamp_us,
                ..blank(RR_EVENT_AUDIO)
            },
            a.data,
            None,
        ),
        Event::Clipboard(t) => (blank(RR_EVENT_CLIPBOARD), vec![], Some(t)),
        Event::Transport(t) => (blank(RR_EVENT_TRANSPORT), vec![], Some(t.to_string())),
        Event::Note(n) => (blank(RR_EVENT_NOTE), vec![], Some(n)),
        Event::Closed(why) => (blank(RR_EVENT_CLOSED), vec![], why),
    };
    let mut o = Box::new(Owned {
        c,
        data,
        text: text.map(|t| cstring(&t)),
    });
    // The pointers into the box's own Vec and CString (their heap memory doesn't move with the box).
    o.c.data = if o.data.is_empty() { std::ptr::null() } else { o.data.as_ptr() };
    o.c.len = o.data.len();
    o.c.text = o.text.as_ref().map_or(std::ptr::null(), |t| t.as_ptr());
    o
}

#[no_mangle]
pub extern "C" fn rr_options_default() -> RrOptions {
    RrOptions::default()
}

/// # Safety
/// `path` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn rr_set_home(path: *const c_char) -> bool {
    match string(path) {
        Some(p) if !p.is_empty() => rr_client::keys::set_home(p.into()),
        _ => false,
    }
}

/// # Safety
/// `target` and `name` are NUL-terminated strings; `pair_code` is NULL or one; `opts` is NULL or
/// points at an `RrOptions`; `error` is NULL or points at a `char *`.
#[no_mangle]
pub unsafe extern "C" fn rr_connect(
    target: *const c_char,
    name: *const c_char,
    pair_code: *const c_char,
    opts: *const RrOptions,
    error: *mut *mut c_char,
) -> *mut RrSession {
    let Some(target) = string(target) else {
        fail(error, "no link to connect to");
        return std::ptr::null_mut();
    };
    let o = if opts.is_null() { RrOptions::default() } else { *opts };
    match rr_client::connect(target, options(&o, string(name).unwrap_or(""), string(pair_code))) {
        Ok(s) => Box::into_raw(Box::new(RrSession {
            sender: s.control(),
            events: Mutex::new(s),
        })),
        Err(e) => {
            fail(error, &e);
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_next_event(s: *const RrSession, wait_ms: u32) -> *mut RrEvent {
    let Some(s) = s.as_ref() else { return std::ptr::null_mut() };
    let e = s.events.lock().unwrap().next_event(Duration::from_millis(wait_ms as u64));
    e.map_or(std::ptr::null_mut(), |e| Box::into_raw(convert(e)) as *mut RrEvent)
}

/// # Safety
/// `e` is NULL or an event from `rr_next_event`, freed once.
#[no_mangle]
pub unsafe extern "C" fn rr_event_free(e: *mut RrEvent) {
    if !e.is_null() {
        drop(Box::from_raw(e as *mut Owned));
    }
}

/// # Safety
/// `s` is NULL or a session from `rr_connect`, not yet freed.
unsafe fn input(s: *const RrSession, i: Input) {
    if let Some(s) = s.as_ref() {
        s.sender.input(i);
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_pointer_move(s: *const RrSession, x: u16, y: u16) {
    input(s, Input::PointerMove { x, y });
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_pointer_delta(s: *const RrSession, dx: i16, dy: i16) {
    input(s, Input::PointerDelta { dx, dy });
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_button(s: *const RrSession, button: u8, down: bool) {
    let button = match button {
        1 => Button::Right,
        2 => Button::Middle,
        3 => Button::Back,
        4 => Button::Forward,
        _ => Button::Left,
    };
    input(s, Input::Button { button, down });
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_wheel(s: *const RrSession, dx: i16, dy: i16) {
    input(s, Input::Wheel { dx, dy });
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed; `name` is NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn rr_key(s: *const RrSession, hid: u16, down: bool, name: *const c_char) {
    let key = string(name).unwrap_or("").to_string();
    input(s, Input::Key { hid, down, key });
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed; `text` is NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn rr_text(s: *const RrSession, text: *const c_char) {
    if let Some(t) = string(text).filter(|t| !t.is_empty()) {
        input(s, Input::Text { text: t.to_string() });
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed; `ids`, `xs` and `ys` each point at `n` values
/// (or anything, when `n` is 0).
#[no_mangle]
pub unsafe extern "C" fn rr_touch(s: *const RrSession, ids: *const u8, xs: *const u16, ys: *const u16, n: usize) {
    let points = if n == 0 || ids.is_null() || xs.is_null() || ys.is_null() {
        vec![]
    } else {
        let (ids, xs, ys) = (
            std::slice::from_raw_parts(ids, n),
            std::slice::from_raw_parts(xs, n),
            std::slice::from_raw_parts(ys, n),
        );
        (0..n).map(|i| (ids[i], xs[i], ys[i])).collect()
    };
    input(s, Input::Touch { points });
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn rr_gamepad(s: *const RrSession, pad: u8, buttons: u32, lx: i16, ly: i16, rx: i16, ry: i16, lt: u8, rt: u8) {
    input(
        s,
        Input::Gamepad {
            pad,
            buttons,
            lx,
            ly,
            rx,
            ry,
            lt,
            rt,
        },
    );
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed; `text` is NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn rr_clipboard(s: *const RrSession, text: *const c_char) {
    if let (Some(s), Some(t)) = (s.as_ref(), string(text)) {
        s.sender.send(Message::Clipboard { text: t.to_string() });
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_resize(s: *const RrSession, width: u16, height: u16) {
    if let Some(s) = s.as_ref() {
        s.sender.send(Message::Resize { width, height });
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_keyframe(s: *const RrSession) {
    if let Some(s) = s.as_ref() {
        s.sender.keyframe();
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_feedback(s: *const RrSession, frame: u32, received_us: u64, decoded_us: u64, lost: u16) {
    if let Some(s) = s.as_ref() {
        let loss_permille = s.sender.stats().loss_permille;
        s.sender.feedback(rr_proto::Feedback {
            frame,
            received_us,
            decoded_us,
            lost,
            loss_permille,
        });
    }
}

fn transport_name(t: &str) -> &'static CStr {
    match t {
        "webtransport-datagrams" => c"webtransport-datagrams",
        "webtransport-streams" => c"webtransport-streams",
        "websocket" => c"websocket",
        "relay" => c"relay",
        "direct-after-punch" => c"direct-after-punch",
        _ => c"",
    }
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed; `out` points at an `RrStats`.
#[no_mangle]
pub unsafe extern "C" fn rr_stats(s: *const RrSession, out: *mut RrStats) {
    let (Some(s), false) = (s.as_ref(), out.is_null()) else { return };
    let st = s.sender.stats();
    *out = RrStats {
        frames: st.frames,
        keyframes: st.keyframes,
        bytes: st.bytes,
        frames_lost: st.frames_lost,
        fec_recovered: st.fec_recovered,
        fec_lost: st.fec_lost,
        loss_permille: st.loss_permille,
        rtt_ms: st.rtt_ms,
        rtt_min_ms: st.rtt_min_ms,
        has_clock_offset: st.clock_offset_us.is_some(),
        clock_offset_us: st.clock_offset_us.unwrap_or(0),
        transport: transport_name(st.transport).as_ptr(),
    };
}

/// # Safety
/// `s` is a session from `rr_connect`, not yet freed.
#[no_mangle]
pub unsafe extern "C" fn rr_close(s: *const RrSession) {
    if let Some(s) = s.as_ref() {
        s.sender.close();
    }
}

/// # Safety
/// `s` is NULL or a session from `rr_connect`, freed once, with no other call on it running.
#[no_mangle]
pub unsafe extern "C" fn rr_free(s: *mut RrSession) {
    if !s.is_null() {
        drop(Box::from_raw(s));
    }
}

/// # Safety
/// `url`, `code` and `name` are NUL-terminated strings; `error` is NULL or points at a `char *`.
#[no_mangle]
pub unsafe extern "C" fn rr_pair(url: *const c_char, code: *const c_char, name: *const c_char, error: *mut *mut c_char) -> *mut c_char {
    let (Some(url), Some(code)) = (string(url), string(code)) else {
        fail(error, "pairing needs the server's address and its code");
        return std::ptr::null_mut();
    };
    let r = rr_client::http::parse(url).and_then(|u| rr_client::pairing::pair(&u.origin, code.trim(), string(name).unwrap_or("rein-remote")));
    match r {
        Ok(id) => cstring(&id).into_raw(),
        Err(e) => {
            fail(error, &e);
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `id` is NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn rr_is_paired(id: *const c_char) -> bool {
    string(id).is_some_and(|id| rr_client::pairing::paired(id).is_some())
}

fn json(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// What discovery found, as JSON.
pub fn found_json(found: &[rr_client::discover::Found]) -> String {
    let items: Vec<String> = found
        .iter()
        .map(|f| {
            let addrs: Vec<String> = f.addrs.iter().map(|a| json(&a.to_string())).collect();
            format!(
                r#"{{"name":{},"addrs":[{}],"port":{},"id":{},"webtransport":{},"https":{}}}"#,
                json(&f.name),
                addrs.join(","),
                f.port,
                json(&f.id),
                f.webtransport.map_or("null".into(), |p| p.to_string()),
                f.https
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// # Safety
/// `error` is NULL or points at a `char *`.
#[no_mangle]
pub unsafe extern "C" fn rr_discover(wait_ms: u32, error: *mut *mut c_char) -> *mut c_char {
    match rr_client::discover::discover(Duration::from_millis(wait_ms as u64)) {
        Ok(f) => cstring(&found_json(&f)).into_raw(),
        Err(e) => {
            fail(error, &e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn rr_now_us() -> u64 {
    rr_client::now_us()
}

#[no_mangle]
pub extern "C" fn rr_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// # Safety
/// `s` is NULL or a string this library returned, freed once.
#[no_mangle]
pub unsafe extern "C" fn rr_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_own_what_they_point_at() {
        let e = convert(Event::Video {
            frame: 7,
            keyframe: true,
            timestamp_us: 1,
            received_us: 2,
            latency_us: Some(3),
            data: vec![0, 0, 0, 1, 0x65],
        });
        let p = Box::into_raw(e) as *mut RrEvent;
        unsafe {
            let c = &*p;
            assert_eq!((c.kind, c.frame, c.keyframe, c.has_latency, c.latency_us), (RR_EVENT_VIDEO, 7, true, true, 3));
            assert_eq!(std::slice::from_raw_parts(c.data, c.len), &[0, 0, 0, 1, 0x65]);
            assert!(c.text.is_null());
            rr_event_free(p);
        }
        let e = convert(Event::Cursor(rr_proto::Cursor {
            x: 5,
            y: 6,
            visible: true,
            shape: Some(rr_proto::CursorShape {
                w: 1,
                h: 2,
                hx: 0,
                hy: 1,
                rgba: vec![9; 8],
            }),
        }));
        assert!(e.c.has_shape && e.c.len == 8 && (e.c.shape_width, e.c.shape_height, e.c.hot_y) == (1, 2, 1));
        let e = convert(Event::Closed(Some("gone\0away".into())));
        assert_eq!(unsafe { CStr::from_ptr(e.c.text) }.to_str().unwrap(), "gone away");
        assert!(convert(Event::Closed(None)).c.text.is_null());
    }

    #[test]
    fn options_and_codecs() {
        let o = RrOptions {
            codecs: (1 << 1) | (1 << 2),
            hardware: true,
            audio: true,
            ..Default::default()
        };
        let opts = options(&o, "iPad", Some(""));
        assert_eq!(opts.codecs.len(), 2);
        assert!(opts.codecs.iter().all(|c| c.hardware));
        assert_eq!(opts.pair, None);
        assert!(opts.audio && !opts.auto_feedback);
        let none = options(
            &RrOptions {
                codecs: 0,
                ..Default::default()
            },
            "",
            None,
        );
        assert_eq!(none.codecs[0].codec, Codec::H264);
        assert_eq!(none.name, "rein-remote");
    }

    #[test]
    fn a_bad_link_says_why() {
        let mut err: *mut c_char = std::ptr::null_mut();
        let s = unsafe { rr_connect(c"nonsense://".as_ptr(), c"t".as_ptr(), std::ptr::null(), std::ptr::null(), &mut err) };
        assert!(s.is_null() && !err.is_null());
        unsafe { rr_string_free(err) };
        let s = unsafe { rr_connect(std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null_mut()) };
        assert!(s.is_null());
    }

    #[test]
    fn discovery_as_json() {
        let f = rr_client::discover::Found {
            name: "box \"1\"".into(),
            addrs: vec!["192.168.1.2".parse().unwrap()],
            port: 7979,
            id: "abcd".into(),
            webtransport: Some(7979),
            https: false,
        };
        assert_eq!(
            found_json(&[f]),
            r#"[{"name":"box \"1\"","addrs":["192.168.1.2"],"port":7979,"id":"abcd","webtransport":7979,"https":false}]"#
        );
        assert_eq!(unsafe { CStr::from_ptr(rr_version()) }.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }
}
