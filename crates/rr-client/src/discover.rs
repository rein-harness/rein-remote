//! Finding machines on the local network (mDNS / DNS-SD, as printers and AirPlay are found): a server
//! listening beyond this computer says where it is as `_rein-remote._tcp`, with its fingerprint and
//! nothing secret (the token stays in the links; a paired client proves itself with its key).
//! `rein-remote discover` lists what answers; native clients look the same way.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::time::{Duration, Instant};

pub const SERVICE: &str = "_rein-remote._tcp.local.";

/// A server found on the network.
#[derive(Debug, Clone)]
pub struct Found {
    /// Its name (the machine's).
    pub name: String,
    pub addrs: Vec<std::net::IpAddr>,
    pub port: u16,
    /// Its key's fingerprint (see identity.rs): what a client checks against the one it paired with.
    pub id: String,
    /// WebTransport's UDP port, when it has it.
    pub webtransport: Option<u16>,
    pub https: bool,
}

/// Saying where this server is, for as long as it's kept.
pub struct Advert {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advert {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// This machine's name without a domain, made safe for a host name.
pub fn machine_name() -> String {
    let raw = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "rein-remote".into());
    let first = raw.split('.').next().unwrap_or("rein-remote");
    let s: String = first.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    if s.is_empty() {
        "rein-remote".into()
    } else {
        s
    }
}

/// Which address to try first: IPv4 on the network, then routable IPv6, link-local, loopback last.
fn rank(a: &std::net::IpAddr) -> u8 {
    match a {
        _ if a.is_loopback() => 4,
        std::net::IpAddr::V4(v) if v.is_link_local() => 2,
        std::net::IpAddr::V4(_) => 0,
        std::net::IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80 => 3,
        std::net::IpAddr::V6(_) => 1,
    }
}

pub fn advertise(port: u16, id: &str, webtransport: Option<u16>, https: bool) -> Result<Advert, String> {
    let daemon = ServiceDaemon::new().map_err(|e| format!("mDNS: {e}"))?;
    let name = machine_name();
    let wt = webtransport.map_or("0".to_string(), |p| p.to_string());
    let props = [("v", "1"), ("id", id), ("wt", wt.as_str()), ("tls", if https { "1" } else { "0" })];
    // Its addresses filled in by the daemon, per interface, as they change.
    let info = ServiceInfo::new(SERVICE, &name, &format!("{name}-rr.local."), "", port, &props[..])
        .map_err(|e| format!("mDNS: {e}"))?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info).map_err(|e| format!("mDNS: {e}"))?;
    Ok(Advert { daemon, fullname })
}

/// The servers that answer within `wait`.
pub fn discover(wait: Duration) -> Result<Vec<Found>, String> {
    let daemon = ServiceDaemon::new().map_err(|e| format!("mDNS: {e}"))?;
    let rx = daemon.browse(SERVICE).map_err(|e| format!("mDNS: {e}"))?;
    let end = Instant::now() + wait;
    let mut out: Vec<Found> = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(s)) => {
                let name = s.fullname.strip_suffix(&format!(".{SERVICE}")).unwrap_or(&s.fullname).replace("\\032", " ");
                let mut addrs: Vec<std::net::IpAddr> = s.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
                addrs.sort_by_key(|a| (rank(a), *a));
                let f = Found {
                    name,
                    addrs,
                    port: s.port,
                    id: s.get_property_val_str("id").unwrap_or("").to_string(),
                    webtransport: s.get_property_val_str("wt").and_then(|p| p.parse().ok()).filter(|p| *p != 0),
                    https: s.get_property_val_str("tls") == Some("1"),
                };
                match out.iter_mut().find(|o| o.id == f.id && o.port == f.port) {
                    Some(o) => {
                        for a in f.addrs {
                            if !o.addrs.contains(&a) {
                                o.addrs.push(a);
                            }
                        }
                    }
                    None => out.push(f),
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a machine with a network that carries multicast (not every CI runner's does): a server
    /// advertised here is found here.
    #[test]
    #[ignore]
    fn finds_what_it_advertises() {
        let id = format!("test-{}", std::process::id());
        let _a = advertise(54321, &id, Some(54321), false).unwrap();
        let found = discover(Duration::from_secs(4)).unwrap();
        let me = found.iter().find(|f| f.id == id).expect("found itself");
        assert_eq!((me.port, me.webtransport, me.https), (54321, Some(54321), false));
        assert!(!me.addrs.is_empty());
    }

    #[test]
    fn addresses_on_the_network_come_first() {
        let mut v: Vec<std::net::IpAddr> = ["::1", "fe80::1", "fd00::5", "127.0.0.1", "192.168.1.9"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect();
        v.sort_by_key(|a| (rank(a), *a));
        assert_eq!(v[0].to_string(), "192.168.1.9");
        assert_eq!(v[1].to_string(), "fd00::5");
        assert!(v[4].is_loopback());
    }

    #[test]
    fn machine_names_are_host_names() {
        let n = machine_name();
        assert!(!n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
