# The macOS host app (`clients/macos-host`)

Rein Remote for macOS is a menu-bar app (SwiftUI, macOS 14+) on `rein-remote host`: it starts the
`rein-remote` inside it as `rein-remote host` when no host answers, and talks to it only through the
control API ([docs/control-api.md](../control-api.md)): the token from `control.json`, live state from
`/control/events`. Quitting the app ends the host it started; a host started otherwise (at login by
launchd, from a terminal) is left running.

## What's tested, and where

| What | How |
|---|---|
| The API client: every object in docs/control-api.md decoded (copied into `ReinRemoteHostTests/Fixtures/`), the server's errors shown as written, the event stream parsed, settings and policy bodies encoded as the doc says, the audit query, the QR code | Unit tests (`ReinRemoteHostTests`), CI job `macos-host` |
| Start and stop sharing; change the source; a viewer (`rein-remote connect`) appears, is kicked after a confirmation, disappears, and the kick shows in the audit log filtered to `session_kicked`; a new pairing code; "Quality first" written to `host.toml`; a policy with an error shows the parser's error; the menu bar's panel (status, viewers, source, pairing code, stop and start); the host killed, and the app starting the `rein-remote` inside it and following that one | UI tests (`ReinRemoteHostUITests`, XCUITest), each against a real `rein-remote host --test-pattern 1280x720 --listen 127.0.0.1:0 --mdns off` on a temporary `REIN_REMOTE_HOME`; CI job `macos-host` and on a Mac (Xcode 26.6, macOS 26.4) |
| The look, light and dark | Screenshots of each page in [macos-host-app/](macos-host-app/), taken by the UI tests (window only) |

The UI tests run like this (their runner is not sandboxed, so the host it starts can listen; on the
Mac above they ran with no prompt, as on CI's runners):

```sh
cargo build --release -p rr-server
cd clients/macos-host && xcodegen
TEST_RUNNER_RR_BIN=$PWD/../../target/release/rein-remote TEST_RUNNER_RR_SHOTS=$PWD/../../docs/verification/macos-host-app \
  xcodebuild test -project ReinRemoteHost.xcodeproj -scheme ReinRemoteHost -destination 'platform=macOS' -derivedDataPath DerivedData
```

Not done by a test: macOS's permission panes (opening System Settings and granting), start at
login, a real phone pairing, sign-in with a real provider. Those are the manual test below.

## Manual test

On a Mac with macOS 14 or newer, Xcode, Rust and XcodeGen (`brew install xcodegen`), and a phone with
Rein Remote (or a browser on another device on the same network).

### 1. Build and install

```sh
cargo build --release -p rr-server
cd clients/macos-host && xcodegen
xcodebuild -project ReinRemoteHost.xcodeproj -scheme ReinRemoteHost -configuration Release -derivedDataPath DerivedData
cp -R "DerivedData/Build/Products/Release/Rein Remote.app" /Applications/
```

- [ ] `ls "/Applications/Rein Remote.app/Contents/MacOS"` lists `Rein Remote` and `rein-remote`.
- [ ] `codesign --verify --deep --strict "/Applications/Rein Remote.app"` says nothing (ad hoc; a
  release passes `CODE_SIGN_IDENTITY="Developer ID Application: …" DEVELOPMENT_TEAM=…
  ENABLE_HARDENED_RUNTIME=YES` and is notarized as in [signing.md](signing.md)).

### 2. First run

Quit any `rein-remote host` you started yourself (`cat ~/.rein-remote/control.json` gives its pid).

- [ ] Open Rein Remote from /Applications. No Dock icon; a menu-bar icon; the window opens (first run only).
- [ ] No macOS permission prompt appears at launch.
- [ ] The sidebar's status goes from "Starting…" to "Not sharing" (or "Sharing" if `host.toml` says
  `sharing = true`). `ps -axo pid,ppid,command | grep 'rein-remote host'`: its parent is the app.
- [ ] Close the window: the app stays in the menu bar. Click the menu-bar icon: the status, Start
  sharing, the source, viewers 0, "Open Rein Remote…", "Quit". "Open Rein Remote…" brings the window back.

### 3. Permissions

- [ ] Permissions page: Screen Recording and Accessibility each say Allowed / Not allowed, and the
  text says to turn on **Rein Remote** (the host is the app's child).
- [ ] "Open System Settings" next to Screen Recording opens System Settings at Privacy & Security ›
  Screen Recording (no prompt from macOS). Turn on Rein Remote there.
- [ ] Back in the app, "Restart the host"; the status comes back within a few seconds and Screen
  Recording says Allowed. Do the same for Accessibility.

### 4. Share, and pair a phone

- [ ] Sharing page: "Start sharing". The status is "Sharing"; links appear (this network, this
  computer, your tailnet if you have one), each with Copy and a QR code.
- [ ] The source picker lists your displays, apps and windows (and the test pattern). Pick a window:
  "Now:" shows `window:…`, and a viewer already connected sees that window without reconnecting.
- [ ] "Pair a device": a code `NNN-NNN` with "Expires in 9:59" counting down, and a QR code. "New code"
  changes it.
- [ ] On the phone, scan the pairing QR code (or enter the code in Rein Remote). It connects; the
  Paired devices page lists it; the Viewers page shows it (`via paired`, its fps and round trip,
  updated every second); the menu bar says "Sharing, 1 viewer".

### 5. Kick

- [ ] Viewers page: "Kick…" on the phone's row; a confirmation; "Kick". The phone shows "the host
  ended this session"; the row disappears.
- [ ] Audit log: "All events" › `session_kicked`: the kick is listed. "Older"/"Newer" page through a
  long log (set "Per page" to 25).
- [ ] Paired devices: "Unpair…" › "Unpair": the phone's session ends and it needs a new code.

### 6. Settings

- [ ] Settings: "Quality first". `grep prefer ~/.rein-remote/host.toml` says `prefer = "quality"`;
  the page says what happened ("Sharing restarted to apply it", or with a viewer connected "it
  applies when sharing restarts" and a "Restart sharing now" button).
- [ ] Frame rate: untick Automatic, enter 300, Apply: the server's error "fps: auto or 1 to 240" is
  shown and `host.toml` is unchanged. Enter 60: saved.
- [ ] Add a relay `wss://relay.example`, set a relay secret: "set" shows, the secret is in
  `~/.rein-remote/relay-secret` (mode 600) and not in `host.toml`.
- [ ] "Open Rein Remote at login": on; log out and in: the app is in the menu bar and the host is up.
  (macOS shows its own "Login item added" notice.)
- [ ] Light and dark: switch System Settings › Appearance; every page reads well in both.

### 7. Admin

- [ ] Sign-in: OpenID Connect with an issuer but no client id, Save: the server's error is shown.
  Fill it in with a real provider ([identity.md](identity.md)), a client secret, Save: "set" shows,
  the secret is never shown again, `~/.rein-remote/oidc-client-secret` holds it.
- [ ] Policy: type `[[rule]]` then `view_ony = true`, "Check and save": the parser's error (unknown
  field `view_ony`, at line 2) is shown and nothing changes. Fix it to `view_only = true`: "On: 1 rule".
- [ ] Live sessions: a connected viewer with its rule and what it's allowed; Kick works from here too.
- [ ] Metrics: counters go up while a viewer watches (frames sent, video sent).

### 8. The host going away

- [ ] `kill $(python3 -c 'import json;print(json.load(open("'$HOME'/.rein-remote/control.json"))["pid"])')`:
  the app shows "Starting…" and is back within a few seconds with a new host (and new links).
- [ ] Quit the app: its host ends too (`pgrep -fl 'rein-remote host'` is empty). Start `rein-remote host`
  in a terminal, then the app: it uses that one and doesn't start another; quitting the app leaves it.

## Screenshots

Taken by `testScreenshotsOfEachPage` with a paired device, one viewer connected and a policy set:
[sharing](macos-host-app/sharing.png), [viewers](macos-host-app/viewers.png),
[paired devices](macos-host-app/devices.png), [settings](macos-host-app/settings.png),
[permissions](macos-host-app/permissions.png), [sign-in](macos-host-app/signin.png),
[policy](macos-host-app/policy.png), [live sessions](macos-host-app/sessions.png),
[audit log](macos-host-app/audit.png), [metrics](macos-host-app/metrics.png),
[the menu bar](macos-host-app/menu-bar.png), and in dark: [sharing](macos-host-app/sharing-dark.png),
[viewers](macos-host-app/viewers-dark.png).
