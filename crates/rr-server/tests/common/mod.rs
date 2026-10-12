//! Running `rein-remote` for a test: its stderr read line by line, waited on for what it prints, and the
//! process ended when the test is done with it.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Proc {
    child: Child,
    lines: mpsc::Receiver<String>,
    pub seen: Vec<String>,
}

impl Proc {
    pub fn start(args: &[&str], env: &[(&str, &str)]) -> Proc {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rein-remote"));
        cmd.args(args).stdout(Stdio::null()).stderr(Stdio::piped()).env_remove("REIN_REMOTE_TOKEN");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("rein-remote starts");
        let err = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Proc { child, lines, seen: vec![] }
    }

    /// The first line (from now) with `what` in it.
    pub fn wait_for(&mut self, what: &str, within: Duration) -> String {
        let end = Instant::now() + within;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(l) => {
                    self.seen.push(l.clone());
                    if l.contains(what) {
                        return l;
                    }
                }
                Err(_) => break,
            }
        }
        panic!("rein-remote didn't print {what:?} in {within:?}; it printed:\n{}", self.seen.join("\n"));
    }

    /// Ends it now (as a crash or `kill -9` would: no goodbye to anyone).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// The address from its "on http://ADDR" line (what Rein reads too).
    pub fn listening(&mut self) -> String {
        let l = self.wait_for("on http://", Duration::from_secs(30));
        l.split("on http://").nth(1).unwrap().split_whitespace().next().unwrap().to_string()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Events from a session until `done` says so (or it ends, or time's up): whether it did.
pub fn until(s: &rr_client::Session, within: Duration, mut done: impl FnMut(&rr_client::Event) -> bool) -> bool {
    let end = Instant::now() + within;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match s.next_event(left.min(Duration::from_millis(200))) {
            Some(rr_client::Event::Closed(why)) => panic!("the session closed: {why:?}"),
            Some(e) if done(&e) => return true,
            _ => {}
        }
    }
    false
}

pub fn frames(s: &rr_client::Session, n: usize, within: Duration) -> bool {
    let mut got = 0;
    until(s, within, |e| {
        if matches!(e, rr_client::Event::Video { .. }) {
            got += 1;
        }
        got >= n
    })
}

/// A plain GET (`headers` added as they are): the status and the body.
pub fn get(addr: &str, path: &str, headers: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).expect("connects");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{headers}Connection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map_or(String::new(), |(_, b)| b.to_string());
    (status, body)
}
