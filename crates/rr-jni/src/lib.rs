//! Rein Remote's native client core for the JVM: the JNI functions behind the Android app's
//! `dev.reinharness.remote.Native` (Kotlin), each a thin call into [`rr_ffi`]'s C ABI, the same one
//! the iOS app uses. So the protocol, the transports and pairing stay in the core, and this crate only
//! moves values between Java and C: no upcalls into Java, no JavaVM kept.
//!
//! Threads, as in `rr_ffi.h`: `connect` blocks (call it off the UI thread), one thread waits for
//! events with `nextEvent`, everything else may be called from any thread. A session is a `long`
//! handle until `free`.
//!
//! An event comes back as numbers in a `long[]` the caller owns ([`event_fields`]: `fields[0]` is its
//! kind, 0 when none came) and its bytes (video, audio, the cursor's RGBA) or text (UTF-8) as the
//! returned `byte[]`.

use jni::objects::{JClass, JDoubleArray, JIntArray, JLongArray, JString};
use jni::sys::{jboolean, jbyteArray, jint, jlong, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use rr_ffi::{RrEvent, RrOptions, RrSession, RrStats};
use std::ffi::{c_char, CStr, CString};
use std::ptr::{null, null_mut};

/// `connect`'s flags (Kotlin's `Native.FLAG_*`).
pub const FLAG_WEBTRANSPORT: i32 = 1;
pub const FLAG_DATAGRAMS: i32 = 2;
pub const FLAG_DIRECT: i32 = 4;
pub const FLAG_AUDIO: i32 = 8;
pub const FLAG_GAMEPAD: i32 = 16;
pub const FLAG_HARDWARE: i32 = 32;

/// The options for `connect`'s flags, codecs (bits: 1 << `RR_CODEC_*`) and refresh rate (mHz).
pub fn options(flags: i32, codecs: i32, refresh_mhz: i32) -> RrOptions {
    let on = |f: i32| flags & f != 0;
    RrOptions {
        webtransport: on(FLAG_WEBTRANSPORT),
        datagrams: on(FLAG_DATAGRAMS),
        direct: on(FLAG_DIRECT),
        audio: on(FLAG_AUDIO),
        gamepad: on(FLAG_GAMEPAD),
        codecs: codecs as u32,
        hardware: on(FLAG_HARDWARE),
        refresh_mhz: if refresh_mhz > 0 { refresh_mhz as u32 } else { 60_000 },
    }
}

/// How many numbers an event has in `fields` (Kotlin's `Native.FIELDS`).
pub const FIELDS: usize = 23;

/// An event's numbers, in the order Kotlin reads them: kind, codec, width, height, fps, bitrate_kbps,
/// frame, keyframe, timestamp_us, received_us, has_latency, latency_us, x, y, visible, has_shape,
/// shape_width, shape_height, hot_x, hot_y, server_input, server_audio, and whether the bytes are text.
pub fn event_fields(e: &RrEvent) -> [i64; FIELDS] {
    let b = |v: bool| v as i64;
    [
        e.kind as i64,
        e.codec as i64,
        e.width as i64,
        e.height as i64,
        e.fps as i64,
        e.bitrate_kbps as i64,
        e.frame as i64,
        b(e.keyframe),
        e.timestamp_us as i64,
        e.received_us as i64,
        b(e.has_latency),
        e.latency_us,
        e.x as i64,
        e.y as i64,
        b(e.visible),
        b(e.has_shape),
        e.shape_width as i64,
        e.shape_height as i64,
        e.hot_x as i64,
        e.hot_y as i64,
        e.server_input as i64,
        b(e.server_audio),
        b(e.data.is_null() && !e.text.is_null()),
    ]
}

/// The event's bytes, or its text's.
///
/// # Safety
/// `e`'s pointers are its own (an event from `rr_next_event`, not yet freed).
pub unsafe fn event_bytes(e: &RrEvent) -> Option<&[u8]> {
    if !e.data.is_null() {
        Some(std::slice::from_raw_parts(e.data, e.len))
    } else if !e.text.is_null() {
        Some(CStr::from_ptr(e.text).to_bytes())
    } else {
        None
    }
}

/// How many numbers `stats` fills (Kotlin's `Native.STATS`).
pub const STATS: usize = 11;

/// The session's numbers: frames, keyframes, bytes, frames_lost, fec_recovered, fec_lost,
/// loss_permille, rtt_ms, rtt_min_ms, has_clock_offset, clock_offset_us (exact as doubles: all under
/// 2⁵³).
pub fn stats_values(s: &RrStats) -> [f64; STATS] {
    [
        s.frames as f64,
        s.keyframes as f64,
        s.bytes as f64,
        s.frames_lost as f64,
        s.fec_recovered as f64,
        s.fec_lost as f64,
        s.loss_permille as f64,
        s.rtt_ms,
        s.rtt_min_ms,
        if s.has_clock_offset { 1.0 } else { 0.0 },
        s.clock_offset_us as f64,
    ]
}

/// A Java string as a C one (NULL stays None; a NUL inside becomes a space).
fn cstr(env: &mut JNIEnv, s: &JString) -> Option<CString> {
    if s.is_null() {
        return None;
    }
    let s: String = env.get_string(s).ok()?.into();
    CString::new(s.replace('\0', " ")).ok()
}

/// A string the core returned, freed.
///
/// # Safety
/// `p` is NULL or a string from rr-ffi, not yet freed.
unsafe fn take(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    rr_ffi::rr_string_free(p);
    Some(s)
}

fn throw(env: &mut JNIEnv, msg: &str) {
    let _ = env.throw_new("java/io/IOException", msg);
}

fn jstring_of(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s).map_or(null_mut(), |s| s.into_raw())
}

fn session(h: jlong) -> *const RrSession {
    h as usize as *const RrSession
}

fn opt_ptr(s: &Option<CString>) -> *const c_char {
    s.as_ref().map_or(null(), |s| s.as_ptr())
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_setHome(mut env: JNIEnv, _: JClass, path: JString) -> jboolean {
    let Some(p) = cstr(&mut env, &path) else { return JNI_FALSE };
    if unsafe { rr_ffi::rr_set_home(p.as_ptr()) } {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_version(mut env: JNIEnv, _: JClass) -> jstring {
    let v = unsafe { CStr::from_ptr(rr_ffi::rr_version()) }.to_string_lossy().into_owned();
    jstring_of(&mut env, &v)
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_nowUs(_: JNIEnv, _: JClass) -> jlong {
    rr_ffi::rr_now_us() as jlong
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_reinharness_remote_Native_connect(
    mut env: JNIEnv,
    _: JClass,
    target: JString,
    name: JString,
    pair_code: JString,
    flags: jint,
    codecs: jint,
    refresh_mhz: jint,
) -> jlong {
    let Some(target) = cstr(&mut env, &target) else {
        throw(&mut env, "no link to connect to");
        return 0;
    };
    let name = cstr(&mut env, &name);
    let pair = cstr(&mut env, &pair_code);
    let opts = options(flags, codecs, refresh_mhz);
    let mut err: *mut c_char = null_mut();
    let s = unsafe { rr_ffi::rr_connect(target.as_ptr(), opt_ptr(&name), opt_ptr(&pair), &opts, &mut err) };
    let why = unsafe { take(err) };
    if s.is_null() {
        throw(&mut env, why.as_deref().unwrap_or("couldn't connect"));
        return 0;
    }
    s as usize as jlong
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_nextEvent(env: JNIEnv, _: JClass, h: jlong, wait_ms: jint, fields: JLongArray) -> jbyteArray {
    let e = unsafe { rr_ffi::rr_next_event(session(h), wait_ms.max(0) as u32) };
    if e.is_null() {
        let _ = env.set_long_array_region(&fields, 0, &[0]);
        return null_mut();
    }
    let ev = unsafe { &*e };
    let _ = env.set_long_array_region(&fields, 0, &event_fields(ev));
    let out = unsafe { event_bytes(ev) }.map_or(null_mut(), |b| env.byte_array_from_slice(b).map_or(null_mut(), |a| a.into_raw()));
    unsafe { rr_ffi::rr_event_free(e) };
    out
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_pointerMove(_: JNIEnv, _: JClass, h: jlong, x: jint, y: jint) {
    unsafe { rr_ffi::rr_pointer_move(session(h), x.clamp(0, 65535) as u16, y.clamp(0, 65535) as u16) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_pointerDelta(_: JNIEnv, _: JClass, h: jlong, dx: jint, dy: jint) {
    unsafe { rr_ffi::rr_pointer_delta(session(h), i16c(dx), i16c(dy)) }
}

fn i16c(v: jint) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_button(_: JNIEnv, _: JClass, h: jlong, button: jint, down: jboolean) {
    unsafe { rr_ffi::rr_button(session(h), button.clamp(0, 255) as u8, down != JNI_FALSE) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_wheel(_: JNIEnv, _: JClass, h: jlong, dx: jint, dy: jint) {
    unsafe { rr_ffi::rr_wheel(session(h), i16c(dx), i16c(dy)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_key(mut env: JNIEnv, _: JClass, h: jlong, hid: jint, down: jboolean, name: JString) {
    let name = cstr(&mut env, &name);
    unsafe { rr_ffi::rr_key(session(h), hid.clamp(0, 65535) as u16, down != JNI_FALSE, opt_ptr(&name)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_text(mut env: JNIEnv, _: JClass, h: jlong, text: JString) {
    let text = cstr(&mut env, &text);
    unsafe { rr_ffi::rr_text(session(h), opt_ptr(&text)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_touch(mut env: JNIEnv, _: JClass, h: jlong, ids: JIntArray, xs: JIntArray, ys: JIntArray) {
    let read = |env: &mut JNIEnv, a: &JIntArray| -> Vec<i32> {
        let n = env.get_array_length(a).unwrap_or(0).max(0) as usize;
        let mut v = vec![0; n];
        if env.get_int_array_region(a, 0, &mut v).is_err() {
            v.clear();
        }
        v
    };
    let (ids, xs, ys) = (read(&mut env, &ids), read(&mut env, &xs), read(&mut env, &ys));
    let n = ids.len().min(xs.len()).min(ys.len());
    let ids: Vec<u8> = ids[..n].iter().map(|v| *v as u8).collect();
    let xs: Vec<u16> = xs[..n].iter().map(|v| (*v).clamp(0, 65535) as u16).collect();
    let ys: Vec<u16> = ys[..n].iter().map(|v| (*v).clamp(0, 65535) as u16).collect();
    unsafe { rr_ffi::rr_touch(session(h), ids.as_ptr(), xs.as_ptr(), ys.as_ptr(), n) }
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_reinharness_remote_Native_gamepad(
    _: JNIEnv,
    _: JClass,
    h: jlong,
    pad: jint,
    buttons: jint,
    lx: jint,
    ly: jint,
    rx: jint,
    ry: jint,
    lt: jint,
    rt: jint,
) {
    let u8c = |v: jint| v.clamp(0, 255) as u8;
    unsafe { rr_ffi::rr_gamepad(session(h), u8c(pad), buttons as u32, i16c(lx), i16c(ly), i16c(rx), i16c(ry), u8c(lt), u8c(rt)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_clipboard(mut env: JNIEnv, _: JClass, h: jlong, text: JString) {
    let text = cstr(&mut env, &text);
    unsafe { rr_ffi::rr_clipboard(session(h), opt_ptr(&text)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_resize(_: JNIEnv, _: JClass, h: jlong, w: jint, hh: jint) {
    unsafe { rr_ffi::rr_resize(session(h), w.clamp(0, 65535) as u16, hh.clamp(0, 65535) as u16) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_keyframe(_: JNIEnv, _: JClass, h: jlong) {
    unsafe { rr_ffi::rr_keyframe(session(h)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_feedback(
    _: JNIEnv,
    _: JClass,
    h: jlong,
    frame: jint,
    received_us: jlong,
    decoded_us: jlong,
    lost: jint,
) {
    unsafe { rr_ffi::rr_feedback(session(h), frame as u32, received_us as u64, decoded_us as u64, lost.clamp(0, 65535) as u16) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_stats(mut env: JNIEnv, _: JClass, h: jlong, out: JDoubleArray) -> jstring {
    let mut s = RrStats {
        frames: 0,
        keyframes: 0,
        bytes: 0,
        frames_lost: 0,
        fec_recovered: 0,
        fec_lost: 0,
        loss_permille: 0,
        rtt_ms: 0.0,
        rtt_min_ms: 0.0,
        has_clock_offset: false,
        clock_offset_us: 0,
        transport: null(),
    };
    unsafe { rr_ffi::rr_stats(session(h), &mut s) };
    let _ = env.set_double_array_region(&out, 0, &stats_values(&s));
    let t = if s.transport.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(s.transport) }.to_string_lossy().into_owned()
    };
    jstring_of(&mut env, &t)
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_close(_: JNIEnv, _: JClass, h: jlong) {
    unsafe { rr_ffi::rr_close(session(h)) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_free(_: JNIEnv, _: JClass, h: jlong) {
    unsafe { rr_ffi::rr_free(session(h) as *mut RrSession) }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_pair(mut env: JNIEnv, _: JClass, url: JString, code: JString, name: JString) -> jstring {
    let (url, code, name) = (cstr(&mut env, &url), cstr(&mut env, &code), cstr(&mut env, &name));
    let mut err: *mut c_char = null_mut();
    let id = unsafe { take(rr_ffi::rr_pair(opt_ptr(&url), opt_ptr(&code), opt_ptr(&name), &mut err)) };
    let why = unsafe { take(err) };
    match id {
        Some(id) => jstring_of(&mut env, &id),
        None => {
            throw(&mut env, why.as_deref().unwrap_or("pairing failed"));
            null_mut()
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_isPaired(mut env: JNIEnv, _: JClass, id: JString) -> jboolean {
    let id = cstr(&mut env, &id);
    if unsafe { rr_ffi::rr_is_paired(opt_ptr(&id)) } {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_reinharness_remote_Native_discover(mut env: JNIEnv, _: JClass, wait_ms: jint) -> jstring {
    let mut err: *mut c_char = null_mut();
    let found = unsafe { take(rr_ffi::rr_discover(wait_ms.max(0) as u32, &mut err)) };
    let why = unsafe { take(err) };
    match found {
        Some(j) => jstring_of(&mut env, &j),
        None => {
            throw(&mut env, why.as_deref().unwrap_or("discovery failed"));
            null_mut()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_make_options() {
        let o = options(FLAG_WEBTRANSPORT | FLAG_AUDIO | FLAG_HARDWARE, (1 << 1) | (1 << 2), 0);
        assert!(o.webtransport && o.audio && o.hardware);
        assert!(!o.datagrams && !o.direct && !o.gamepad);
        assert_eq!((o.codecs, o.refresh_mhz), (6, 60_000));
        assert_eq!(options(0, 0, 120_000).refresh_mhz, 120_000);
    }

    /// A real event (through rr-ffi's own conversion would need a session; this one is made by hand)
    /// read the way Kotlin reads it.
    #[test]
    fn events_as_kotlin_reads_them() {
        let text = CString::new("box").unwrap();
        let data = [1u8, 2, 3];
        let mut e = RrEvent {
            kind: rr_ffi::RR_EVENT_VIDEO,
            codec: 1,
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_kbps: 8000,
            frame: 42,
            keyframe: true,
            timestamp_us: 1_700_000_000_000_000,
            received_us: 1_700_000_000_005_000,
            has_latency: true,
            latency_us: -7,
            x: 10,
            y: 20,
            visible: true,
            has_shape: false,
            shape_width: 0,
            shape_height: 0,
            hot_x: 0,
            hot_y: 0,
            server_input: 15,
            server_audio: true,
            data: data.as_ptr(),
            len: 3,
            text: null(),
        };
        let f = event_fields(&e);
        assert_eq!(f.len(), FIELDS);
        assert_eq!((f[0], f[2], f[3], f[6], f[7]), (3, 1280, 720, 42, 1));
        assert_eq!((f[8], f[9], f[10], f[11]), (1_700_000_000_000_000, 1_700_000_000_005_000, 1, -7));
        assert_eq!((f[20], f[21], f[22]), (15, 1, 0));
        assert_eq!(unsafe { event_bytes(&e) }, Some(&data[..]));
        e.data = null();
        e.len = 0;
        e.text = text.as_ptr();
        assert_eq!(event_fields(&e)[22], 1);
        assert_eq!(unsafe { event_bytes(&e) }, Some(&b"box"[..]));
        e.text = null();
        assert_eq!(unsafe { event_bytes(&e) }, None);
    }

    #[test]
    fn stats_as_doubles() {
        let s = RrStats {
            frames: 5,
            keyframes: 1,
            bytes: 1000,
            frames_lost: 2,
            fec_recovered: 3,
            fec_lost: 0,
            loss_permille: 4,
            rtt_ms: 1.5,
            rtt_min_ms: 1.0,
            has_clock_offset: true,
            clock_offset_us: -1234,
            transport: null(),
        };
        let v = stats_values(&s);
        assert_eq!(v.len(), STATS);
        assert_eq!((v[0], v[7], v[9], v[10]), (5.0, 1.5, 1.0, -1234.0));
    }
}
