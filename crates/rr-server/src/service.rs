//! `rein-remote install-service` / `uninstall-service`: `serve` or `relay` started with the machine
//! (or the user's session), with its flags from a config file (`--config`, see `config.rs`).
//!
//!   rein-remote install-service [serve|relay|host] [--config FILE.toml] [--system] [--name NAME] [--print]
//!   rein-remote uninstall-service [serve|relay|host] [--system] [--name NAME] [--print]
//!
//! - Linux: a systemd unit, the user's (`systemctl --user`, the default) or the system's (`--system`).
//! - macOS: a launchd LaunchAgent in the user's session (the default: screen capture needs it) or a
//!   LaunchDaemon (`--system`).
//! - Windows: a scheduled task at logon (the default: the user's desktop), or a Windows service
//!   (`--system`; a service can't see anyone's screen, so it suits `relay`, or `serve` with a test
//!   pattern, a VNC display or piped frames).
//!
//! `--print` writes what it would install (the file, the commands) and changes nothing. The service's
//! command line is only `rein-remote serve --config FILE`: secrets stay in files the config names.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }
}

/// What to install.
#[derive(Clone, Debug)]
pub struct Spec {
    /// `serve` or `relay`.
    pub command: String,
    pub name: String,
    pub exe: PathBuf,
    pub config: Option<PathBuf>,
    pub system: bool,
    /// The user's home (user-scope files go under it) and id (launchd's `gui/UID`).
    pub home: PathBuf,
    /// Where systemd looks for the user's units: `$XDG_CONFIG_HOME`, else `~/.config`.
    pub config_home: PathBuf,
    pub uid: u32,
    /// Started (or stopped) now too, not only at the next login or boot.
    pub now: bool,
}

/// A command to run; `may_fail`: its failure is expected sometimes (stopping what isn't running).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub argv: Vec<String>,
    pub may_fail: bool,
    /// Its failure is the usual case (clearing what an earlier install may have left): not said.
    pub quiet: bool,
}

fn step(argv: &[&str]) -> Step {
    Step {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        may_fail: false,
        quiet: false,
    }
}

fn quietly(argv: &[&str]) -> Step {
    Step { quiet: true, ..may_fail(argv) }
}

fn may_fail(argv: &[&str]) -> Step {
    Step { may_fail: true, ..step(argv) }
}

/// Files to write, then commands to run; files to remove once the first `remove_after` commands
/// have run (what's running stopped first, the service manager told after).
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub files: Vec<(PathBuf, String)>,
    pub remove: Vec<PathBuf>,
    pub remove_after: usize,
    pub steps: Vec<Step>,
}

impl Spec {
    /// The service's own arguments after the program: `serve --config FILE`.
    fn args(&self) -> Vec<String> {
        let mut a = vec![self.command.clone()];
        if let Some(c) = &self.config {
            a.push("--config".into());
            a.push(c.display().to_string());
        }
        a
    }

    fn what(&self) -> &'static str {
        match self.command.as_str() {
            "relay" => "relay",
            "host" => "host",
            _ => "server",
        }
    }

    /// launchd's label.
    pub fn label(&self) -> String {
        format!("com.rein-harness.{}", self.name)
    }
}

/// An argument as systemd's ExecStart reads it: quoted when it has to be, `%` doubled.
fn systemd_quote(a: &str) -> String {
    let a = a.replace('%', "%%");
    if a.is_empty() || a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '\\' || c == ';') {
        format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        a
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// An argument for a Windows command line (quoted when it has spaces or quotes).
fn win_quote(a: &str) -> String {
    if a.is_empty() || a.contains([' ', '\t', '"']) {
        format!("\"{}\"", a.replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

pub fn systemd_unit(s: &Spec) -> String {
    let exec: Vec<String> = std::iter::once(s.exe.display().to_string())
        .chain(s.args())
        .map(|a| systemd_quote(&a))
        .collect();
    let mut u = format!(
        "# Written by `rein-remote install-service`; `rein-remote uninstall-service` removes it.\n[Unit]\nDescription=Rein Remote {} ({})\nDocumentation=https://github.com/rein-harness/rein-remote\n",
        s.what(),
        s.name
    );
    if s.system {
        u.push_str("After=network-online.target\nWants=network-online.target\n");
    }
    u.push_str(&format!(
        "\n[Service]\nType=simple\nExecStart={}\nRestart=on-failure\nRestartSec=2\n",
        exec.join(" ")
    ));
    if s.system && s.command == "relay" {
        // A relay reads its config and writes nothing.
        u.push_str("NoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=read-only\nPrivateTmp=yes\n");
    }
    u.push_str(&format!(
        "\n[Install]\nWantedBy={}\n",
        if s.system { "multi-user.target" } else { "default.target" }
    ));
    u
}

pub fn launchd_plist(s: &Spec) -> String {
    let args: String = std::iter::once(s.exe.display().to_string())
        .chain(s.args())
        .map(|a| format!("\t\t<string>{}</string>\n", xml(&a)))
        .collect();
    let log = log_path(s);
    let session = if s.system {
        String::new()
    } else {
        // The user's graphical session: where the screen can be captured.
        "\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n\t<key>ProcessType</key>\n\t<string>Interactive</string>\n".to_string()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `rein-remote install-service`; `rein-remote uninstall-service` removes it. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
{args}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ThrottleInterval</key>
	<integer>5</integer>
	<!-- 077: its log (serve's links carry its token) is its owner's alone. -->
	<key>Umask</key>
	<integer>63</integer>
{session}	<key>StandardErrorPath</key>
	<string>{}</string>
	<key>StandardOutPath</key>
	<string>{}</string>
</dict>
</plist>
"#,
        xml(&s.label()),
        xml(&log.display().to_string()),
        xml(&log.display().to_string()),
    )
}

/// `dir/rest` with `/` (the paths here are Linux's and macOS's, whatever OS writes them out).
fn under(dir: &Path, rest: &str) -> PathBuf {
    PathBuf::from(format!("{}/{rest}", dir.display().to_string().trim_end_matches('/')))
}

/// Where launchd writes what the service says (macOS).
fn log_path(s: &Spec) -> PathBuf {
    if s.system {
        PathBuf::from(format!("/Library/Logs/{}.log", s.name))
    } else {
        under(&s.home, &format!("Library/Logs/{}.log", s.name))
    }
}

/// Where the unit file goes.
pub fn unit_path(s: &Spec, os: Os) -> Option<PathBuf> {
    match (os, s.system) {
        (Os::Linux, true) => Some(PathBuf::from(format!("/etc/systemd/system/{}.service", s.name))),
        (Os::Linux, false) => Some(under(&s.config_home, &format!("systemd/user/{}.service", s.name))),
        (Os::Mac, true) => Some(PathBuf::from(format!("/Library/LaunchDaemons/{}.plist", s.label()))),
        (Os::Mac, false) => Some(under(&s.home, &format!("Library/LaunchAgents/{}.plist", s.label()))),
        (Os::Windows, _) => None,
    }
}

pub fn install_plan(s: &Spec, os: Os) -> Plan {
    let mut p = Plan::default();
    match os {
        Os::Linux => {
            let path = unit_path(s, os).unwrap();
            p.files.push((path, systemd_unit(s)));
            let unit = format!("{}.service", s.name);
            let ctl: &[&str] = if s.system { &["systemctl"] } else { &["systemctl", "--user"] };
            p.steps.push(step(&[ctl, &["daemon-reload"]].concat()));
            if s.now {
                p.steps.push(step(&[ctl, &["enable", "--now", &unit]].concat()));
                // Started again if it was already running (an update of the binary or the unit).
                p.steps.push(step(&[ctl, &["restart", &unit]].concat()));
            } else {
                p.steps.push(step(&[ctl, &["enable", &unit]].concat()));
            }
        }
        Os::Mac => {
            let path = unit_path(s, os).unwrap();
            let path_s = path.display().to_string();
            p.files.push((path, launchd_plist(s)));
            let domain = if s.system { "system".to_string() } else { format!("gui/{}", s.uid) };
            // (Not now: launchd loads it at the next login.)
            if s.now {
                p.steps.push(quietly(&["launchctl", "bootout", &format!("{domain}/{}", s.label())]));
                p.steps.push(step(&["launchctl", "bootstrap", &domain, &path_s]));
            }
        }
        Os::Windows if s.system => {
            let bin = std::iter::once(s.exe.display().to_string())
                .chain(["service-run".to_string(), s.name.clone()])
                .chain(s.args())
                .map(|a| win_quote(&a))
                .collect::<Vec<_>>()
                .join(" ");
            let display = format!("Rein Remote {} ({})", s.what(), s.name);
            p.steps.push(step(&[
                "sc.exe",
                "create",
                &s.name,
                "binPath=",
                &bin,
                "start=",
                "auto",
                "DisplayName=",
                &display,
            ]));
            p.steps.push(step(&[
                "sc.exe",
                "description",
                &s.name,
                "Rein Remote: rein-remote install-service put it here; rein-remote uninstall-service --system removes it.",
            ]));
            p.steps.push(step(&[
                "sc.exe",
                "failure",
                &s.name,
                "reset=",
                "60",
                "actions=",
                "restart/2000/restart/5000/restart/10000",
            ]));
            p.steps.push(step(&["sc.exe", "start", &s.name]));
        }
        Os::Windows => {
            let run = std::iter::once(s.exe.display().to_string())
                .chain(s.args())
                .map(|a| win_quote(&a))
                .collect::<Vec<_>>()
                .join(" ");
            p.steps.push(step(&[
                "schtasks.exe",
                "/Create",
                "/F",
                "/TN",
                &s.name,
                "/SC",
                "ONLOGON",
                "/RL",
                "LIMITED",
                "/TR",
                &run,
            ]));
            if s.now {
                p.steps.push(step(&["schtasks.exe", "/Run", "/TN", &s.name]));
            }
        }
    }
    p
}

pub fn uninstall_plan(s: &Spec, os: Os) -> Plan {
    let mut p = Plan::default();
    match os {
        Os::Linux => {
            let unit = format!("{}.service", s.name);
            let ctl: &[&str] = if s.system { &["systemctl"] } else { &["systemctl", "--user"] };
            if s.now {
                p.steps.push(may_fail(&[ctl, &["stop", &unit]].concat()));
            }
            p.steps.push(may_fail(&[ctl, &["disable", &unit]].concat()));
            p.remove.push(unit_path(s, os).unwrap());
            p.remove_after = p.steps.len();
            p.steps.push(step(&[ctl, &["daemon-reload"]].concat()));
        }
        Os::Mac => {
            let domain = if s.system { "system".to_string() } else { format!("gui/{}", s.uid) };
            if s.now {
                p.steps.push(may_fail(&["launchctl", "bootout", &format!("{domain}/{}", s.label())]));
            }
            p.remove.push(unit_path(s, os).unwrap());
            p.remove_after = p.steps.len();
        }
        Os::Windows if s.system => {
            p.steps.push(may_fail(&["sc.exe", "stop", &s.name]));
            p.steps.push(step(&["sc.exe", "delete", &s.name]));
        }
        Os::Windows => {
            if s.now {
                p.steps.push(may_fail(&["schtasks.exe", "/End", "/TN", &s.name]));
            }
            p.steps.push(step(&["schtasks.exe", "/Delete", "/F", "/TN", &s.name]));
        }
    }
    p
}

/// What a plan does, as text (`--print`): each file with its path, then each command.
pub fn describe(p: &Plan) -> String {
    let mut out = String::new();
    for (path, body) in &p.files {
        out.push_str(&format!("# {}\n{body}\n", path.display()));
    }
    for (i, s) in p.steps.iter().enumerate() {
        if i == p.remove_after {
            for path in &p.remove {
                out.push_str(&format!("# remove {}\n", path.display()));
            }
        }
        let line: Vec<String> = s.argv.iter().map(|a| win_quote(a)).collect();
        out.push_str(&format!(
            "$ {}{}\n",
            line.join(" "),
            if s.may_fail { "   # (may fail: nothing to stop)" } else { "" }
        ));
    }
    if p.remove_after >= p.steps.len() {
        for path in &p.remove {
            out.push_str(&format!("# remove {}\n", path.display()));
        }
    }
    out
}

fn carry_out(p: &Plan) -> Result<(), String> {
    for (path, body) in &p.files {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("rein-remote: wrote {}", path.display());
    }
    let remove = || -> Result<(), String> {
        for path in &p.remove {
            match std::fs::remove_file(path) {
                Ok(()) => eprintln!("rein-remote: removed {}", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Ok(())
    };
    for (i, s) in p.steps.iter().enumerate() {
        if i == p.remove_after {
            remove()?;
        }
        let out = std::process::Command::new(&s.argv[0])
            .args(&s.argv[1..])
            .output()
            .map_err(|e| format!("{}: {e}", s.argv[0]))?;
        if !out.status.success() && s.may_fail && !s.quiet {
            eprintln!(
                "rein-remote: `{}`: {}",
                s.argv.join(" "),
                String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("failed (nothing to stop?)")
            );
        }
        if !out.status.success() && !s.may_fail {
            return Err(format!(
                "`{}` failed ({}): {}{}",
                s.argv.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            ));
        }
    }
    if p.remove_after >= p.steps.len() {
        remove()?;
    }
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn uid() -> u32 {
    #[cfg(unix)]
    unsafe {
        libc::getuid()
    }
    #[cfg(not(unix))]
    0
}

/// `install-service` (`install`) or `uninstall-service`, with its arguments.
pub fn run(args: &[String], install: bool) -> Result<(), String> {
    let mut command = "serve".to_string();
    let (mut config, mut system, mut name, mut print) = (None::<String>, false, None::<String>, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "serve" | "relay" | "host" => command = a.clone(),
            "--config" => config = Some(it.next().ok_or("--config needs a file")?.clone()),
            "--system" => system = true,
            "--user" => system = false,
            "--name" => name = Some(it.next().ok_or("--name needs a name")?.clone()),
            "--print" | "--dry-run" => print = true,
            _ => {
                return Err(format!(
                    "{a}? (install-service [serve|relay|host] [--config FILE.toml] [--system] [--name NAME] [--print])"
                ))
            }
        }
    }
    let name = name.unwrap_or_else(|| match command.as_str() {
        "relay" => "rein-remote-relay".into(),
        "host" => HOST_NAME.into(),
        _ => "rein-remote".into(),
    });
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        return Err("--name: letters, digits, - _ and . only".into());
    }
    let config = match config {
        Some(c) if install => {
            let path = absolute(Path::new(&c));
            if !print {
                // The service would fail at every start with a file that doesn't read: say so now.
                crate::config::expand(&["--config".into(), path.display().to_string()])?;
            }
            Some(path)
        }
        _ => None,
    };
    let spec = spec(command, name, config, system, true)?;
    let os = Os::current();
    let plan = if install { install_plan(&spec, os) } else { uninstall_plan(&spec, os) };
    if print {
        print!("{}", describe(&plan));
        return Ok(());
    }
    if install && spec.command == "serve" && spec.system {
        eprintln!("rein-remote: a system service sees no one's screen: --system suits serve with --test-pattern, --vnc, --pipe or --kms");
    }
    carry_out(&plan)?;
    eprintln!(
        "rein-remote: {} {} ({})",
        spec.name,
        if install { "installed and started" } else { "stopped and removed" },
        match (os, spec.system) {
            (Os::Linux, false) => "systemd, this user's",
            (Os::Linux, true) => "systemd, the system's",
            (Os::Mac, false) => "launchd, a LaunchAgent",
            (Os::Mac, true) => "launchd, a LaunchDaemon",
            (Os::Windows, false) => "a scheduled task at logon",
            (Os::Windows, true) => "a Windows service",
        }
    );
    Ok(())
}

/// What `install-service` installs for this user (or the system), running this binary.
fn spec(command: String, name: String, config: Option<PathBuf>, system: bool, now: bool) -> Result<Spec, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(Spec {
        command,
        name,
        exe: plain(exe.canonicalize().unwrap_or(exe)),
        config,
        system,
        config_home: std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home().join(".config"), PathBuf::from),
        home: home(),
        uid: uid(),
        now,
    })
}

/// The host's service (`rein-remote host`, the user's, started at login).
pub const HOST_NAME: &str = "rein-remote-host";

/// Whether the host starts at login: it's installed, what kind of service it is, and its file.
pub fn host_at_login() -> Result<(bool, &'static str, Option<PathBuf>), String> {
    let s = spec("host".into(), HOST_NAME.into(), None, false, false)?;
    let os = Os::current();
    let kind = match os {
        Os::Linux => "systemd",
        Os::Mac => "launchd",
        Os::Windows => "task",
    };
    Ok(match unit_path(&s, os) {
        Some(path) => (path.exists(), kind, Some(path)),
        None => {
            let found = std::process::Command::new("schtasks.exe")
                .args(["/Query", "/TN", HOST_NAME])
                .output()
                .is_ok_and(|o| o.status.success());
            (found, kind, None)
        }
    })
}

/// The host started at login from now on (or not): for the next login, neither starting another
/// host now nor stopping this one.
pub fn set_host_at_login(on: bool) -> Result<(), String> {
    let s = spec("host".into(), HOST_NAME.into(), None, false, false)?;
    let os = Os::current();
    carry_out(&if on { install_plan(&s, os) } else { uninstall_plan(&s, os) })
}

fn absolute(p: &Path) -> PathBuf {
    plain(
        p.canonicalize()
            .unwrap_or_else(|_| std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())),
    )
}

/// Without Windows's `\\?\` prefix (what canonicalize gives there; not every program takes it).
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// Windows: what the service manager starts (`service-run NAME serve|relay --config FILE`): it says
/// it's running, runs the command, and stops when asked. What it prints goes to
/// `%ProgramData%\rein-remote\NAME.log`.
#[cfg(windows)]
pub mod windows {
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
    use windows_service::{define_windows_service, service_dispatcher};

    static ARGS: OnceLock<(String, Vec<String>)> = OnceLock::new();
    static HANDLE: Mutex<Option<ServiceStatusHandle>> = Mutex::new(None);

    define_windows_service!(ffi_service_main, service_main);

    pub fn run(args: &[String]) -> Result<(), String> {
        let (name, rest) = args.split_first().ok_or("service-run NAME serve|relay [--config FILE]")?;
        let _ = ARGS.set((name.clone(), rest.to_vec()));
        service_dispatcher::start(name, ffi_service_main)
            .map_err(|e| format!("service-run is for the service manager (install-service --system puts it there): {e}"))
    }

    fn status(state: ServiceState, code: u32) {
        if let Some(h) = *HANDLE.lock().unwrap() {
            let _ = h.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: if state == ServiceState::Running {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                } else {
                    ServiceControlAccept::empty()
                },
                exit_code: ServiceExitCode::Win32(code),
                checkpoint: 0,
                wait_hint: Duration::from_secs(5),
                process_id: None,
            });
        }
    }

    /// stderr to a log file of the service's own (a service has no console).
    fn log_to(name: &str) {
        use std::os::windows::io::IntoRawHandle;
        let dir = std::path::PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into())).join("rein-remote");
        let _ = std::fs::create_dir_all(&dir);
        // The log may hold serve's links (they carry its token): SYSTEM's and Administrators' only,
        // not the read access ProgramData gives every user.
        let _ = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"])
            .output();
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{name}.log"))) {
            let h = f.into_raw_handle();
            unsafe {
                let _ = ::windows::Win32::System::Console::SetStdHandle(
                    ::windows::Win32::System::Console::STD_ERROR_HANDLE,
                    ::windows::Win32::Foundation::HANDLE(h),
                );
            }
        }
    }

    fn service_main(_: Vec<OsString>) {
        let Some((name, args)) = ARGS.get().cloned() else { return };
        log_to(&name);
        let handler = move |c| match c {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                status(ServiceState::Stopped, 0);
                std::process::exit(0);
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(h) = service_control_handler::register(&name, handler) else { return };
        *HANDLE.lock().unwrap() = Some(h);
        status(ServiceState::Running, 0);
        let code = crate::dispatch(args);
        status(ServiceState::Stopped, code as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &str, system: bool) -> Spec {
        Spec {
            command: command.into(),
            name: if command == "relay" {
                "rein-remote-relay".into()
            } else {
                "rein-remote".into()
            },
            exe: PathBuf::from("/opt/rein remote/rein-remote"),
            config: Some(PathBuf::from("/etc/rein-remote/serve 100%.toml")),
            system,
            home: PathBuf::from("/home/me"),
            config_home: PathBuf::from("/home/me/.config"),
            uid: 501,
            now: true,
        }
    }

    #[test]
    fn systemd_units_run_the_config_and_nothing_secret() {
        let u = systemd_unit(&spec("serve", false));
        assert!(
            u.contains("ExecStart=\"/opt/rein remote/rein-remote\" serve --config \"/etc/rein-remote/serve 100%%.toml\"\n"),
            "{u}"
        );
        assert!(u.contains("WantedBy=default.target") && !u.contains("network-online"), "{u}");
        let u = systemd_unit(&spec("relay", true));
        assert!(u.contains("WantedBy=multi-user.target") && u.contains("ProtectSystem=strict"), "{u}");
        let p = install_plan(&spec("relay", false), Os::Linux);
        assert_eq!(p.files[0].0, PathBuf::from("/home/me/.config/systemd/user/rein-remote-relay.service"));
        assert_eq!(p.steps[1].argv, ["systemctl", "--user", "enable", "--now", "rein-remote-relay.service"]);
        let p = install_plan(&spec("relay", true), Os::Linux);
        assert_eq!(p.files[0].0, PathBuf::from("/etc/systemd/system/rein-remote-relay.service"));
        assert_eq!(p.steps[0].argv, ["systemctl", "daemon-reload"]);
        let p = uninstall_plan(&spec("relay", false), Os::Linux);
        assert!(p.steps[0].may_fail && p.remove.len() == 1);
        // Stopped and disabled while its file is there, removed, then systemd told.
        let d = describe(&p);
        let at = |what: &str| d.find(what).unwrap_or_else(|| panic!("{what} in {d}"));
        assert!(at("$ systemctl --user stop") < at("$ systemctl --user disable"));
        assert!(at("$ systemctl --user disable") < at("# remove /home/me/.config/systemd/user/rein-remote-relay.service"));
        assert!(at("# remove") < at("$ systemctl --user daemon-reload"));
        let d = describe(&uninstall_plan(&spec("relay", false), Os::Mac));
        assert!(d.find("$ launchctl bootout").unwrap() < d.find("# remove").unwrap(), "{d}");
    }

    #[test]
    fn launchd_agents_are_in_the_users_session() {
        let s = spec("serve", false);
        let x = launchd_plist(&s);
        assert!(x.contains("<string>com.rein-harness.rein-remote</string>"), "{x}");
        assert!(
            x.contains("\t\t<string>/opt/rein remote/rein-remote</string>\n\t\t<string>serve</string>\n\t\t<string>--config</string>\n"),
            "{x}"
        );
        assert!(
            x.contains("<string>Aqua</string>") && x.contains("/home/me/Library/Logs/rein-remote.log"),
            "{x}"
        );
        let p = install_plan(&s, Os::Mac);
        assert_eq!(p.files[0].0, PathBuf::from("/home/me/Library/LaunchAgents/com.rein-harness.rein-remote.plist"));
        assert_eq!(p.steps[1].argv[..3], ["launchctl", "bootstrap", "gui/501"]);
        let d = launchd_plist(&spec("relay", true));
        assert!(!d.contains("Aqua") && d.contains("/Library/Logs/rein-remote-relay.log"), "{d}");
        assert_eq!(install_plan(&spec("relay", true), Os::Mac).steps[1].argv[2], "system");
    }

    #[test]
    fn the_host_is_installed_for_the_next_login_without_starting_another() {
        let mut s = spec("host", false);
        s.name = HOST_NAME.into();
        s.config = None;
        s.now = false;
        let p = install_plan(&s, Os::Linux);
        assert!(
            systemd_unit(&s).contains("ExecStart=\"/opt/rein remote/rein-remote\" host\n"),
            "{}",
            systemd_unit(&s)
        );
        assert_eq!(p.steps.last().unwrap().argv, ["systemctl", "--user", "enable", "rein-remote-host.service"]);
        let p = uninstall_plan(&s, Os::Linux);
        assert!(p.steps.iter().all(|x| !x.argv.contains(&"stop".to_string())), "{:?}", p.steps);
        assert!(install_plan(&s, Os::Mac).steps.is_empty(), "launchd loads it at login");
        assert!(uninstall_plan(&s, Os::Mac).steps.is_empty());
        assert_eq!(
            install_plan(&s, Os::Mac).files[0].0,
            PathBuf::from("/home/me/Library/LaunchAgents/com.rein-harness.rein-remote-host.plist")
        );
        let p = install_plan(&s, Os::Windows);
        assert_eq!(p.steps.len(), 1, "created, not run: {:?}", p.steps);
        assert!(p.steps[0].argv.last().unwrap().ends_with("rein-remote\" host"), "{:?}", p.steps[0]);
        assert_eq!(uninstall_plan(&s, Os::Windows).steps.len(), 1);
    }

    #[test]
    fn windows_services_and_tasks() {
        let mut s = spec("relay", true);
        s.exe = PathBuf::from(r"C:\Program Files\rein-remote\rein-remote.exe");
        s.config = Some(PathBuf::from(r"C:\ProgramData\rein-remote\relay.toml"));
        let p = install_plan(&s, Os::Windows);
        assert_eq!(p.steps[0].argv[..3], ["sc.exe", "create", "rein-remote-relay"]);
        assert_eq!(
            p.steps[0].argv[4],
            r#""C:\Program Files\rein-remote\rein-remote.exe" service-run rein-remote-relay relay --config C:\ProgramData\rein-remote\relay.toml"#
        );
        assert_eq!(p.steps.last().unwrap().argv, ["sc.exe", "start", "rein-remote-relay"]);
        let s = Spec { system: false, ..s };
        let p = install_plan(&s, Os::Windows);
        assert_eq!(
            p.steps[0].argv[..7],
            ["schtasks.exe", "/Create", "/F", "/TN", "rein-remote-relay", "/SC", "ONLOGON"]
        );
        let p = uninstall_plan(&s, Os::Windows);
        assert_eq!(p.steps[1].argv, ["schtasks.exe", "/Delete", "/F", "/TN", "rein-remote-relay"]);
        assert!(describe(&p).contains("$ schtasks.exe /End /TN rein-remote-relay"));
    }
}
