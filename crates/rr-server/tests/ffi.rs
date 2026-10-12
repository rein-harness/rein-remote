//! The core through its C ABI (crates/rr-ffi, what the iOS app links), against a real `rein-remote
//! serve`: connected, the config, video and sound (as datagrams over WebTransport) as events, input of
//! every kind sent, the stats, and the end. The app's own tests do the same in the simulator, with
//! decoding.

mod common;
use common::Proc;
use rr_ffi::*;
use std::ffi::{c_char, CStr, CString};
use std::time::{Duration, Instant};

#[test]
fn a_session_through_the_c_abi() {
    let mut server = Proc::start(
        &[
            "serve",
            "--test-pattern",
            "320x240",
            "--listen",
            "127.0.0.1:0",
            "--token",
            "tok",
            "--mdns",
            "off",
        ],
        &[],
    );
    let addr = server.listening();
    let url = CString::new(format!("http://{addr}/?token=tok")).unwrap();
    let opts = RrOptions {
        audio: true,
        gamepad: true,
        ..rr_options_default()
    };
    let mut err: *mut c_char = std::ptr::null_mut();
    let s = unsafe { rr_connect(url.as_ptr(), c"ffi test".as_ptr(), std::ptr::null(), &opts, &mut err) };
    assert!(!s.is_null(), "connected: {:?}", unsafe { err.as_ref().map(|e| CStr::from_ptr(e)) });

    let (mut config, mut keyframe, mut video, mut audio, mut hello) = (None, false, 0, 0, None);
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end && !(config.is_some() && keyframe && video >= 10 && audio >= 10) {
        let e = unsafe { rr_next_event(s, 200) };
        let Some(ev) = (unsafe { e.as_ref() }) else { continue };
        match ev.kind {
            RR_EVENT_HELLO => hello = Some((unsafe { CStr::from_ptr(ev.text) }.to_str().unwrap().to_string(), ev.server_input)),
            RR_EVENT_CONFIG => config = Some((ev.codec, ev.width, ev.height)),
            RR_EVENT_VIDEO => {
                let data = unsafe { std::slice::from_raw_parts(ev.data, ev.len) };
                // Annex B: a start code first.
                assert!(data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]), "Annex B");
                keyframe |= ev.keyframe;
                video += 1;
                unsafe { rr_feedback(s, ev.frame, ev.received_us, rr_now_us(), 0) };
            }
            RR_EVENT_AUDIO => {
                assert!(ev.len > 0 && ev.timestamp_us > 0);
                audio += 1;
            }
            RR_EVENT_CLOSED => panic!("closed: {:?}", unsafe { ev.text.as_ref().map(|t| CStr::from_ptr(t)) }),
            _ => {}
        }
        unsafe { rr_event_free(e) };
    }
    assert_eq!(config, Some((1, 320, 240)), "an H.264 config");
    assert!(keyframe && video >= 10, "video: {video}");
    assert!(audio >= 10, "sound as datagrams: {audio} packets");
    let (_, input) = hello.expect("the server's hello");
    assert!(input & 1 != 0, "it takes the pointer");

    // Input of every kind, from another thread while this one waits for events.
    let p = s as usize;
    std::thread::spawn(move || unsafe {
        let s = p as *const RrSession;
        rr_pointer_move(s, 100, 100);
        rr_button(s, RR_BUTTON_LEFT, true);
        rr_button(s, RR_BUTTON_LEFT, false);
        rr_wheel(s, 0, 40);
        rr_key(s, 4, true, c"a".as_ptr());
        rr_key(s, 4, false, c"a".as_ptr());
        rr_text(s, c"hi".as_ptr());
        rr_touch(s, [1u8].as_ptr(), [5u16].as_ptr(), [6u16].as_ptr(), 1);
        rr_touch(s, std::ptr::null(), std::ptr::null(), std::ptr::null(), 0);
        rr_gamepad(s, 0, 1, 0, -32767, 0, 0, 0, 255);
        rr_keyframe(s);
    })
    .join()
    .unwrap();
    // The session goes on (input it doesn't take is skipped, never an error).
    let mut more = 0;
    let end = Instant::now() + Duration::from_secs(10);
    while more < 10 && Instant::now() < end {
        let e = unsafe { rr_next_event(s, 200) };
        if let Some(ev) = unsafe { e.as_ref() } {
            assert_ne!(ev.kind, RR_EVENT_CLOSED, "still streaming after the input");
            more += (ev.kind == RR_EVENT_VIDEO) as u32;
        }
        unsafe { rr_event_free(e) };
    }
    assert_eq!(more, 10);
    let mut st = std::mem::MaybeUninit::<RrStats>::uninit();
    let st = unsafe {
        rr_stats(s, st.as_mut_ptr());
        st.assume_init()
    };
    assert!(st.frames >= 20, "{st:?}");
    assert_eq!(unsafe { CStr::from_ptr(st.transport) }.to_str().unwrap(), "webtransport-datagrams");

    // Closed: the last event says so, and the handle goes.
    unsafe { rr_close(s) };
    let end = Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while !closed && Instant::now() < end {
        let e = unsafe { rr_next_event(s, 200) };
        closed = unsafe { e.as_ref() }.is_some_and(|ev| ev.kind == RR_EVENT_CLOSED);
        unsafe { rr_event_free(e) };
    }
    assert!(closed);
    unsafe { rr_free(s) };
}
