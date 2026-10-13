//! Several relays for one machine (`serve --relay A --relay B`, or `--relay A,B`): a fleet.
//!
//! The machine opens its room on every relay it was given, and keeps it open on each (again after
//! each viewer, again when a relay comes back). So whichever relay a viewer reaches finds it there:
//! the fastest one the link names, the next one in the link when that one is gone, or any of them
//! behind one name (a load balancer in front, checking each relay's `/healthz`).
//!
//! The link names the relay that answered `/healthz` fastest when the machine started, and the others
//! after `#` (`&alt=`), where only the viewer reads them; the end-to-end key stays there too, so no
//! relay ever has it.

use std::time::{Duration, Instant};

/// How often each relay's `/healthz` is asked (for the metrics and the log; the rooms reconnect by
/// themselves).
const EVERY: Duration = Duration::from_secs(10);

/// The relay's `/healthz` round trip in ms, or None when it doesn't answer. An older relay with no
/// `/healthz` answers 404: alive all the same.
pub fn probe(relay: &str) -> Option<f64> {
    let url = rr_client::http::parse(relay).ok()?;
    let path = format!("{}/healthz", url.path.trim_end_matches('/'));
    let t = Instant::now();
    let (status, _) = rr_client::http::request(&url.origin, "GET", &path, None).ok()?;
    (status == 200 || status == 404).then(|| t.elapsed().as_secs_f64() * 1000.0)
}

/// The relays, the ones that answer first (fastest first), then the others as given. Asks them all at
/// once (a relay that's down costs its timeout once, not once per relay).
pub fn rank(relays: &[String]) -> Vec<String> {
    let probes: Vec<_> = relays.iter().cloned().map(|r| std::thread::spawn(move || (probe(&r), r))).collect();
    let mut seen: Vec<(Option<f64>, String)> = probes.into_iter().filter_map(|p| p.join().ok()).collect();
    for (rtt, r) in &seen {
        crate::metrics::relay_update(r, |s| {
            s.up = rtt.is_some();
            s.rtt_ms = *rtt;
        });
    }
    // Stable: equal ones (all down) keep the order they were given in.
    seen.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    if relays.len() > 1 {
        let said: Vec<String> = seen
            .iter()
            .map(|(rtt, r)| match rtt {
                Some(ms) => format!("{} {ms:.0} ms", crate::metrics::relay_name(r)),
                None => format!("{} not answering", crate::metrics::relay_name(r)),
            })
            .collect();
        eprintln!("rein-remote: relays, fastest first: {}", said.join(", "));
    }
    seen.into_iter().map(|(_, r)| r).collect()
}

/// The viewer's link: the first relay's page, the room, then after `#` the key and the other relays.
pub fn link(ranked: &[String], room: &str, key: &[u8; 32]) -> String {
    let page = ranked[0].replacen("ws", "http", 1);
    let mut link = format!("{}/?room={room}#k={}", page.trim_end_matches('/'), rr_client::e2e::b64url(key));
    if ranked.len() > 1 {
        let alt: Vec<&str> = ranked[1..].iter().map(|r| r.trim_end_matches('/')).collect();
        link.push_str(&format!("&alt={}", alt.join(",")));
    }
    link
}

/// Asks each relay's `/healthz` from now on, for the metrics, and says when one stops answering or
/// comes back; until `stop` is set (sharing stopped).
pub fn watch(relays: Vec<String>, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    std::thread::spawn(move || {
        let mut was: Vec<bool> = relays
            .iter()
            .map(|r| crate::metrics::relays().iter().any(|s| s.name == crate::metrics::relay_name(r) && s.up))
            .collect();
        loop {
            std::thread::sleep(EVERY);
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            for (i, r) in relays.iter().enumerate() {
                let rtt = probe(r);
                crate::metrics::relay_update(r, |s| {
                    s.up = rtt.is_some();
                    s.rtt_ms = rtt;
                });
                match (was[i], rtt.is_some()) {
                    (true, false) => eprintln!("rein-remote: the relay {} stopped answering", crate::metrics::relay_name(r)),
                    (false, true) => eprintln!("rein-remote: the relay {} answers", crate::metrics::relay_name(r)),
                    _ => {}
                }
                was[i] = rtt.is_some();
            }
        }
    });
}

/// `--relay` given once or more, each maybe a comma-separated list: every relay once, in order.
pub fn relays(given: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in given.iter().flat_map(|g| g.split(',')).map(str::trim).filter(|r| !r.is_empty()) {
        if !out.iter().any(|o| o == r) {
            out.push(r.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relays_from_flags() {
        let given = vec!["ws://a:1,ws://b:2".to_string(), "ws://c:3".into(), "ws://a:1".into()];
        assert_eq!(relays(&given), vec!["ws://a:1", "ws://b:2", "ws://c:3"]);
    }

    #[test]
    fn the_link_keeps_the_key_and_the_other_relays_after_the_hash() {
        let link = link(&["wss://a.example/".into(), "ws://b:7990".into()], "abcdefghijklmnopq", &[7; 32]);
        let (before, after) = link.split_once('#').unwrap();
        assert_eq!(before, "https://a.example/?room=abcdefghijklmnopq");
        assert!(after.starts_with("k=") && after.ends_with("&alt=ws://b:7990"), "{after}");
        let url = rr_client::http::parse(&link).unwrap();
        assert_eq!(rr_client::http::param(&url.fragment, "alt"), Some("ws://b:7990"));
    }

    #[test]
    fn a_relay_not_there_ranks_last() {
        // Nothing listens on port 1.
        let ranked = rank(&["ws://127.0.0.1:1".into()]);
        assert_eq!(ranked, vec!["ws://127.0.0.1:1"]);
        assert!(probe("ws://127.0.0.1:1").is_none());
    }
}
