//! The small UDP packets that find a direct path through NAT. No I/O here: the relay, the host and
//! native viewers share these.
//!
//! - **Probe / Seen**: "what address do you see me from?", asked of the relay on its UDP port from the
//!   very socket QUIC will use, and its answer: that socket's address on the far side of the NAT (what
//!   STUN does; this is smaller, and the relay answers only for a room it has open, named by a hash so
//!   the room's code isn't sent in the clear).
//! - **Keepalive**: sent to the relay now and then, so the NAT keeps the mapping; never answered (an
//!   answer would land on a socket QUIC owns by then).
//! - **Punch**: sent by each side to the other's candidates, opening its own NAT for the other's
//!   packets; it carries a tag from the offer's ticket so each knows the other's from stray ones.
//!
//! Every packet starts with its type (below 0x40), then `RRP` and a version. Punch and keepalive
//! packets are under 22 bytes: when one lands on a socket QUIC already owns, it's too small for QUIC to
//! answer (a stateless reset needs more), so it's dropped there without a word. A Seen answer is never
//! bigger than the probe it answers (probes are padded), so the relay can't be used to amplify.

use crate::{Error, Reader, Writer};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;

pub const MAGIC: &[u8; 3] = b"RRP";
pub const VERSION: u8 = 0;
/// A probe is padded to this, so its answer (at most 32 bytes) is never bigger.
pub const PROBE_LEN: usize = 64;

const PROBE: u8 = 1;
const SEEN: u8 = 2;
const KEEPALIVE: u8 = 3;
const PUNCH: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// What address does the relay see this socket from? `id` matches the answer to the question;
    /// `room_hash` names the relay room it's for ([`room_hash`]: the relay answers only for a room it
    /// has open).
    Probe {
        id: [u8; 8],
        room_hash: [u8; 16],
    },
    /// The answer: the address the probe came from.
    Seen {
        id: [u8; 8],
        addr: SocketAddr,
    },
    Keepalive,
    /// "Here I am": opens the sender's NAT toward the receiver. `tag`: see [`tag`].
    Punch {
        tag: [u8; 16],
    },
}

/// The tag both sides put in punch packets for a session: from its one-time ticket, which the relay
/// never sees (the offer is sealed) and a stranger can't guess; the tag doesn't give the ticket away.
pub fn tag(ticket: &str) -> [u8; 16] {
    let h = Sha256::new().chain_update(b"rein-remote punch v1\0").chain_update(ticket.as_bytes()).finalize();
    h[..16].try_into().unwrap()
}

/// A relay room's code as a probe names it: a hash (the code lets a viewer in, so it isn't sent over
/// UDP in the clear; the relay hashes the rooms it has open and compares).
pub fn room_hash(room: &str) -> [u8; 16] {
    let h = Sha256::new().chain_update(b"rein-remote room v1\0").chain_update(room.as_bytes()).finalize();
    h[..16].try_into().unwrap()
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        let t = match self {
            Packet::Probe { .. } => PROBE,
            Packet::Seen { .. } => SEEN,
            Packet::Keepalive => KEEPALIVE,
            Packet::Punch { .. } => PUNCH,
        };
        w.u8(t);
        for b in MAGIC {
            w.u8(*b);
        }
        w.u8(VERSION);
        match self {
            Packet::Probe { id, room_hash } => {
                id.iter().for_each(|b| w.u8(*b));
                room_hash.iter().for_each(|b| w.u8(*b));
            }
            Packet::Seen { id, addr } => {
                id.iter().for_each(|b| w.u8(*b));
                w.addr(addr);
            }
            Packet::Keepalive => {}
            Packet::Punch { tag } => tag.iter().for_each(|b| w.u8(*b)),
        }
        let mut out = w.into_inner();
        if t == PROBE && out.len() < PROBE_LEN {
            out.resize(PROBE_LEN, 0);
        }
        out
    }

    /// A packet, if `b` is one of these (anything else, a QUIC packet say, is `None`).
    pub fn decode(b: &[u8]) -> Option<Packet> {
        if b.len() < 5 || &b[1..4] != MAGIC || b[4] != VERSION {
            return None;
        }
        let mut r = Reader::new(&b[5..]);
        let p = match b[0] {
            PROBE => Packet::Probe {
                id: array(&mut r).ok()?,
                room_hash: array(&mut r).ok()?,
            },
            SEEN => Packet::Seen {
                id: array(&mut r).ok()?,
                addr: r.addr().ok()?,
            },
            KEEPALIVE => Packet::Keepalive,
            PUNCH => Packet::Punch { tag: array(&mut r).ok()? },
            _ => return None,
        };
        Some(p)
    }
}

fn array<const N: usize>(r: &mut Reader) -> Result<[u8; N], Error> {
    let mut a = [0u8; N];
    for x in &mut a {
        *x = r.u8()?;
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip() {
        for p in [
            Packet::Probe {
                id: [1, 2, 3, 4, 5, 6, 7, 8],
                room_hash: room_hash("abcdefghijkmnpqrstuvwxyz"),
            },
            Packet::Seen {
                id: [9; 8],
                addr: "203.0.113.7:7979".parse().unwrap(),
            },
            Packet::Seen {
                id: [9; 8],
                addr: "[2001:db8::1]:443".parse().unwrap(),
            },
            Packet::Keepalive,
            Packet::Punch { tag: tag("ticket") },
        ] {
            assert_eq!(Packet::decode(&p.encode()), Some(p));
        }
    }

    #[test]
    fn answers_are_never_bigger_than_probes_and_quic_never_answers_punches() {
        let probe = Packet::Probe {
            id: [0; 8],
            room_hash: [0; 16],
        }
        .encode();
        let seen = Packet::Seen {
            id: [0; 8],
            addr: "[2001:db8::1]:443".parse().unwrap(),
        }
        .encode();
        assert!(seen.len() <= probe.len(), "{} > {}", seen.len(), probe.len());
        // QUIC answers an unknown packet with a stateless reset only when it's 22 bytes or more.
        assert!(Packet::Punch { tag: [0; 16] }.encode().len() < 22);
        assert!(Packet::Keepalive.encode().len() < 22);
        // Each starts below 0x40 (QUIC's fixed bit clear).
        for p in [probe, seen, Packet::Keepalive.encode()] {
            assert!(p[0] < 0x40);
        }
    }

    #[test]
    fn other_packets_are_not_these() {
        assert_eq!(Packet::decode(&[]), None);
        assert_eq!(Packet::decode(&[0xc0, 0, 0, 0, 1, 8, 1, 2, 3]), None);
        assert_eq!(Packet::decode(b"\x04RRQ\x00"), None);
        assert_eq!(Packet::decode(b"\x04RRP\x00short"), None);
        assert_eq!(Packet::decode(b"\x09RRP\x00"), None);
        // Different tickets, different tags; different rooms, different hashes.
        assert_ne!(tag("a"), tag("b"));
        assert_ne!(room_hash("a"), room_hash("b"));
        assert_ne!(room_hash("a"), tag("a"));
    }
}
