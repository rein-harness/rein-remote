//! `rein-remote-host-gtk`: the Linux host app (GTK4 and libadwaita, a tray icon), the UI for
//! `rein-remote host`. Built with `--features gtk` on Linux; without it, this binary only says so.

#[cfg(all(target_os = "linux", feature = "gtk"))]
mod ui;

#[cfg(all(target_os = "linux", feature = "gtk"))]
fn main() -> std::process::ExitCode {
    ui::main()
}

#[cfg(not(all(target_os = "linux", feature = "gtk")))]
fn main() {
    eprintln!("rein-remote-host-gtk: this build has no app (it's Linux's, built with --features gtk)");
    std::process::exit(2);
}
