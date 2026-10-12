//! A server and a relay with their flags from a config file (the token from a file of its own, never
//! on the command line), the command line winning over the file, and `install-service --print`: the
//! unit this OS gets, written to stdout, nothing installed.

mod common;
use common::{get, Proc};
use std::path::PathBuf;
use std::time::Duration;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rr-config-it-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn serve_and_relay_from_a_config_file() {
    let d = dir("run");
    std::fs::write(d.join("token"), "from-a-file\n").unwrap();
    let cfg = d.join("serve.toml");
    std::fs::write(
        &cfg,
        "test-pattern = \"320x240\"\nlisten = \"127.0.0.1:0\"\nmdns = false\nwebtransport = false\ntoken = { file = \"token\" }\nfps = 1000\n",
    )
    .unwrap();
    // fps = 1000 in the file is refused, unless the command line says otherwise (it wins).
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rein-remote"))
        .args(["serve", "--config", cfg.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--fps"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut server = Proc::start(&["serve", "--config", cfg.to_str().unwrap(), "--fps", "30"], &[]);
    let addr = server.listening();
    assert_eq!(get(&addr, "/healthz", "").0, 403);
    assert_eq!(get(&addr, "/healthz", "Authorization: Bearer from-a-file\r\n").0, 200);
    let (status, metrics) = get(&addr, "/metrics?token=from-a-file", "");
    assert_eq!(status, 200);
    assert!(metrics.contains("rein_remote_build_info{version="), "{metrics}");

    let relay_cfg = d.join("relay.toml");
    std::fs::write(
        &relay_cfg,
        "listen = \"127.0.0.1:0\"\nmetrics-listen = \"127.0.0.1:0\"\nsecret = { file = \"token\" }\n",
    )
    .unwrap();
    let mut relay = Proc::start(&["relay", "--config", relay_cfg.to_str().unwrap()], &[]);
    let at = relay.listening();
    let line = relay.wait_for("metrics on http://", Duration::from_secs(30));
    let metrics_at = line.split("http://").nth(1).unwrap().trim_end_matches("/metrics");
    let (status, health) = get(&at, "/healthz", "");
    assert_eq!(status, 200);
    assert!(health.contains(r#""role":"relay""#), "{health}");
    assert!(get(metrics_at, "/metrics", "").1.contains("rein_remote_relay_sessions 0"));
    // /metrics isn't on the relay's public port.
    assert_eq!(get(&at, "/metrics", "").0, 404);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn install_service_prints_what_it_would_install() {
    let d = dir("print");
    let cfg = d.join("relay.toml");
    std::fs::write(&cfg, "listen = \"127.0.0.1:7990\"\n").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rein-remote"))
        .args(["install-service", "relay", "--config", cfg.to_str().unwrap(), "--print"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let cfg_s = cfg.canonicalize().unwrap().display().to_string().trim_start_matches(r"\\?\").to_string();
    if cfg!(target_os = "linux") {
        assert!(text.contains("/systemd/user/rein-remote-relay.service\n[Unit]"), "{text}");
        assert!(text.contains(&format!(" relay --config {cfg_s}\n")), "{text}");
        assert!(text.contains("$ systemctl --user enable --now rein-remote-relay.service"), "{text}");
    } else if cfg!(target_os = "macos") {
        assert!(text.contains("/Library/LaunchAgents/com.rein-harness.rein-remote-relay.plist"), "{text}");
        assert!(text.contains(&format!("<string>{cfg_s}</string>")), "{text}");
        assert!(text.contains("$ launchctl bootstrap gui/"), "{text}");
    } else {
        assert!(text.contains("$ schtasks.exe /Create /F /TN rein-remote-relay /SC ONLOGON"), "{text}");
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rein-remote"))
        .args(["install-service", "relay", "--system", "--config", cfg.to_str().unwrap(), "--print"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    if cfg!(windows) {
        assert!(text.contains("$ sc.exe create rein-remote-relay binPath="), "{text}");
        assert!(text.contains("service-run rein-remote-relay relay --config"), "{text}");
    } else if cfg!(target_os = "macos") {
        assert!(text.contains("# /Library/LaunchDaemons/com.rein-harness.rein-remote-relay.plist"), "{text}");
    } else {
        assert!(text.contains("# /etc/systemd/system/rein-remote-relay.service"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
