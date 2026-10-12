//! A direct path through NAT, when a session came through a relay: both sides learn the address their
//! QUIC socket has on the far side of their NAT (asking the relay over UDP from that very socket),
//! swap candidates inside the sealed session, send each other punch packets (each NAT then lets the
//! other's packets in, as answers to its own), and the viewer opens WebTransport straight to the host
//! from the same socket. The host and native viewers share what's here; the packets are rr-proto's
//! `punch`.
//!
//! It works through the NATs most homes and phones have (the same outside address and port whoever
//! the socket talks to). Through a NAT that gives each destination its own port (symmetric, some
//! carrier-grade NATs), what the relay saw isn't what the other side would reach, and the session stays
//! on the relay. Browsers can't do any of this (no UDP of their own): they stay on the relay.

use rr_proto::punch::{self, Packet};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// Where a relay answers probes: the host and port of its link (`ws://relay:7990` → relay:7990 over
/// UDP; `wss://relay.example` → port 443). Behind an HTTPS proxy that doesn't pass UDP, there's none to
/// be had there and sessions stay relayed. IPv4 first (the sockets here are IPv4).
pub fn reflector(relay: &str) -> Option<SocketAddr> {
    let u = crate::http::parse(relay).ok()?;
    let mut all: Vec<SocketAddr> = (u.origin.bare_host(), u.origin.port).to_socket_addrs().ok()?.collect();
    all.sort_by_key(|a| !a.is_ipv4());
    all.into_iter().next()
}

/// The address `socket` has as `reflector` sees it, asked for `room` (the relay answers only for a room
/// it has open). A few tries over `wait`. The socket's read timeout is changed.
pub fn probe(socket: &UdpSocket, reflector: SocketAddr, room: &str, wait: Duration) -> Option<SocketAddr> {
    let id: [u8; 8] = crate::keys::random(8).try_into().unwrap();
    let ask = Packet::Probe {
        id,
        room_hash: punch::room_hash(room),
    }
    .encode();
    let end = Instant::now() + wait;
    let mut buf = [0u8; 128];
    while Instant::now() < end {
        let _ = socket.send_to(&ask, reflector);
        let _ = socket.set_read_timeout(Some(Duration::from_millis(250)));
        let round = Instant::now() + Duration::from_millis(250);
        while Instant::now() < round {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) if from == reflector => {
                    if let Some(Packet::Seen { id: got, addr }) = Packet::decode(&buf[..n]) {
                        if got == id {
                            return Some(addr);
                        }
                    }
                }
                Ok(_) => {}
                // Timed out (or, on Windows, an earlier send's ICMP error): ask again.
                Err(_) => break,
            }
        }
    }
    None
}

/// This machine's own addresses (not loopback or link-local) with `port`: candidates for a peer on the
/// same network (no NAT between them, or one that doesn't loop back).
pub fn local_candidates(port: u16, v4_only: bool) -> Vec<SocketAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| !(ip.is_loopback() || v4_only && ip.is_ipv6()))
        .filter(|ip| match ip {
            IpAddr::V4(v) => !v.is_link_local(),
            IpAddr::V6(v) => (v.segments()[0] & 0xffc0) != 0xfe80,
        })
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

/// Sends punch packets to each of `to` every 100 ms for `time` (the host does this from a clone of its
/// QUIC socket: it can send there, not read).
pub fn punch(socket: &UdpSocket, to: &[SocketAddr], tag: [u8; 16], time: Duration) {
    let p = Packet::Punch { tag }.encode();
    let end = Instant::now() + time;
    while Instant::now() < end {
        for a in to {
            let _ = socket.send_to(&p, a);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The viewer's punching, before it hands its socket to QUIC: punch packets to each of the host's
/// candidates, listening for the host's own. Returns the address the host's came from (the path that
/// works) as soon as one does, or none after `time` (QUIC still tries every candidate: the host's
/// packets may not reach here while this side's reach there).
pub fn punch_and_listen(socket: &UdpSocket, to: &[SocketAddr], tag: [u8; 16], time: Duration) -> Option<SocketAddr> {
    let p = Packet::Punch { tag }.encode();
    let end = Instant::now() + time;
    let mut buf = [0u8; 64];
    let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
    let mut next = Instant::now();
    while Instant::now() < end {
        if Instant::now() >= next {
            for a in to {
                let _ = socket.send_to(&p, a);
            }
            next = Instant::now() + Duration::from_millis(100);
        }
        if let Ok((n, from)) = socket.recv_from(&mut buf) {
            if Packet::decode(&buf[..n]) == Some(Packet::Punch { tag }) {
                // One more each way, so the host's NAT has seen this side answer too.
                let _ = socket.send_to(&p, from);
                return Some(from);
            }
        }
    }
    None
}

/// Keeps the NAT's mapping for the host's QUIC socket (from a clone of it) while it waits for viewers:
/// a keepalive to the relay every 15 s (NATs forget an idle UDP mapping after 30 s or more). Never
/// answered: an answer would land on the socket QUIC owns.
pub fn keep_alive(socket: UdpSocket, reflector: SocketAddr) {
    let k = Packet::Keepalive.encode();
    std::thread::spawn(move || loop {
        let _ = socket.send_to(&k, reflector);
        std::thread::sleep(Duration::from_secs(15));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punches_meet_on_this_machine() {
        let a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (aa, ba) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let tag = punch::tag("t");
        let t = std::thread::spawn(move || punch(&a, &[ba], tag, Duration::from_millis(500)));
        assert_eq!(punch_and_listen(&b, &[aa], tag, Duration::from_secs(2)), Some(aa));
        t.join().unwrap();
        // Another session's punches (another tag) aren't taken for the host's.
        let c = UdpSocket::bind("127.0.0.1:0").unwrap();
        let ca = c.local_addr().unwrap();
        let d = UdpSocket::bind("127.0.0.1:0").unwrap();
        let t = std::thread::spawn(move || punch(&c, &[d.local_addr().unwrap()], punch::tag("other"), Duration::from_millis(300)));
        let d2 = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert_eq!(punch_and_listen(&d2, &[ca], tag, Duration::from_millis(300)), None);
        t.join().unwrap();
    }

    #[test]
    fn reflectors_and_candidates() {
        assert_eq!(reflector("ws://127.0.0.1:7990").unwrap(), "127.0.0.1:7990".parse().unwrap());
        assert_eq!(reflector("wss://127.0.0.1/").unwrap().port(), 443);
        assert!(local_candidates(5, true).iter().all(|a| a.is_ipv4() && a.port() == 5 && !a.ip().is_loopback()));
    }
}
