# The Linux host app on a real GNOME and KDE desktop

The Linux app (`crates/rr-host-gtk`, `rein-remote-host-gtk`: Rust, GTK4 and libadwaita) is the UI for
`rein-remote host`: a tray icon (a StatusNotifierItem, through D-Bus) and a window with Sharing,
Viewers, Paired devices, Settings and the admin pages (Sign-in, Policy, Live sessions, Audit log,
Metrics). It talks to the host only through the control API ([docs/control-api.md](../control-api.md)).

**What CI checks** (the `linux-host` job in `.github/workflows/linux-host.yml`, on `ubuntu-latest`):
the app linted and built with GTK4 and libadwaita; the API client's tests against every JSON example
in docs/control-api.md and against a fake host (the bearer token, the bodies sent, errors as the host
words them, the event stream, attaching to a running host and following it going, starting one when
none runs); then a UI test (`crates/rr-host-gtk/tests/ui`) that drives the real app through
**AT-SPI** (pyatspi, as a screen reader sees it) on Xvfb with a D-Bus session and the accessibility
bus: buttons and toggles by their AT-SPI action, pages by selecting them in the sidebar's list
(AT-SPI's Selection), text by AT-SPI's EditableText; no fallback (a GTK action over D-Bus, typing
with xdotool) was needed. The app starts the real `rein-remote host --test-pattern 1280x720` beside
it on a home folder of its own, and the test, checking each step at the host's API and in
`host.toml`: starts and stops
sharing; switches the source (the display, then the test pattern); makes a new pairing code (the
app shows the host's new code); sees a viewer (`rein-remote connect`) and kicks it (the confirmation
dialog first; gone at the host and from the app); finds the kick in the audit log (filtered to
`session_kicked`); saves "Quality first" (`prefer = "quality"` in `host.toml`) and sees a refused
frame rate's message; sees a bad policy refused with the parser's message (nothing changed) and saves
a good one; checks the tray's D-Bus item (`org.kde.StatusNotifierItem`) and its menu
(`com.canonical.dbusmenu.GetLayout`: the status, Stop sharing, the pairing code, the viewers, Open,
Quit); kills the host and sees the app say so, start a new one and reconnect. Screenshots of each page
are the job's `linux-host-app` artifact; a few are in [linux-host-app/](linux-host-app/):
[Sharing](linux-host-app/sharing.png), [kicking a viewer](linux-host-app/viewers-kick-confirm.png),
[Settings](linux-host-app/settings.png), [a refused policy](linux-host-app/policy-error.png),
[the audit log with the kick](linux-host-app/audit.png).

**What it can't check**, and this test is for: a real panel showing the tray icon (GNOME needs the
AppIndicator extension; KDE has it), a phone on the network pairing and watching, a real X11 or
Wayland session's displays and windows in the source picker, start at login, the system's dark theme,
Orca.

## Install

Either:

- **From source** (GTK 4.14 and libadwaita 1.5 or later: Ubuntu 24.04, Fedora 40, Debian 13, Arch):

  ```sh
  # Ubuntu / Debian
  sudo apt install libgtk-4-dev libadwaita-1-dev libudev-dev cmake g++ pkg-config
  # Fedora
  sudo dnf install gtk4-devel libadwaita-devel systemd-devel cmake gcc-c++
  # Arch
  sudo pacman -S gtk4 libadwaita cmake base-devel

  cargo build --release -p rr-server -p rr-host-gtk --features rr-host-gtk/gtk
  crates/rr-host-gtk/install.sh        # ~/.local/bin, the menu entry and icon (or PREFIX=/usr/local with sudo)
  ```

  The app starts the `rein-remote` next to it (`install.sh` puts both in `~/.local/bin`), else the one
  on `PATH`.

- **GNOME only:** install and enable the **AppIndicator and KStatusNotifierItem Support** extension
  (`gnome-shell-extension-appindicator` on Ubuntu, where it's on by default; elsewhere from
  extensions.gnome.org), then log out and in. Without it there's no tray icon: the app still works,
  and closing its window quits it.

## First run

1. Open **Rein Remote** from the app menu (or run `rein-remote-host-gtk`). **Expect:** a window
   "Rein Remote: Not sharing", a sidebar (Sharing, Viewers, Paired devices, Settings; Admin: Sign-in,
   Policy, Live sessions, Audit log, Metrics), and a tray icon (a grey screen). A bar "Starting
   …/rein-remote host…" shows for a moment, then goes.
   - `~/.rein-remote/` now has `control.json` (mode `600`), `host.toml` and `host.log` (the host's
     output).
   - `ps -ef | grep rein-remote`: `rein-remote-host-gtk` and its child `rein-remote host`.
2. Run `rein-remote-host-gtk` again. **Expect:** no second app; the first one's window comes forward.
3. Close the window. **Expect (with a tray):** the app stays in the tray. Click the icon: the window
   comes back. **Without a tray** (GNOME without the extension), the app quits, and the host it started
   stops.

## Share

1. Sharing page, **Start sharing**. **Expect:** the status "Sharing, no one watching", the button now
   **Stop sharing** (red), and the tray icon blue with a green dot. If a firewall is on
   (`ufw status`), allow port 7979 (TCP and UDP) for phones on the network.
2. **Links:** one per way to reach this computer ("this network", "your tailnet" with Tailscale,
   "this computer"), each said to be a key to the screen. The QR code shows the first; the grid
   button beside a link shows its QR code instead. The copy button puts it on the clipboard (a toast
   says so); paste it into a browser on another device on the network. **Expect:** the screen, live.
3. **What's shared:** the display, each window (under "Windows (N)"), and the test pattern, the one
   shared marked with a tick. **Share** a window: the browser's picture switches to it without
   reconnecting. **Share** the display again. **Another source** takes an id (`vnc:HOST:PORT`).
   - **Wayland:** the host captures X11 (on a Wayland session, only X11 apps through Xwayland show as
     windows, and the display is Xwayland's). Note what you see.

## Pair a phone

1. Install the Rein Remote app on a phone (iOS or Android) on the same network.
2. Sharing page, **Pairing**: a code like `123-456` with its countdown (`9:59 left`, down to
   `expired`), and a QR code. In the phone's app, pick this computer (found by name) and type the code,
   or scan the QR code. **Expect:** the phone shows the screen; the code is gone (a code pairs one
   device). **New code** makes another, and the old one stops working.
3. **Paired devices:** the phone, its id, when it paired.
4. The tray icon's menu (right-click on KDE; click on GNOME). **Expect:** the status ("Sharing, 1
   watching"), Stop sharing, "Pairing code: …", New pairing code, "1 viewer", Open Rein Remote, Quit.
   Start/Stop from the menu works, and the window follows.

## Kick and unpair

1. **Viewers:** the phone, with its user, how it came in (`via paired`), its transport, fps, round
   trip, size and bitrate, refreshed every second.
2. **Kick**, type a reason, **Kick viewer**. **Expect:** the phone says "the host ended this session:
   REASON" and closes; the row goes. The phone can connect again (it's still paired).
3. **Paired devices, Unpair**, confirm. **Expect:** its session ends (if it had one), and it needs a new
   code to come back.

## Settings

1. **On a slow link: Quality first**, set **Frame rate** to `30`, **Save**. **Expect:** "Saved. …" (with no
   viewers while sharing: "Sharing restarted to apply it"). `~/.rein-remote/host.toml` has
   `prefer = "quality"` and `fps = 30`.
2. Type `500` in Frame rate and Save. **Expect:** the host's message (`fps: auto or 1 to 240`) in red;
   nothing changed. **Revert**.
3. With the phone watching, turn **Sound** off and Save. **Expect:** "Viewers are connected: it applies
   when sharing restarts", and **Restart sharing** on the Sharing page; press it: the phone is told,
   and when it connects again it has no sound.
4. **Relays:** add `wss://relay.example` and a **Relay secret**, Save. **Expect:** the secret field
   empties and says "A secret is set"; `host.toml` has `relay-secret = { file = "relay-secret" }`,
   never the secret.
5. **Start the host at login** on. **Expect:** `systemctl --user status rein-remote-host` shows the
   unit (enabled). **Show Rein Remote in the tray at login** on: `~/.config/autostart/
   dev.reinharness.RemoteHost.desktop` exists. Log out and in: the host runs, the tray icon is there
   without a window, and the app connected to that host rather than starting another. Turn both off:
   the unit and the file are gone.

## Admin

1. **Sign-in:** Sign in with **OpenID Connect**, an issuer, a client id, a client secret, the public URL; **Save
   sign-in**. With a real provider (see [identity.md](identity.md)), viewers then sign in. Without one:
   an issuer without a client id is refused with the host's message. The secret never shows again
   ("Set (never shown)"). **Off**, Save: sign-in is off.
2. **Policy:** type `[[rule]]` / `name = "x"` / `view_ony = true`, **Check and save**. **Expect:** red,
   "policy: unknown field `view_ony`, expected one of …" with the line; the policy is unchanged. Fix it
   to `view_only = true` and save: "1 rule" and its file; the phone's taps stop doing anything.
   **Turn the policy off** (asked first).
3. **Live sessions:** each session's rule, groups, clipboard, sound, recorded; **Kick** works here too.
4. **Audit log:** the newest first. Press `session_kicked` (and other events), a user, a date; **Search**.
   **Older** / **Newest** page through. The kick and the unpair are there.
5. **Metrics:** the counters climbing while the phone watches (frames, bytes), every 2 seconds.

## The host goes away

1. `pkill -f "rein-remote host"`. **Expect:** a bar "rein-remote host stopped; looking for it again…";
   the app keeps running; within seconds it starts a new host and the bar goes (the links have a new
   token: paired devices come back without a code; people with links need new ones).
2. Start the app with `--no-start` and no host running: the bar says the host isn't running and why,
   and stays until you run `rein-remote host` in a terminal; then the app connects to it.
3. **Quit** from the tray menu. **Expect:** the icon goes, and the host the app started stops (sharing
   ends). A host started at login (not by the app) keeps running.

## Desktops, theme and accessibility

1. Do the above on **GNOME** (with the AppIndicator extension) and on **KDE Plasma**. On KDE the icon
   is in the system tray (maybe under the arrow); its tooltip says the status.
2. Switch the system to dark (GNOME Settings, Appearance; KDE Global Theme, Breeze Dark). **Expect:**
   the app follows at once; QR codes stay black on white.
3. **Orca** (Super+Alt+S): Tab through each page. **Expect:** every button, box and switch is read with
   a name (for example "Kick Gary's iPhone", "Copy the link for this network", "Pairing code").
4. Make the window narrow (under 640 px): the sidebar folds away, with a back button.

Note what didn't match, with `~/.rein-remote/host.log` and the app's output (run it from a terminal).
