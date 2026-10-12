//! Pairing natively: a server that shows a code, a client that enters it once (its key and the
//! server's kept in its config folder, owner-only), then signs in with nothing to type. Its own test
//! binary: it points REIN_REMOTE_HOME at a folder of its own, which is the process's.

mod common;
use common::{frames, Proc};
use rr_client::{connect, Options};
use std::time::Duration;

#[test]
fn pairs_with_the_code_then_signs_in_with_keys() {
    let home = std::env::temp_dir().join(format!("rr-pair-it-{}", std::process::id()));
    let server_home = home.join("server");
    let client_home = home.join("client");
    std::env::set_var("REIN_REMOTE_HOME", &client_home);
    let mut server = Proc::start(
        &["serve", "--test-pattern", "320x240", "--listen", "127.0.0.1:0", "--mdns", "off", "--pair", "on"],
        &[("REIN_REMOTE_HOME", server_home.to_str().unwrap())],
    );
    let addr = server.listening();
    let line = server.wait_for(" and enter ", Duration::from_secs(30));
    let code = line.split(" and enter ").nth(1).unwrap().split(',').next().unwrap().trim().to_string();
    let url = format!("http://{addr}/");

    // Not paired, no token: refused, saying how.
    let err = connect(&url, Options::default()).err().expect("refused");
    assert!(err.contains("--pair"), "{err}");
    // A wrong code: refused.
    let wrong = Options {
        pair: Some("000-000".into()),
        ..Default::default()
    };
    let err = connect(&url, wrong).err().expect("a wrong code is refused");
    assert!(err.contains("didn't match"), "{err}");

    let opts = Options {
        pair: Some(code),
        webtransport: false,
        ..Default::default()
    };
    let s = connect(&url, opts).unwrap();
    assert!(frames(&s, 5, Duration::from_secs(30)), "frames after pairing");
    drop(s);
    let kept: Vec<_> = std::fs::read_dir(client_home.join("servers")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(kept.len(), 1, "one server kept");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&kept[0]).unwrap().permissions().mode() & 0o077, 0, "only its owner reads it");
    }

    // Again, with nothing to type: signed in with its key (and over WebTransport, the same ticket).
    let s = connect(&url, Options::default()).unwrap();
    assert!(frames(&s, 5, Duration::from_secs(30)), "frames after signing in");
    let _ = std::fs::remove_dir_all(home);
}
