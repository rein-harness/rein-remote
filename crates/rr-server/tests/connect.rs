//! rr-client against a real `rein-remote serve` on this machine: over the WebSocket, over WebTransport
//! (datagrams with parity, and a stream a frame), and through a relay, from where the session punches
//! its way to a direct path (on one machine there's no NAT to punch through, but every step runs: the
//! probe over UDP, the sealed offer and answer, punch packets, WebTransport with the offer's certificate
//! hash and one-time ticket).

mod common;
use common::{frames, until, Proc};
use rr_client::{connect, Event, Options};
use std::time::Duration;

const T: Duration = Duration::from_secs(30);

fn serve(extra: &[&str]) -> (Proc, String) {
    let mut args = vec![
        "serve",
        "--test-pattern",
        "320x240",
        "--listen",
        "127.0.0.1:0",
        "--token",
        "tok",
        "--mdns",
        "off",
    ];
    args.extend_from_slice(extra);
    let mut p = Proc::start(&args, &[]);
    let addr = p.listening();
    (p, addr)
}

#[test]
fn streams_over_the_websocket() {
    let (_server, addr) = serve(&[]);
    let opts = Options {
        webtransport: false,
        ..Default::default()
    };
    let s = connect(&format!("http://{addr}/?token=tok"), opts).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Config(c) if c.width == 320)), "a config");
    assert!(frames(&s, 10, T), "frames");
    assert_eq!(s.stats().transport, "websocket");
    // Its pings come back, and it sends feedback the server reads (no error, the stream goes on).
    assert!(until(&s, T, |_| s.stats().rtt_ms > 0.0));
    // A wrong token: refused.
    assert!(connect(&format!("ws://{addr}/stream?token=nope"), Options::default()).is_err());
}

#[test]
fn streams_over_webtransport_datagrams_and_streams() {
    let (_server, addr) = serve(&[]);
    let s = connect(&format!("http://{addr}/?token=tok"), Options::default()).unwrap();
    assert!(frames(&s, 10, T), "frames");
    assert_eq!(s.stats().transport, "webtransport-datagrams");
    let opts = Options {
        datagrams: false,
        ..Default::default()
    };
    let s = connect(&format!("http://{addr}/?token=tok"), opts).unwrap();
    assert!(frames(&s, 10, T), "frames");
    assert_eq!(s.stats().transport, "webtransport-streams");
}

#[test]
fn through_the_relay_then_straight_there() {
    let mut relay = Proc::start(&["relay", "--listen", "127.0.0.1:0"], &[]);
    let at = relay.listening();
    let (mut server, _) = serve(&["--relay", &format!("ws://{at}")]);
    let line = server.wait_for("through the relay: open ", T);
    let link = line.split("open ").nth(1).unwrap().trim().to_string();
    server.wait_for("the relay sees WebTransport's socket as", T);
    // The host waits in its room as long as it takes: a viewer more than 10 s later still gets in.
    std::thread::sleep(Duration::from_secs(11));

    // The relay answers probes for its open rooms only (the room is in the link).
    let room = rr_client::http::param(&rr_client::http::parse(&link).unwrap().query, "room")
        .unwrap()
        .to_string();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let reflector = at.parse().unwrap();
    assert_eq!(rr_client::direct::probe(&udp, reflector, &room, T), Some(udp.local_addr().unwrap()));
    assert_eq!(rr_client::direct::probe(&udp, reflector, "not-a-room-anyone-has", Duration::from_secs(1)), None);

    // Relayed (sealed end to end), staying there when asked to.
    let opts = Options {
        direct: false,
        ..Default::default()
    };
    let s = connect(&link, opts).unwrap();
    assert!(frames(&s, 10, T), "frames through the relay");
    assert_eq!(s.stats().transport, "relay");
    s.close();
    drop(s);

    // A wrong key: nothing opens.
    let wrong = format!("{}#k={}", link.split('#').next().unwrap(), rr_client::e2e::b64url(&[7; 32]));
    let s = connect(&wrong, Options::default()).unwrap();
    let mut closed = false;
    for _ in 0..100 {
        match s.next_event(Duration::from_millis(300)) {
            Some(Event::Closed(_)) => {
                closed = true;
                break;
            }
            Some(Event::Video { .. }) => panic!("a frame opened with the wrong key"),
            _ => {}
        }
    }
    assert!(closed, "a wrong key ends the session");
    drop(s);

    // Through the relay, then the direct path.
    let s = connect(&link, Options::default()).unwrap();
    assert!(until(&s, T, |e| matches!(e, Event::Transport("direct-after-punch"))), "went direct");
    assert!(frames(&s, 10, T), "frames on the direct path");
    assert_eq!(s.stats().transport, "direct-after-punch");
    server.wait_for("a viewer came straight here", T);
}
