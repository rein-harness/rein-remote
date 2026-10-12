//! A relay fleet on this machine: two relays, a server with its room open on both, a viewer through
//! the one its link names. That relay dies (killed, as a crash would): the viewer, with the same link,
//! is back on the other within seconds; the relay comes back: the server is in its room there again.
//! Health and metrics along the way: what a load balancer and Prometheus read, and nothing secret.

mod common;
use common::{frames, get, Proc};
use rr_client::{connect, Event, Options};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(30);

fn relay(listen: &str) -> (Proc, String, String) {
    let mut p = Proc::start(&["relay", "--listen", listen, "--metrics-listen", "127.0.0.1:0"], &[]);
    let at = p.listening();
    let m = p.wait_for("metrics on http://", T);
    let metrics = m.split("http://").nth(1).unwrap().trim_end_matches("/metrics").to_string();
    (p, at, metrics)
}

/// Waits until `f` holds (polled), or panics saying `what`.
fn eventually(what: &str, mut f: impl FnMut() -> bool) -> Duration {
    let t = Instant::now();
    while t.elapsed() < T {
        if f() {
            return t.elapsed();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{what}: not within {T:?}");
}

fn waiting(addr: &str) -> bool {
    let (status, body) = get(addr, "/healthz", "");
    status == 200 && body.contains(r#""hosts_waiting":1"#)
}

#[test]
fn a_relay_dies_and_the_viewer_carries_on_through_the_other() {
    let (mut a, a_at, a_metrics) = relay("127.0.0.1:0");
    let (_b, b_at, b_metrics) = relay("127.0.0.1:0");
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
            "--webtransport",
            "off",
            "--relay",
            &format!("ws://{a_at}"),
            "--relay",
            &format!("ws://{b_at}"),
        ],
        &[],
    );
    let addr = server.listening();
    let line = server.wait_for("through the relay: open ", T);
    let link = line.split("open ").nth(1).unwrap().trim().to_string();
    // The key and the other relay after #, where no relay sees them.
    let (before, after) = link.split_once('#').unwrap();
    assert!(after.starts_with("k=") && after.contains("&alt=ws://"), "{link}");
    assert!(!before.contains("k="), "{link}");
    let first = rr_client::http::parse(&link).unwrap().origin.authority();
    let (first_at, other_at) = if first == a_at { (&a_at, &b_at) } else { (&b_at, &a_at) };
    // The room open on both.
    eventually("the room open on both relays", || waiting(&a_at) && waiting(&b_at));

    // The server's health and metrics: with the token only (in the query, or as a bearer).
    assert_eq!(get(&addr, "/healthz", "").0, 403);
    assert_eq!(get(&addr, "/metrics", "Authorization: Bearer nope\r\n").0, 403);
    let (status, health) = get(&addr, "/healthz?token=tok", "");
    assert_eq!(status, 200, "{health}");
    assert!(health.contains(r#""relays":2,"relays_up":2"#), "{health}");
    let (status, metrics) = get(&addr, "/metrics", "Authorization: Bearer tok\r\n");
    assert_eq!(status, 200);
    assert!(metrics.contains(&format!("rein_remote_relay_registered{{relay=\"{a_at}\"}} 1")), "{metrics}");

    let opts = || Options {
        direct: false,
        webtransport: false,
        ..Default::default()
    };
    let s = connect(&link, opts()).unwrap();
    assert!(frames(&s, 10, T), "frames through the first relay");
    assert_eq!(s.stats().transport, "relay");
    // The relay's metrics: a session, bytes through it, and no room code or key anywhere.
    let busy = if first_at == &a_at { &a_metrics } else { &b_metrics };
    let (_, m) = get(busy, "/metrics", "");
    assert!(m.contains("rein_remote_relay_sessions 1"), "{m}");
    let room = rr_client::http::param(&rr_client::http::parse(&link).unwrap().query, "room")
        .unwrap()
        .to_string();
    let key = rr_client::http::param(after, "k").unwrap();
    for text in [&m, &get(first_at, "/healthz", "").1, &metrics] {
        assert!(!text.contains(&room[..6]) && !text.contains(key) && !text.contains("tok\""), "{text}");
    }
    let (_, h) = get(&addr, "/metrics?token=tok", "");
    assert!(h.contains("rein_remote_viewers{path=\"relay\"} 1"), "{h}");

    // The first relay dies.
    let killed = Instant::now();
    if first_at == &a_at {
        a.kill();
    } else {
        // Kill b instead (the one the link names); a stays.
        drop(_b);
    }
    let mut closed = false;
    while killed.elapsed() < T && !closed {
        closed = matches!(s.next_event(Duration::from_millis(100)), Some(Event::Closed(_)));
    }
    assert!(closed, "the session through the dead relay ends");
    let noticed = killed.elapsed();
    // The same link again: the dead relay refuses at once, the other has the room waiting.
    let s = connect(&link, opts()).unwrap();
    assert!(frames(&s, 1, T), "a frame through the other relay");
    let back = killed.elapsed();
    assert!(frames(&s, 10, T), "frames through the other relay");
    eprintln!(
        "fleet: the viewer saw the relay go in {} ms, and had a frame through {other_at} {} ms after it died",
        noticed.as_millis(),
        back.as_millis()
    );
    assert!(back < Duration::from_secs(10), "back within 10 s: {back:?}");

    // The dead relay comes back (same address): the server is in its room there again.
    let restarted = Instant::now();
    let (_again, again_at, _) = relay(first_at);
    assert_eq!(&again_at, first_at);
    eventually("the server back in its room on the restarted relay", || waiting(first_at));
    let reregistered = restarted.elapsed();
    server.wait_for(&format!("back in the room on the relay {first_at}"), T);
    eprintln!(
        "fleet: the server was back in its room on the restarted relay {} ms after it was started again",
        reregistered.as_millis()
    );
    assert!(reregistered < Duration::from_secs(10), "{reregistered:?}");
}
