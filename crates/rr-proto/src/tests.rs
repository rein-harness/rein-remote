use super::*;

fn round_trip(m: Message) {
    let bytes = m.encode();
    let (back, used) = Message::decode(&bytes).expect("decodes");
    assert_eq!(used, bytes.len());
    assert_eq!(back, Some(m));
}

fn hello(codecs: &[(Codec, bool)]) -> Hello {
    Hello {
        version_min: 0,
        version_max: 0,
        name: "build-box".into(),
        codecs: codecs.iter().map(|&(codec, hardware)| CodecCap { codec, hardware }).collect(),
        max_width: 3840,
        max_height: 2160,
        max_fps: 120,
        audio: true,
        input: INPUT_POINTER | INPUT_KEYBOARD | INPUT_TOUCH,
        refresh_mhz: 120_000,
    }
}

#[test]
fn every_message_round_trips() {
    round_trip(Message::Hello(hello(&[(Codec::H264, true), (Codec::Av1, false)])));
    round_trip(Message::Config(Config {
        codec: Codec::H265,
        width: 2560,
        height: 1440,
        fps: 60,
        bitrate_kbps: 25_000,
    }));
    round_trip(Message::Video(Video {
        keyframe: true,
        frame: 7,
        timestamp_us: 1_234_567,
        data: vec![0, 0, 0, 1, 0x67, 0x42],
    }));
    round_trip(Message::Video(Video {
        keyframe: false,
        frame: 8,
        timestamp_us: 1_250_000,
        data: vec![],
    }));
    round_trip(Message::Audio(Audio {
        timestamp_us: 9,
        data: vec![1, 2, 3],
    }));
    round_trip(Message::Cursor(Cursor {
        x: 10,
        y: 20,
        visible: true,
        shape: Some(CursorShape {
            w: 2,
            h: 1,
            hx: 0,
            hy: 0,
            rgba: vec![255; 8],
        }),
    }));
    round_trip(Message::Cursor(Cursor {
        x: 1,
        y: 2,
        visible: false,
        shape: None,
    }));
    for input in [
        Input::PointerMove { x: 100, y: 200 },
        Input::PointerDelta { dx: -5, dy: 12 },
        Input::Button {
            button: Button::Right,
            down: true,
        },
        Input::Wheel { dx: 0, dy: -120 },
        Input::Key {
            hid: 0x04,
            down: true,
            key: "a".into(),
        },
        Input::Text { text: "héllo 👋".into() },
        Input::Touch {
            points: vec![(0, 10, 20), (1, 30, 40)],
        },
        Input::Touch { points: vec![] },
        Input::Gamepad {
            pad: 0,
            buttons: 0b1010,
            lx: -32768,
            ly: 32767,
            rx: 0,
            ry: 1,
            lt: 255,
            rt: 0,
        },
    ] {
        round_trip(Message::Input(input));
    }
    round_trip(Message::Feedback(Feedback {
        frame: 3,
        received_us: 10,
        decoded_us: 12,
        lost: 1,
    }));
    round_trip(Message::KeyframeRequest);
    round_trip(Message::Clipboard { text: "copied".into() });
    round_trip(Message::Resize { width: 1280, height: 720 });
    round_trip(Message::Ping { id: 1, sent_us: 99 });
    round_trip(Message::Pong { id: 1, sent_us: 99 });
}

#[test]
fn keyframes_are_flagged_in_the_header() {
    // A sender or relay that's backed up can skip to the next keyframe without parsing bodies.
    let key = Message::Video(Video {
        keyframe: true,
        frame: 1,
        timestamp_us: 0,
        data: vec![1],
    })
    .encode();
    let delta = Message::Video(Video {
        keyframe: false,
        frame: 2,
        timestamp_us: 0,
        data: vec![1],
    })
    .encode();
    let (hk, hd) = (Header::read(&key).unwrap(), Header::read(&delta).unwrap());
    assert_eq!((hk.kind, hk.version, hk.flags & FLAG_KEYFRAME), (kind::VIDEO, version::VIDEO, FLAG_KEYFRAME));
    assert_eq!(hd.flags & FLAG_KEYFRAME, 0);
}

#[test]
fn partial_frames_wait_for_more_and_streams_split_into_frames() {
    let a = Message::Ping { id: 1, sent_us: 2 }.encode();
    let b = Message::Clipboard { text: "x".into() }.encode();
    let stream = [a.clone(), b.clone()].concat();
    assert_eq!(Message::decode(&stream[..3]), Err(Error::Incomplete));
    assert_eq!(Message::decode(&stream[..a.len() - 1]), Err(Error::Incomplete));
    let (m1, n1) = Message::decode(&stream).unwrap();
    let (m2, n2) = Message::decode(&stream[n1..]).unwrap();
    assert_eq!(
        (m1, m2, n1 + n2),
        (
            Some(Message::Ping { id: 1, sent_us: 2 }),
            Some(Message::Clipboard { text: "x".into() }),
            stream.len()
        )
    );
}

#[test]
fn unknown_kinds_are_skipped_and_bad_frames_refused() {
    // A newer peer's message: skipped by its length, the next frame still reads.
    let mut stream = vec![200, 0, 0, 3, 0, 0, 0, 1, 2, 3];
    stream.extend(Message::KeyframeRequest.encode());
    let (m, n) = Message::decode(&stream).unwrap();
    assert_eq!((m, n), (None, 10));
    assert_eq!(Message::decode(&stream[n..]).unwrap().0, Some(Message::KeyframeRequest));
    // Too large, truncated body, bad enum values, bad UTF-8 (headers: kind, version, flags, len).
    assert_eq!(
        Message::decode(&[kind::VIDEO, 0, 0, 0xff, 0xff, 0xff, 0xff]),
        Err(Error::TooLarge(u32::MAX as usize))
    );
    assert!(matches!(Message::decode(&[kind::RESIZE, 0, 0, 1, 0, 0, 0, 5]), Err(Error::Malformed(_))));
    assert!(matches!(
        Message::decode(&[kind::CONFIG, 0, 0, 11, 0, 0, 0, 99, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(Error::Malformed("codec"))
    ));
    assert!(matches!(
        Message::decode(&[kind::CLIPBOARD, 0, 0, 3, 0, 0, 0, 1, 0, 0xff]),
        Err(Error::Malformed("utf-8"))
    ));
    // A video frame whose data claims more bytes than the body has.
    assert!(matches!(
        Message::decode(&[kind::VIDEO, 0, 0, 16, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0]),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn versions_and_codecs_are_negotiated() {
    let server = hello(&[(Codec::H265, true), (Codec::H264, true), (Codec::Vp9, false)]);
    // A phone that decodes H.264 in hardware, VP9 only in software: H.264 wins over VP9.
    let phone = hello(&[(Codec::Vp9, false), (Codec::H264, true)]);
    assert_eq!(pick_codec(&server, &phone), Some(Codec::H264));
    // A client with only software VP9: VP9.
    assert_eq!(pick_codec(&server, &hello(&[(Codec::Vp9, false)])), Some(Codec::Vp9));
    assert_eq!(pick_codec(&server, &hello(&[(Codec::Av1, true)])), None);
    let newer = Hello {
        version_min: 0,
        version_max: 3,
        ..phone.clone()
    };
    assert_eq!(negotiate(&server, &newer), Some(0));
    assert_eq!(
        negotiate(
            &server,
            &Hello {
                version_min: 2,
                version_max: 3,
                ..phone
            }
        ),
        None
    );
}

#[test]
fn long_strings_are_cut_on_a_character_boundary() {
    let long = "é".repeat(40_000); // 80,000 bytes
    let bytes = Message::Clipboard { text: long }.encode();
    let (m, _) = Message::decode(&bytes).unwrap();
    let Some(Message::Clipboard { text }) = m else { panic!() };
    assert!(text.len() <= u16::MAX as usize && text.chars().all(|c| c == 'é'));
}

/// A frame as a peer of another version might write it: this build's body, with `extra` bytes after
/// it (fields a newer version added), under header version `v`.
fn frame_from(m: &Message, v: u8, extra: &[u8]) -> Vec<u8> {
    let mut f = m.encode();
    f[1] = v;
    f.extend_from_slice(extra);
    let len = (f.len() - HEADER_LEN) as u32;
    f[3..7].copy_from_slice(&len.to_le_bytes());
    f
}

#[test]
fn a_newer_peers_messages_still_read() {
    // A future version added fields at the end of Config, Video, Hello and an input event: this
    // build reads what it knows and ignores the rest.
    for m in [
        Message::Config(Config {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 8000,
        }),
        Message::Video(Video {
            keyframe: true,
            frame: 1,
            timestamp_us: 5,
            data: vec![1, 2, 3],
        }),
        Message::Hello(hello(&[(Codec::H264, true)])),
        Message::Input(Input::Key {
            hid: 4,
            down: true,
            key: "a".into(),
        }),
        Message::Cursor(Cursor {
            x: 1,
            y: 2,
            visible: true,
            shape: Some(CursorShape {
                w: 1,
                h: 1,
                hx: 0,
                hy: 0,
                rgba: vec![9; 4],
            }),
        }),
    ] {
        let f = frame_from(&m, 7, &[0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(Message::decode(&f), Ok((Some(m), f.len())));
    }
    // A newer codec in Hello is left out; a newer input kind or button is skipped, not an error.
    let mut h = Message::Hello(hello(&[(Codec::H264, true)])).encode();
    let at = HEADER_LEN + 2 + 2 + 2 + "build-box".len();
    h[at] = 2; // two codecs: H.264 and an unknown one
    h.splice(at + 3..at + 3, [77, 1]);
    let len = (h.len() - HEADER_LEN) as u32;
    h[3..7].copy_from_slice(&len.to_le_bytes());
    let Ok((Some(Message::Hello(read)), _)) = Message::decode(&h) else { panic!() };
    assert_eq!(
        read.codecs,
        vec![CodecCap {
            codec: Codec::H264,
            hardware: true
        }]
    );
    assert_eq!(Message::decode(&[kind::INPUT, 3, 0, 1, 0, 0, 0, 99]), Ok((None, 8)));
    assert_eq!(Message::decode(&[kind::INPUT, 3, 0, 3, 0, 0, 0, 3, 42, 1]), Ok((None, 10)));
}

#[test]
fn an_older_peers_messages_get_defaults_for_fields_added_since() {
    // Today's Ping is (id, sent_us). Suppose a later version adds `seq: u32` at the end: it reads
    // it with `or_u32`, and an older peer's frame (which stops after sent_us) gets the default.
    let old = Message::Ping { id: 3, sent_us: 9 }.encode();
    let mut r = Reader::new(&old[HEADER_LEN..]);
    assert_eq!((r.u32().unwrap(), r.u64().unwrap()), (3, 9));
    assert_eq!(r.or_u32(0).unwrap(), 0);
    assert_eq!(r.or_str("none").unwrap(), "none");
    // And from a newer peer that sent it, the value.
    let newer = frame_from(&Message::Ping { id: 3, sent_us: 9 }, 1, &42u32.to_le_bytes());
    let mut r = Reader::new(&newer[HEADER_LEN..]);
    r.u32().unwrap();
    r.u64().unwrap();
    assert_eq!(r.or_u32(0).unwrap(), 42);
    // Hello version 0 had no refresh rate: it reads as unknown.
    let mut v0 = Message::Hello(hello(&[(Codec::H264, true)])).encode();
    v0.truncate(v0.len() - 4);
    v0[1] = 0;
    let len = (v0.len() - HEADER_LEN) as u32;
    v0[3..7].copy_from_slice(&len.to_le_bytes());
    let Ok((Some(Message::Hello(read)), _)) = Message::decode(&v0) else {
        panic!()
    };
    assert_eq!((read.refresh_mhz, read.max_fps), (0, 120));
    // Every kind has a version this build writes, in the header.
    for m in [Message::KeyframeRequest, Message::Resize { width: 1, height: 1 }] {
        let f = m.encode();
        assert_eq!(Header::read(&f).unwrap().version, version::of(f[0]));
    }
}
