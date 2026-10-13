//! Finding `rein-remote host`, starting it when none is running, and staying connected to it: a
//! thread that reads `control.json`, follows the host's events, and when the host goes (it exited,
//! it was restarted) says so and finds the next one.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::model::{ControlInfo, Event, State};

/// The host's home folder: `$REIN_REMOTE_HOME`, else `~/.rein-remote` (`%APPDATA%\rein-remote`).
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("REIN_REMOTE_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    if cfg!(windows) {
        if let Some(a) = std::env::var_os("APPDATA") {
            return PathBuf::from(a).join("rein-remote");
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".rein-remote")
}

pub fn read_control(home: &Path) -> Result<ControlInfo, String> {
    let path = home.join("control.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("no {}: {e}", path.display()))?;
    let info: ControlInfo = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if info.url.is_empty() || info.token.is_empty() {
        return Err(format!("{} has no url or token", path.display()));
    }
    Ok(info)
}

/// The `rein-remote` binary: `$REIN_REMOTE_BIN`, else the one next to this app, else on `PATH`.
pub fn find_binary() -> Option<PathBuf> {
    if let Some(b) = std::env::var_os("REIN_REMOTE_BIN").filter(|b| !b.is_empty()) {
        return Some(PathBuf::from(b));
    }
    let name = if cfg!(windows) { "rein-remote.exe" } else { "rein-remote" };
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let b = dir.join(name);
        if b.is_file() {
            return Some(b);
        }
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|b| b.is_file())
}

/// How the app is connected to the host, for the window and the tray.
#[derive(Clone, Debug)]
pub enum Link {
    /// Looking for it.
    Connecting,
    /// Started it; waiting for its API.
    Starting(String),
    /// Connected: the client to call it with, and its first state.
    Connected { client: Client, info: ControlInfo, state: Box<State> },
    /// Not running, or gone; why, said for a person. The thread keeps looking.
    Gone(String),
}

/// What the connection thread sends.
#[derive(Clone, Debug)]
pub enum Msg {
    Link(Link),
    Event(Event),
}

pub struct Options {
    pub home: PathBuf,
    /// Start `rein-remote host` when none answers (else only wait for one).
    pub start: bool,
    /// The binary to start (else [`find_binary`]).
    pub binary: Option<PathBuf>,
    /// Extra arguments for `rein-remote host`.
    pub args: Vec<String>,
}

/// The host this app started, to stop when the app quits.
#[derive(Clone, Default)]
pub struct Started(Arc<Mutex<Option<Child>>>);

impl Started {
    /// Whether the child is still running (and reaped when it isn't).
    fn running(&self) -> bool {
        let mut g = self.0.lock().unwrap();
        match g.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,
            Some(_) => {
                *g = None;
                false
            }
            None => false,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.0.lock().unwrap().as_ref().map(Child::id)
    }

    /// Ends the host this app started (SIGTERM, then a kill after 3 s), if it did start one.
    pub fn stop(&self) {
        let Some(mut child) = self.0.lock().unwrap().take() else { return };
        #[cfg(unix)]
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The host answering at `home`'s `control.json`, and its state.
pub fn attach(home: &Path) -> Result<(ControlInfo, Client, State), String> {
    let info = read_control(home)?;
    let client = Client::new(&info);
    let state = client.state().map_err(|e| e.message)?;
    Ok((info, client, state))
}

/// The last lines of the host's log, to say why it ended.
fn log_tail(path: &Path) -> String {
    let Ok(mut f) = File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(2000)));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s.lines().rfind(|l| !l.starts_with("rein-remote: {")).unwrap_or("").trim().to_owned()
}

fn spawn(opts: &Options, bin: &Path) -> std::io::Result<Child> {
    fs::create_dir_all(&opts.home)?;
    let log = OpenOptions::new().create(true).append(true).open(opts.home.join("host.log"))?;
    Command::new(bin)
        .arg("host")
        .args(&opts.args)
        .env("REIN_REMOTE_HOME", &opts.home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
}

/// Runs until `stop` is set or `send` returns false: connects, follows events, reconnects; starts the
/// host when `opts.start` and none answers.
pub fn supervise(opts: Options, started: Started, stop: Arc<AtomicBool>, send: impl Fn(Msg) -> bool) {
    let live = || !stop.load(Ordering::Relaxed);
    let nap = |d: Duration| {
        let end = Instant::now() + d;
        while Instant::now() < end && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let mut last_start: Option<Instant> = None;
    let mut last_gone = String::new();
    if !send(Msg::Link(Link::Connecting)) {
        return;
    }
    while live() {
        match attach(&opts.home) {
            Ok((info, client, state)) => {
                last_gone.clear();
                if !send(Msg::Link(Link::Connected {
                    client: client.clone(),
                    info,
                    state: Box::new(state),
                })) {
                    return;
                }
                let mut open = true;
                let r = client.events(|e| {
                    open = send(Msg::Event(e));
                    open && live()
                });
                if !open || !live() {
                    return;
                }
                let why = match r {
                    Ok(()) => "rein-remote host stopped".to_owned(),
                    Err(e) => e.message,
                };
                if !send(Msg::Link(Link::Gone(format!("{why}; looking for it again…")))) {
                    return;
                }
                nap(Duration::from_millis(500));
            }
            Err(why) => {
                let may_start = opts.start && !started.running() && last_start.is_none_or(|t| t.elapsed() > Duration::from_secs(10));
                if may_start {
                    last_start = Some(Instant::now());
                    let Some(bin) = opts.binary.clone().or_else(find_binary) else {
                        let msg = "rein-remote isn't next to this app or on PATH: install it, or start `rein-remote host` yourself".to_owned();
                        if !send(Msg::Link(Link::Gone(msg))) {
                            return;
                        }
                        nap(Duration::from_secs(2));
                        continue;
                    };
                    match spawn(&opts, &bin) {
                        Ok(child) => {
                            *started.0.lock().unwrap() = Some(child);
                            if !send(Msg::Link(Link::Starting(format!("Starting {} host…", bin.display())))) {
                                return;
                            }
                            // Its API, within 15 s.
                            let end = Instant::now() + Duration::from_secs(15);
                            while live() && Instant::now() < end && started.running() {
                                if attach(&opts.home).is_ok() {
                                    break;
                                }
                                nap(Duration::from_millis(200));
                            }
                            if !started.running() && attach(&opts.home).is_err() {
                                let tail = log_tail(&opts.home.join("host.log"));
                                let msg = if tail.is_empty() {
                                    "rein-remote host exited as it started".to_owned()
                                } else {
                                    format!("rein-remote host exited: {tail}")
                                };
                                if !send(Msg::Link(Link::Gone(msg))) {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            if !send(Msg::Link(Link::Gone(format!("can't start {}: {e}", bin.display())))) {
                                return;
                            }
                            nap(Duration::from_secs(2));
                        }
                    }
                    continue;
                }
                let msg = if started.running() {
                    "rein-remote host is starting…".to_owned()
                } else {
                    format!("rein-remote host isn't running ({why})")
                };
                if msg != last_gone {
                    last_gone = msg.clone();
                    if !send(Msg::Link(Link::Gone(msg))) {
                        return;
                    }
                }
                nap(Duration::from_secs(1));
            }
        }
    }
}
