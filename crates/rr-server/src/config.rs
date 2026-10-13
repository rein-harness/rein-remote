//! `--config PATH`: a command's flags from a TOML file, for a server or relay run as a service.
//!
//! Each key is a long flag's name without its dashes, so every flag works from the file, including
//! ones added later, with nothing to declare here:
//!
//! ```toml
//! listen = "0.0.0.0:7979"
//! test-pattern = "1280x720"       # --test-pattern 1280x720
//! fps = 60                        # --fps 60
//! webtransport = false            # --webtransport off (true: on)
//! relay = ["wss://a.example", "wss://b.example"]   # --relay, once each
//! token = { file = "/etc/rein-remote/token" }      # a secret: read from its file, never in argv
//! ```
//!
//! The command line wins: a flag given there replaces every value the file has for it. A secret's
//! file is read here, so a service's command line (what `ps` and the service manager show) carries
//! only `--config PATH`. A relative `file` is relative to the config file's folder.

use std::path::{Path, PathBuf};

/// `args` (a command's, after its name) with `--config PATH` replaced by the file's flags, put first
/// so the command line's own come after (and win: the last of a flag counts). Unchanged without one.
pub fn expand(args: &[String]) -> Result<Vec<String>, String> {
    let Some(i) = args.iter().position(|a| a == "--config") else {
        return Ok(args.to_vec());
    };
    let path = args.get(i + 1).ok_or("--config needs a file (TOML)")?;
    if args[i + 2..].iter().any(|a| a == "--config") {
        return Err("--config once (one file)".into());
    }
    let rest: Vec<String> = args[..i].iter().chain(&args[i + 2..]).cloned().collect();
    let text = std::fs::read_to_string(path).map_err(|e| format!("--config {path}: {e}"))?;
    let given: Vec<&str> = rest.iter().filter_map(|a| a.strip_prefix("--")).collect();
    let mut out = flags(&text, Path::new(path).parent().unwrap_or(Path::new(".")), &given).map_err(|e| format!("{path}: {e}"))?;
    out.extend(rest);
    Ok(out)
}

/// The file's flags, in its order, leaving out those in `skip` (given on the command line).
fn flags(text: &str, dir: &Path, skip: &[&str]) -> Result<Vec<String>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
    table_flags(&table, dir, skip)
}

/// A table's flags (as a config file's), in its order, leaving out those in `skip`; a secret's
/// `{ file = … }` is relative to `dir`.
pub fn table_flags(table: &toml::Table, dir: &Path, skip: &[&str]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for (key, value) in table {
        if key.starts_with('-') || key.contains(char::is_whitespace) || key.is_empty() {
            return Err(format!("{key:?}: a key is a flag's name without its dashes (listen, not --listen)"));
        }
        if skip.contains(&key.as_str()) {
            continue;
        }
        let values = match value {
            toml::Value::Array(all) => all.iter().map(|v| one(key, v, dir)).collect::<Result<Vec<_>, _>>()?,
            v => vec![one(key, v, dir)?],
        };
        for v in values {
            out.push(format!("--{key}"));
            out.push(v);
        }
    }
    Ok(out)
}

/// One value as the flag takes it.
fn one(key: &str, v: &toml::Value, dir: &Path) -> Result<String, String> {
    Ok(match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => (if *b { "on" } else { "off" }).to_string(),
        toml::Value::Table(t) => match (t.get("file"), t.len()) {
            (Some(toml::Value::String(f)), 1) => secret(&dir.join(f))?,
            _ => return Err(format!("{key}: a table here is {{ file = \"PATH\" }} (the value read from that file)")),
        },
        toml::Value::Array(_) => return Err(format!("{key}: a list in a list")),
        toml::Value::Datetime(d) => d.to_string(),
    })
}

/// A secret from its file: the first line, without its line ending. Said when others can read it.
fn secret(path: &PathBuf) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(path) {
            if m.permissions().mode() & 0o077 != 0 {
                eprintln!("rein-remote: {} can be read by others than its owner (chmod 600 it)", path.display());
            }
        }
    }
    let line = text.lines().next().unwrap_or("").trim_end().to_string();
    if line.is_empty() {
        return Err(format!("{}: empty", path.display()));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn every_key_is_a_flag_and_the_command_line_wins() {
        let dir = std::env::temp_dir().join(format!("rr-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "s3cret\n").unwrap();
        let path = dir.join("serve.toml");
        std::fs::write(
            &path,
            r#"
# a comment
listen = "0.0.0.0:7979"
test-pattern = "640x480"
fps = 30
webtransport = false
mdns = true
relay = ["ws://a:1", "ws://b:2"]
token = { file = "token" }
some-flag-added-later = "x"
"#,
        )
        .unwrap();
        let p = path.to_str().unwrap();
        let got = expand(&s(&["--config", p, "--fps", "60"])).unwrap();
        assert_eq!(
            got,
            s(&[
                "--listen",
                "0.0.0.0:7979",
                "--test-pattern",
                "640x480",
                "--webtransport",
                "off",
                "--mdns",
                "on",
                "--relay",
                "ws://a:1",
                "--relay",
                "ws://b:2",
                "--token",
                "s3cret",
                "--some-flag-added-later",
                "x",
                "--fps",
                "60",
            ])
        );
        // The command line's --relay replaces the file's list, not adds to it.
        let got = expand(&s(&["--relay", "ws://c:3", "--config", p])).unwrap();
        assert_eq!(got.iter().filter(|a| *a == "--relay").count(), 1);
        assert_eq!(got.last().unwrap(), "ws://c:3");
        // No --config: as it was.
        assert_eq!(expand(&s(&["--fps", "5"])).unwrap(), s(&["--fps", "5"]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mistakes_are_said() {
        let dir = Path::new(".");
        assert!(flags("--listen = \"x\"", dir, &[]).is_err());
        assert!(flags("[serve]\nlisten = \"x\"", dir, &[]).unwrap_err().contains("table"));
        assert!(flags("token = { file = \"/no/such/file\" }", dir, &[]).unwrap_err().contains("/no/such/file"));
        assert!(flags("listen = ", dir, &[]).is_err());
        assert!(expand(&s(&["--config"])).is_err());
        assert!(expand(&s(&["--config", "/no/such.toml"])).is_err());
    }
}
