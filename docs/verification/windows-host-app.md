# The Windows host app on a real PC

The Windows app (`clients/windows-host`, WinUI 3, unpackaged) is the UI for `rein-remote host`: a
notification-area icon and a window with Sharing, Viewers, Paired devices, Settings and the admin
pages (Sign-in, Policy, Live sessions, Audit log, Metrics). It talks to the host only through the
control API ([docs/control-api.md](../control-api.md)).

**What CI checks** (the `windows-host` job, on `windows-latest`): the API client's unit tests against
every JSON example in docs/control-api.md, the app built, and a UI test (FlaUI, UI Automation) that
drives the real app against a real `rein-remote host --test-pattern 1280x720` on a home folder of its
own: start and stop, the source switched (the display, then the test pattern), a new pairing code, a
viewer (`rein-remote connect`) listed then kicked (asked first; gone at the host; `session_kicked` in
the audit log), a second viewer in Live sessions, `fps = 30` saved to `host.toml` and a refused
value's message shown, a bad policy refused with the parser's message and a good one saved, the audit
log filtered to the kick, the host killed (the app says so and keeps running) and a new host found on
its own. A second test starts the app with no host running: it starts the `rein-remote.exe` beside
it, shares on every interface, and shows the links' QR code (in the dark theme). Screenshots of each
page are the job's `windows-host-app` artifact; a few are in
[windows-host-app/](windows-host-app/).

**What it can't check**, and this test is for: a real desktop and its notification area (the tray
icon and its menu), the firewall prompt, a phone on the network pairing and watching, a real display
and windows in the source picker, the system theme switched live, Narrator, start at login.

## Get the app

Either:

- **From CI:** a pull request's run, the `windows-host-app-build` artifact: unzip it anywhere
  (`C:\Program Files\Rein Remote` or `%LOCALAPPDATA%\Programs\Rein Remote`). It has
  `ReinRemoteHost.exe` and `rein-remote.exe` side by side, the .NET runtime and the Windows App SDK
  in it: nothing else to install.
- **From source** (Windows 10 1809 or later, the .NET 10 SDK, Rust):

  ```powershell
  cargo build --release -p rr-server
  dotnet publish clients/windows-host/ReinRemote.Host/ReinRemote.Host.csproj -c Release -p:Platform=x64 -o "$env:LOCALAPPDATA\Programs\Rein Remote"
  ```

  The publish copies `target\release\rein-remote.exe` next to the app when it's there; check it is.

## First run

1. Start `ReinRemoteHost.exe`. **Expect:** a window "Rein Remote: Not sharing" and a grey monitor
   icon in the notification area (it may be in the overflow, `^`). The window shows "Starting
   rein-remote host…" for a moment, then the bar goes away.
   - `%APPDATA%\rein-remote\` now has `control.json`, `host.log` (the host's output) and `app.log`.
   - Task Manager, Details: `ReinRemoteHost.exe` and its child `rein-remote.exe` (`host`).
2. Start `ReinRemoteHost.exe` again. **Expect:** no second app; the first one's window comes forward.
3. Close the window (the X). **Expect:** the app stays in the notification area. Click the icon: the
   window comes back.

## Share, and the firewall

1. Sharing page, **Start sharing**. **Expect:** Windows Defender Firewall asks about
   `rein-remote.exe` the first time (it listens on port 7979, TCP and UDP). Allow it on **Private**
   networks. Sharing shows a green dot and "Sharing, no one watching"; the tray icon turns blue with a
   green dot.
   - If you say Cancel: sharing still runs, but phones on the network can't reach it; the links for
     "this computer" work. Allow it later in *Windows Security, Firewall, Allow an app*.
2. **Links:** one per way to reach this PC ("this network", "your tailnet" with Tailscale, "this
   computer"). Select one: its QR code shows (black on white, in either theme). **Copy** puts it on
   the clipboard; paste it into a browser on another device on the network. **Expect:** the screen,
   live.
3. **What's shared:** open the picker. **Expect:** "Display: …" and each top-level window. Pick a
   window: the browser's picture switches to it without reconnecting. Pick the display again.

## Pair a phone

1. Install the Rein Remote app on a phone (iOS or Android) on the same network.
2. Sharing page, **Pair a device**: a code like `123-456`, its countdown (10:00 down), and a QR code.
   In the phone's app, pick this PC (found by name) and type the code, or scan the QR code.
   **Expect:** the phone shows the screen; the code is gone (a code pairs one device); **New code**
   makes another, and the old one stops working.
3. **Paired devices:** the phone, its id, when it paired.
4. Right-click the tray icon. **Expect:** the status, "1 viewer", "Pairing code …, copy", Stop sharing,
   New pairing code, Open, Quit. A balloon said "… is watching this screen" when the phone joined.

## Kick and unpair

1. **Viewers:** the phone, with its user, how it came in (`paired`), its transport, fps, size,
   bitrate, round trip. The nav shows a badge with the count.
2. **Kick**, type a reason, **Kick**. **Expect:** the phone says "the host ended this session: REASON"
   and closes; the row goes. The phone can connect again (it's still paired).
3. **Paired devices, Unpair**, confirm. **Expect:** its session ends (if it had one), and it needs a
   new code to come back.

## Settings

1. Change **Frames a second** to `30`, **Save**. **Expect:** "Saved…" (or "viewers are connected, so
   they apply when sharing restarts", with **Restart sharing now**). `%APPDATA%\rein-remote\host.toml`
   has `fps = 30`.
2. Type `500` and Save. **Expect:** the host's message (`fps: "auto" or 1 to 240`) in red; nothing
   changed.
3. With the phone watching, turn **Sound** off and Save. **Expect:** "Viewers are connected, so they
   apply when sharing restarts" and **Restart sharing now**; press it: the phone is told, and when it
   connects again it has no sound.
4. **Relays:** add `wss://relay.example` and a secret, Save. **Expect:** the secret field empties and
   says "A secret is set."; `host.toml` has `relay-secret = { file = "relay-secret" }`, never the
   secret.
5. **Start at login** on. **Expect:** Task Scheduler has `rein-remote-host` (at logon). Sign out and
   in: `rein-remote.exe host` runs before the app is opened, and the app (when opened) connects to it
   rather than starting another. Turn it off: the task is gone.

## Admin

1. **Sign-in:** OIDC, an issuer, a client id, a secret, the public address; Save. With a real
   provider (see [identity.md](identity.md)), viewers then sign in. Without one: an issuer without a
   client id is refused with the host's message. The secret never shows again ("A secret is set.").
2. **Policy:** type `[[rule]]` / `name = "x"` / `view_ony = true`, **Check and save**. **Expect:** red,
   "policy: unknown field `view_ony`, expected one of …" with the line; the policy is unchanged.
   Fix it to `view_only = true` and save: "On: 1 rule"; the phone's taps stop doing anything.
   **Turn the policy off** (asked first).
3. **Live sessions:** each session's rule, groups, clipboard, sound, recorded.
4. **Audit log:** the newest first. Filter events with **Add** (`session_kicked`), a user, a date;
   **Search**. **Older** / **Newer** page through. The kick and the unpair are there.
5. **Metrics:** the counters climbing while the phone watches (frames, bytes), every 2 seconds.

## The host goes away

1. In Task Manager, end `rein-remote.exe`. **Expect:** a red bar "Not connected"; the app keeps
   running; within a few seconds it starts a new host and the bar goes (the links have a new token:
   devices that paired come back without a code; people with links need new ones).
2. Quit from the tray menu. **Expect:** the icon goes, and the host the app started stops (sharing
   ends). A host started at login (not by the app) keeps running.

## Theme and accessibility

1. Settings, Personalization, Colors: switch between Light and Dark. **Expect:** the app follows at
   once, title bar included; QR codes stay black on white.
2. Narrator (Ctrl+Win+Enter): Tab through each page. **Expect:** every button, box and switch is read
   with a name (for example "Kick Gary's iPhone", "Copy the link for this network", "Pairing code
   123-456"); the sharing status and the connection bar are announced when they change.
3. Display scale at 150%: nothing clipped; the window fits a 1366×768 screen.

Note what didn't match, with `%APPDATA%\rein-remote\app.log` and `host.log`.
