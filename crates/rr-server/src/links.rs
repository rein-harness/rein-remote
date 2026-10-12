//! Where a viewer can open the stream: the addresses this machine has, by kind. A tailnet address
//! (Tailscale's 100.64.0.0/10, and its MagicDNS name when the tailscale command is there) works from
//! any of your devices with nothing to set up; a local-network one from the same network.

use std::net::IpAddr;

pub struct Place {
    pub label: &'static str,
    pub host: String,
}

fn tailnet(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v) if v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]))
}

/// This machine's MagicDNS name, if Tailscale is installed and running.
pub fn magic_dns() -> Option<String> {
    let out = std::process::Command::new("tailscale").args(["status", "--self", "--json"]).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    // "DNSName": "box.tail1234.ts.net." in the Self object (the first one in the output).
    let at = text.find("\"DNSName\"")?;
    let rest = &text[at + 9..];
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    let name = rest[start..end].trim_end_matches('.');
    (!name.is_empty()).then(|| name.to_string())
}

/// The places to open, best first: the tailnet, then the local network, then this computer.
pub fn places() -> Vec<Place> {
    let mut tail = vec![];
    let mut lan = vec![];
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        let ip = i.ip();
        if ip.is_loopback() || matches!(ip, IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80) {
            continue;
        }
        let host = match ip {
            IpAddr::V4(v) => v.to_string(),
            IpAddr::V6(v) => format!("[{v}]"),
        };
        if tailnet(&ip) {
            tail.push(Place { label: "your tailnet", host });
        } else if ip.is_ipv4() {
            lan.push(Place { label: "this network", host });
        }
    }
    let mut out = vec![];
    if !tail.is_empty() {
        if let Some(name) = magic_dns() {
            out.push(Place {
                label: "your tailnet",
                host: name,
            });
        }
    }
    out.extend(tail);
    out.extend(lan);
    out.push(Place {
        label: "this computer",
        host: "localhost".into(),
    });
    out
}

/// `text` as a QR code drawn with block characters (two rows a line), for a terminal.
pub fn qr(text: &str) -> String {
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else {
        return String::new();
    };
    let w = code.width();
    let dark = |x: i64, y: i64| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < w && code[(x as usize, y as usize)] == qrcode::Color::Dark;
    let mut out = String::new();
    // A quiet zone of 2 around it; light on dark terminals reads as well as dark on light.
    for y in (-2..w as i64 + 2).step_by(2) {
        out.push_str("  ");
        for x in -2..w as i64 + 2 {
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => ' ',
                (true, false) => '▄',
                (false, true) => '▀',
                (false, false) => '█',
            });
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_tailnet_addresses() {
        assert!(tailnet(&"100.101.102.103".parse().unwrap()));
        assert!(!tailnet(&"100.200.1.1".parse().unwrap()));
        assert!(!tailnet(&"192.168.1.5".parse().unwrap()));
        assert_eq!(places().last().unwrap().host, "localhost");
    }
}
