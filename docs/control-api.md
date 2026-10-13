# The host and its control API

`rein-remote host` is the long-running process behind the platform apps (SwiftUI on macOS, WinUI 3
on Windows, GTK4 on Linux). It owns the sharing state and runs the same server as `rein-remote serve`
inside it, so sharing starts and stops without the process exiting. The apps drive it through a
small HTTP + JSON API on this machine, the **control API**, and follow it through a stream of events.

This document is the whole contract: an app team should never need to read the server's code.

- [Running the host](#running-the-host)
- [Files](#files)
- [Connecting: `control.json`, auth, errors](#connecting)
- [Events: `GET /control/events`](#events)
- [State and sharing](#state-and-sharing)
- [Sources](#sources)
- [Pairing and paired devices](#pairing-and-paired-devices)
- [Viewers](#viewers)
- [Settings](#settings)
- [Start at login](#start-at-login)
- [macOS permissions](#macos-permissions)
- [Admin: sign-in, policy, sessions, audit log, metrics](#admin)
- [Endpoint index](#endpoint-index)

## Running the host

```sh
rein-remote host                              # settings from ~/.rein-remote/host.toml
rein-remote host --config /path/to/host.toml  # another settings file
rein-remote host --test-pattern 640x480       # any `serve` flag: wins over host.toml for this run
rein-remote host --control-listen 127.0.0.1:7980   # a fixed port for the API (default: a random one)
rein-remote host --sharing on                 # share now, whatever the settings say (for this run)
```

- **Not sharing is a valid state.** The host starts with sharing off unless the settings say
  `sharing = true` (which `POST /control/sharing/start` and `/stop` keep up to date, so the host comes
  back the way the user left it).
- **One host per user.** A second `rein-remote host` for the same home folder refuses to start while
  the first answers (it says which pid).
- **`serve` flags on the command line** (`--test-pattern`, `--listen`, `--fps`…) win over `host.toml`
  for this run and are not written to it. A setting changed through the API wins over the command
  line from then on, and is written.
- The host prints what `serve` prints (its `on http://ADDR` line, the links) on stderr, and
  `rein-remote: control API on http://127.0.0.1:PORT` once the API listens.
- Apps typically start it as a child process (or install it to start at login, below) and then read
  `control.json`.

**Remote control** is off. `--control-listen` takes a loopback address; anything else is refused
unless `--control-remote on` is given too, and then only with HTTPS (the host's `tls-cert` and
`tls-key` settings), so the token never crosses a network in the clear. Same token, same API.

## Files

All in the host's **home folder**: `$REIN_REMOTE_HOME` when set, else `~/.rein-remote` on macOS and
Linux and `%APPDATA%\rein-remote` on Windows (the same folder as this machine's key and its paired
devices).

| File | What | Who writes it |
|---|---|---|
| `host.toml` | The settings (below). | You, or the host when they change through the API. |
| `control.json` | Where the API is and its token. Owner-only. | The host, each time it starts. |
| `audit.jsonl` | The audit log, unless `audit` says elsewhere (or `off`). | The host. |
| `policy.toml` / `policy.json` | The policy, when set through the API. | The host (`PUT /control/admin/policy`). |
| `oidc-client-secret`, `relay-secret` | Secrets set through the API. Owner-only. | The host. |
| `identity.pem`, `paired` | This machine's key, the paired devices (as for `serve`). | The host. |

`host.toml` uses the same format as `serve --config`: **each key is a `serve` flag's name without its
dashes**, plus three keys of the host's own (`sharing`, `control-listen`, `control-remote`).
Secrets go in files of their own, as `{ file = "NAME" }` (relative to the home folder).

```toml
# ~/.rein-remote/host.toml
sharing = true                  # share when the host starts
display = "1"                   # the source: display / window / app / test-pattern / vnc (one of them)
prefer = "framerate"            # or "quality"
fps = "auto"
bitrate = "auto"
codec = "h264"
audio = true
listen = "auto"                 # every interface, the first free port from 7979, behind the link's token
mdns = true
pair = true
relay = ["wss://relay.example"]
relay-secret = { file = "relay-secret" }
audit = "/Users/me/.rein-remote/audit.jsonl"
```

### `control.json`

```json
{"url":"http://127.0.0.1:52817","token":"4f9c0d…(64 hex)","pid":41237,"version":"0.1.1","api":1}
```

- Written when the API is listening. It stays when the host ends; a host that starts finds the old
  one's API not answering and writes its own. So: the file and a `GET /control/state` that answers
  mean the host is up.
- **Owner-only.** On macOS and Linux it's created with mode `0600` (the folder is the owner's too).
  On Windows it's in the user's profile (`%APPDATA%`, which only the user, SYSTEM and Administrators
  can read), and the host also removes inherited permissions from the file and grants full control to
  its owner only (`icacls FILE /inheritance:r /grant:r *S-1-3-4:F`, S-1-3-4 being "Owner Rights").
- The token is new each time the host starts. Read the file again after restarting the host.

## Connecting

Every request carries the token as a bearer:

```http
GET /control/state HTTP/1.1
Host: 127.0.0.1:52817
Authorization: Bearer 4f9c0d…
```

- No token, or a wrong one: `401 {"error":"…"}`. The token is compared in constant time.
- **Never in a URL**: a `?token=` is not looked at. (A web view can't open the API: browsers don't
  send this header across origins without a preflight the API doesn't answer.)
- A `Host` header other than `127.0.0.1:PORT`, `localhost:PORT` or `[::1]:PORT` is refused (`403`),
  so a web page can't reach the API through DNS rebinding.
- Requests and answers are JSON (`Content-Type: application/json`, UTF-8). Bodies up to 1 MB.
  Each answer closes its connection (`Connection: close`); nothing needs keep-alive.

### Errors

Every error is JSON with one `error` string written to be shown to a person as it is:

```json
{"error":"no viewer has that id (it may have just left)"}
```

| Status | When |
|---|---|
| 400 | The request is wrong: a bad value, a policy that doesn't parse (with the parser's message). |
| 401 | No token or the wrong token. |
| 403 | A refused `Host` header. |
| 404 | No such endpoint, or no such viewer, device, source. |
| 405 | The endpoint exists, not with this method. |
| 409 | Not in this state: starting what's already sharing, kicking when not sharing, pairing that's off. |
| 413 | A body over 1 MB. |
| 500 | It failed: a port in use, a source that won't open, a file that can't be written. |

Times are RFC 3339 in UTC (`2026-10-13T09:30:00.123Z`). Keys that mirror `host.toml` are written as
the flags are (`relay-secret`); every other key is `snake_case`.

## Events

`GET /control/events` answers with **Server-Sent Events** (`Content-Type: text/event-stream`) and stays
open. Every platform's HTTP client can read it as a streamed body (URLSession `bytes(for:)`,
`HttpClient` with `HttpCompletionOption.ResponseHeadersRead`, libsoup's input stream); a line-based
reader is enough:

```text
event: state
data: {"api":1,"sharing":true, … the same object as GET /control/state}

event: viewers
data: {"viewers":[ … the same rows as GET /control/viewers ]}

: keepalive
```

- **On connect**, one of each: `state`, `viewers`, `devices`, `settings`. So an app subscribes and
  draws from the events alone, with no GET-then-subscribe race.
- **Then each again when it changes**: `state` (sharing on or off, the source, the links, pairing:
  a code made, used or expired, `restart_needed`, an error), `viewers` (one joins or leaves, and once
  a second while any are connected, for their fps and round trip), `devices` (one paired or
  unpaired), `settings` (changed through the API).
- Each event's `data` is the full object, never a diff. Changes are seen within a second.
- A comment line (`: keepalive`) every 15 seconds keeps idle connections open.
- When the host exits, the stream ends. Reconnect after reading `control.json` again.

## State and sharing

### `GET /control/state`

```json
{
  "api": 1,
  "version": "0.1.1",
  "pid": 41237,
  "sharing": true,
  "error": null,
  "source": "display:1",
  "listen": "0.0.0.0:7979",
  "links": [
    {"kind": "tailnet", "label": "your tailnet", "url": "https://box.tail1234.ts.net:7979/?token=k3…", "qr": "https://box.tail1234.ts.net:7979/?token=k3…"},
    {"kind": "lan", "label": "this network", "url": "http://192.168.1.20:7979/?token=k3…", "qr": "http://192.168.1.20:7979/?token=k3…"},
    {"kind": "local", "label": "this computer", "url": "http://localhost:7979/?token=k3…", "qr": "http://localhost:7979/?token=k3…"},
    {"kind": "relay", "label": "through relay.example", "url": "https://relay.example/?room=…#k=…", "qr": "https://relay.example/?room=…#k=…"}
  ],
  "pairing": {
    "enabled": true,
    "code": "123-456",
    "expires_at": "2026-10-13T09:40:00.000Z",
    "url": "http://192.168.1.20:7979/#pair=123456&id=abcd-efgh-ijkl-mnop",
    "qr": "http://192.168.1.20:7979/#pair=123456&id=abcd-efgh-ijkl-mnop"
  },
  "viewers": 1,
  "restart_needed": false
}
```

- `sharing`: whether the server is running. `error`: why the last start failed (or `null`).
- `source`: what's shared, as a [source id](#sources) (also when not sharing: what will be).
- `links`: one per way to reach this machine, best first. **Each is the key to the screen** (the
  token is in it; a relay link has its end-to-end key after `#`): show them as such. `qr` is the text
  to draw as a QR code (the same as `url`). `kind` is `tailnet`, `lan`, `local` or `relay`. Empty
  when not sharing. When the host listens on one address only (`listen` set to one), one `local` link
  for that address. The token in them lasts as long as the host process; stopping and starting
  sharing keeps it.
- `listen`: where viewers connect (`ADDR:PORT` while sharing; the setting when not).
- `pairing`: `enabled` is the `pair` setting as it applies now (pairing needs sharing on); `code` is the
  open code (`null` when there's none: used, expired, five wrong tries, or pairing off). `url`/`qr`:
  the link a phone's camera opens to pair with this code. The code lasts 10 minutes and pairs one
  device.
- `viewers`: how many are connected. `restart_needed`: settings were changed while viewers were
  connected; they apply once sharing restarts (`POST /control/sharing/restart`).

### `POST /control/sharing/start`

No body. Starts the server with the settings and source as they are, and remembers `sharing = true`.
Answers with the new state (as `GET /control/state`).

- `409 {"error":"already sharing"}`.
- `500 {"error":"can't listen on 0.0.0.0:7979: Address already in use"}`: it didn't start; `state.error`
  says the same until the next start.

### `POST /control/sharing/stop`

Optional body `{"reason":"…"}` (shown to viewers). Every viewer is told (a `Notice`, code 3, whose
text is the reason, or "the host stopped sharing") and their sessions end; the server stops listening (its ports are free when
this answers); `sharing = false` is remembered. Answers with the new state. `409` when not sharing.

### `POST /control/sharing/restart`

Stop then start (viewers are told and can reconnect at once), to apply settings changed while
viewers were connected. `409` when not sharing.

## Sources

A **source id** names what's shared, as policies name it:

| Id | What |
|---|---|
| `display` | The main display (Linux: `$DISPLAY`; Windows: the primary display). |
| `display:N` | macOS: display N (1 is the main one); Linux: X display `:N`. |
| `window:ID` | One window, by the id `GET /control/sources` gives (on Linux, an X window id in decimal). |
| `app:NAME` | macOS: all of an app's windows. |
| `test-pattern`, `test-pattern:WxH` | A moving test pattern (1280×720 by default): for testing. |
| `vnc:HOST:PORT` | A VNC display (a VM, an emulator). |

### `GET /control/sources`

What can be shared now. Listing windows can take a moment (up to a second on macOS); call it when
the picker opens, not on a timer.

```json
{
  "displays": [
    {"id": "display:1", "name": "Display 1 (main)", "width": 3024, "height": 1964},
    {"id": "display:2", "name": "Display 2", "width": 2560, "height": 1440}
  ],
  "windows": [
    {"id": "window:4242", "app": "Safari", "title": "Apple", "width": 1440, "height": 900}
  ],
  "apps": [
    {"id": "app:Safari", "name": "Safari", "windows": 2}
  ],
  "other": [
    {"id": "test-pattern", "name": "Test pattern"}
  ],
  "error": null
}
```

- Linux: one entry in `displays` (`display`), the X windows in `windows`, no `apps`.
  Windows: `display` (the primary display) and the top-level windows, no `apps`.
- macOS without the Screen Recording permission: `displays`, `windows` and `apps` are empty and
  `error` says why. Listing never asks macOS to prompt (see [permissions](#macos-permissions)).
- A listing that fails (no X display) is empty with `error` set; the request still succeeds.

### `PUT /control/source`

```json
{"source": "window:4242"}
```

Switches what's shared and remembers it in `host.toml`. **While sharing, open sessions switch
live**: each viewer gets a new `Config` (the new size) and a keyframe, with no reconnect. A viewer
whose policy rule doesn't allow the new source is told why and its session ends. Answers with the
new state.

- `400` for an id that isn't one; `500` when it can't be opened (a window that closed, no
  permission): what was shared before stays.
- **Sound** stays with the source sharing started with until sharing restarts (macOS captures an
  app's or a display's sound with its picture).

## Pairing and paired devices

### `GET /control/pairing`

```json
{"enabled": true, "code": "123-456", "expires_at": "2026-10-13T09:40:00.000Z", "expires_in_s": 512,
 "url": "http://192.168.1.20:7979/#pair=123456&id=abcd-efgh-ijkl-mnop", "qr": "http://192.168.1.20:7979/#pair=123456&id=abcd-efgh-ijkl-mnop",
 "fingerprint": "abcd-efgh-ijkl-mnop"}
```

`fingerprint` is this machine's (what a paired device pins and what discovery shows). When pairing
is off or not sharing: `{"enabled": false, "code": null, …}`.

### `POST /control/pairing/code`

A new code (any open one stops working). Answers as `GET /control/pairing`.
`409 {"error":"pairing is off (turn on the pair setting)"}` or `409 {"error":"not sharing"}`.

### `GET /control/devices`

```json
{"devices": [{"id": "wxyz-1234-abcd-5678", "name": "Gary's iPhone", "since": "2026-10-01T18:02:11.000Z"}]}
```

### `DELETE /control/devices/{id}`

Unpairs the device (`id` as listed; a unique start of it works too). Its open sessions end (with a
`Notice`), and it must pair again with a new code. Answers `{"devices":[…]}`. `404` for an unknown id.

## Viewers

### `GET /control/viewers`

One row per session (a viewer that reconnects is a new row):

```json
{"viewers": [
  {"id": "5f1c2a9b0e7d4c31", "device": "Gary's iPhone", "user": "Gary's iPhone (wxyz-1234-abcd-5678)",
   "via": "paired", "transport": "webtransport", "addr": "192.168.1.30:51234",
   "fps": 60, "rtt_ms": 4.2, "kbps": 8000, "width": 1280, "height": 720,
   "since": "2026-10-13T09:31:02.481Z", "view_only": false}
]}
```

- `device`: the name the viewer's app gave (its machine name). `user`: who the server took them for:
  a signed-in user's name, `token` for a link, a paired device's name and fingerprint, `relay viewer`.
- `via`: how they came in: `token`, `paired`, `oidc`, `saml`, `relay` (`open` when nothing guards the
  server: listening on this computer only, with no token).
- `transport`: `websocket`, `webtransport` or `relay`.
- `fps`: frames sent in the last second (a still screen sends none); `rtt_ms`: the last round trip;
  `kbps`: what the controller sends now; `width`/`height`: the picture now (smaller on a slow link).
- `view_only`: the policy drops their input.

### `POST /control/viewers/{id}/kick`

Optional body `{"reason": "…"}`. The viewer is sent a `Notice` (code 3, `NOTICE_ENDED`) whose text
is "the host ended this session" (with the reason: "the host ended this session: REASON"), then the
session closes; it's written to the audit log
(`session_kicked`, then `session_end` with reason `kicked`). Answers `{"viewers":[…]}` (without it).
`404` when no session has that id.

The viewer may reconnect with what it has (its link, its pairing). To keep a device out, unpair it;
to keep a link out, restart the host (a new token).

## Settings

### `GET /control/settings`

The settings as they apply, defaults filled in. Keys are `host.toml`'s (flag names):

```json
{
  "settings": {
    "prefer": "framerate",
    "fps": "auto",
    "bitrate": "auto",
    "codec": "h264",
    "audio": true,
    "listen": "auto",
    "mdns": true,
    "pair": true,
    "relay": ["wss://relay.example"],
    "relay-secret-set": true,
    "webtransport": true,
    "tailscale-cert": true,
    "record": null,
    "audit": "/Users/me/.rein-remote/audit.jsonl"
  },
  "sharing": true,
  "source": "display:1",
  "path": "/Users/me/.rein-remote/host.toml",
  "overridden": ["test-pattern"],
  "restart_needed": false
}
```

| Key | Values | Default |
|---|---|---|
| `prefer` | `framerate`: on a slow link the bitrate, then the picture's size give way before frames do (games, motion). `quality`: frames give way first, the picture keeps its size until the frame rate is at its least (text, detail). | `framerate` |
| `fps` | `"auto"` (the viewer's display rate, up to 240; 60 with a software encoder) or 1–240. | `auto` |
| `bitrate` | `"auto"` (from the picture's size and rate) or the most kbit/s to send. | `auto` |
| `codec` | `h264`, `h265` or `auto` (HEVC where both sides have it in hardware). | `h264` |
| `audio` | `true`/`false`: this machine's sound to viewers. | `true` |
| `listen` | `"auto"` (every interface, first free port from 7979, the token in the links) or `ADDR:PORT` (`127.0.0.1:0`: this computer only, any port). | `auto` |
| `mdns` | Found by name on the local network. | `true` |
| `pair` | Pairing with a code. | `true` |
| `relay` | Relays to wait on (`ws://`/`wss://`), for viewers on no shared network. | `[]` |
| `relay-secret` | **Write-only**: the relays' secret for hosts. GET shows `relay-secret-set`. | none |
| `webtransport` | QUIC/WebTransport next to the WebSocket. | `true` |
| `tailscale-cert` | A real certificate from Tailscale for the tailnet name. | `true` |
| `record` | A folder to record sessions to (MP4), or `null`. | `null` |
| `audit` | The audit log's file, or `"off"`. | `audit.jsonl` in the home folder |

- `sharing`: whether the host shares when it starts (set by start/stop, not by PUT).
- `overridden`: keys the command line set for this run (they win over the file until the API changes them).

### `PUT /control/settings`

A partial object: only the keys given change; `null` puts a key back to its default.

```json
{"prefer": "quality", "fps": 60, "relay-secret": "s3cret"}
```

Answers as `GET /control/settings`, plus `"applied"`:

- `"saved"`: not sharing; used the next time sharing starts.
- `"restarted"`: sharing with no viewers: sharing was restarted to apply it (the links stay the same
  unless `listen` changed).
- `"on_restart"`: viewers are connected, so nothing was interrupted; `restart_needed` is now true
  (in the state too) until `POST /control/sharing/restart` or the next start.

Errors: `400 {"error":"fps: auto or 1 to 240"}`, `400 {"error":"unknown setting \"fsp\""}`. Nothing is
changed when any key is refused. Settings are written to `host.toml` (its other keys are kept; comments are not).

## Start at login

### `GET /control/start-at-login`

```json
{"enabled": true, "kind": "launchd", "name": "rein-remote-host",
 "path": "/Users/me/Library/LaunchAgents/com.rein-harness.rein-remote-host.plist"}
```

`kind`: `launchd` (a LaunchAgent in the user's session), `systemd` (a `systemctl --user` unit:
`~/.config/systemd/user/rein-remote-host.service`) or `task` (a Windows scheduled task at logon,
`rein-remote-host`; `path` is `null`). It runs `rein-remote host` (this binary, by its full path),
which reads `host.toml`. This is `rein-remote install-service host` (the user's service).

### `PUT /control/start-at-login`

```json
{"enabled": true}
```

Installs it (or removes it with `false`) for the **next** login: it neither starts a second host now
nor stops this one. Answers as GET. `500` with the service manager's message when it fails.

An app bundle that has its own login item (macOS `SMAppService`, an MSIX startup task) can use that
instead and leave this off.

## macOS permissions

### `GET /control/permissions`

```json
{"platform": "macos", "screen_recording": "granted", "accessibility": "denied"}
```

On macOS: `granted` or `denied`, read with `CGPreflightScreenCaptureAccess()` and
`AXIsProcessTrusted()`. **These never make macOS prompt.** On Linux and Windows both are
`"not_needed"`.

- Screen Recording is needed to share anything but a test pattern or VNC; Accessibility for viewers
  to control the Mac (clicks and keys).
- macOS grants these to the **responsible process**: the app that started `rein-remote host` as its
  child, or the `rein-remote` binary itself when launchd started it (start at login). Show the right
  name in the app's instructions.
- macOS applies a new Screen Recording grant to a process that's already running only after it
  restarts: restart the host after the user grants it.

### `POST /control/permissions/open`

```json
{"which": "screen_recording"}
```

`which`: `screen_recording` or `accessibility`. The host opens System Settings at that pane (`open
x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture`, or
`?Privacy_Accessibility`), where the user turns it on, and answers
`{"opened": true, "url": "x-apple.systempreferences:…"}`. An app that would rather open it itself
sends `{"which": "…", "open": false}` and opens `url`. Not macOS: `{"opened": false, "url": null}`.

## Admin

The admin pages use the same token: whoever can read `control.json` owns this machine's sharing.

### Sign-in: `GET /control/admin/signin`

```json
{
  "kind": "oidc",
  "public-url": "https://remote.example.com",
  "oidc-issuer": "https://example.okta.com",
  "oidc-client-id": "0oa1b2c3",
  "oidc-client-secret-set": true,
  "oidc-scopes": "openid email profile",
  "oidc-user-claim": "email",
  "oidc-groups-claim": "groups",
  "saml-idp-metadata": null,
  "saml-entity-id": null,
  "saml-user-attr": null,
  "saml-groups-attr": "groups",
  "saml_available": false
}
```

`kind`: `oidc`, `saml` or `null` (off). Secrets are **never returned**: `oidc-client-secret-set` says
whether there is one. `saml_available`: whether this build has SAML (built with `--features saml`).
The keys are the `serve` flags of the same names (README: "sign-in with your identity provider").

### `PUT /control/admin/signin`

A partial object as above. `oidc-client-secret` is **write-only**: it's kept in `oidc-client-secret`
(owner-only) in the home folder, and `host.toml` names that file. `{"oidc-issuer": null,
"saml-idp-metadata": null}` turns sign-in off.

- `400` when it's incomplete: an issuer without a client id or `public-url`, a `public-url` that isn't
  `http(s)://` or has a query, SAML in a build without it.
- The provider itself is reached when sharing (re)starts; an unreachable one shows in
  `state.error`. Applied like settings (`applied`, `restart_needed`).

### Policy: `GET /control/admin/policy`

```json
{"enabled": true, "path": "/Users/me/.rein-remote/policy.toml", "format": "toml", "rules": 2,
 "text": "[[rule]]\nname = \"admins\"\ngroups = [\"rr-admins\"]\n\n[[rule]]\nname = \"the link\"\nvia = [\"token\", \"paired\"]\n"}
```

Off: `{"enabled": false, "path": null, "format": null, "rules": 0, "text": null}`. The format is the
README's `--policy` (TOML or JSON): rules matched in order, the first that matches decides.

### `PUT /control/admin/policy`

```json
{"text": "[[rule]]\nname = \"everyone, view only\"\nview_only = true\n", "format": "toml"}
```

The text is checked with the server's own policy parser first; a bad one is refused with its
message and nothing changes:

```json
{"error":"policy: unknown field `view_ony`, expected one of `name`, `users`, … at line 2 column 1"}
```

A good one is written to the policy file (the one in use, else `policy.toml`/`policy.json` in the
home folder) and `host.toml`'s `policy` names it. `{"text": null}` turns the policy off (the file is
kept). Applied like settings (`applied`, `restart_needed`). Answers as GET plus `applied`.

### Live sessions: `GET /control/admin/sessions`

`{"sessions": [ … ]}`: the rows of `GET /control/viewers`, each with the policy's view of it too: `groups` (from the
identity provider), `rule` (the policy rule that let them in; `none` without a policy), `clipboard`,
`audio`, `recorded`, and `ends_at` (a time limit's end, or `null`). Kick with
`POST /control/viewers/{id}/kick`.

### Audit log: `GET /control/admin/audit`

Newest first, a page at a time:

```http
GET /control/admin/audit?event=session_start,session_end&user=alice&since=2026-10-12T00:00:00Z&limit=50
```

| Query | What |
|---|---|
| `event` | Event types, comma-separated (exact): `signin`, `signin_denied`, `signout`, `session_start`, `session_end`, `session_refused`, `session_kicked`, `pair`, `pair_failed`, `device_signin`, `device_signin_failed`, `unpair`, `server_start`, `sharing_start`, `sharing_stop`, `settings_changed`, `policy_changed`, `signin_changed`. |
| `user` | Entries whose `user` contains this (any case). |
| `since` | Entries at or after this time (RFC 3339, or a date `2026-10-12`). |
| `limit` | At most this many (default 100, at most 1000). |
| `before` | The `next` of the previous page: the page after it (older). |

```json
{
  "path": "/Users/me/.rein-remote/audit.jsonl",
  "entries": [
    {"ts": "2026-10-13T09:35:12.004Z", "event": "session_end", "session": "5f1c2a9b0e7d4c31", "user": "alice@corp.example", "via": "oidc", "reason": "kicked", "seconds": 250.1, …},
    {"ts": "2026-10-13T09:31:02.481Z", "event": "session_start", "session": "5f1c2a9b0e7d4c31", "user": "alice@corp.example", …}
  ],
  "next": "1841"
}
```

Each entry is the log's line as it is (README: "an audit log"; never a token or secret). `next` is
`null` on the last page. With audit `off`: `{"path": null, "entries": [], "next": null}`.

### Metrics: `GET /control/admin/metrics`

The same counters as `/metrics` (Prometheus), as JSON:

```json
{
  "version": "0.1.1",
  "uptime_s": 3600,
  "viewers": {"direct": 1, "relayed": 0},
  "sessions_total": 12,
  "frames_sent_total": 182344,
  "keyframes_sent_total": 31,
  "video_bytes_sent_total": 911203344,
  "frames_skipped_total": 12,
  "direct_offers_total": 2,
  "direct_answers_total": 2,
  "relays": [{"relay": "relay.example:443", "up": true, "rtt_ms": 12.5, "registered": true, "registrations": 3}]
}
```

Counters count from when the host process started (across stops and starts of sharing).

## Endpoint index

| Method | Path | |
|---|---|---|
| GET | `/control/events` | Server-Sent Events: `state`, `viewers`, `devices`, `settings` |
| GET | `/control/state` | Sharing, source, links, pairing |
| POST | `/control/sharing/start` | |
| POST | `/control/sharing/stop` | `{"reason"}` optional |
| POST | `/control/sharing/restart` | |
| GET | `/control/sources` | Displays, windows, apps |
| PUT | `/control/source` | `{"source"}`: switched live |
| GET | `/control/pairing` | The code, its expiry, the QR |
| POST | `/control/pairing/code` | A new code |
| GET | `/control/devices` | Paired devices |
| DELETE | `/control/devices/{id}` | Unpair |
| GET | `/control/viewers` | Sessions |
| POST | `/control/viewers/{id}/kick` | `{"reason"}` optional |
| GET, PUT | `/control/settings` | |
| GET, PUT | `/control/start-at-login` | `{"enabled"}` |
| GET | `/control/permissions` | macOS: Screen Recording, Accessibility |
| POST | `/control/permissions/open` | `{"which","open"}` |
| GET, PUT | `/control/admin/signin` | OIDC/SAML; secrets write-only |
| GET, PUT | `/control/admin/policy` | `{"text","format"}`, checked by the parser |
| GET | `/control/admin/sessions` | Viewers, with the policy's view |
| GET | `/control/admin/audit` | `event`, `user`, `since`, `limit`, `before` |
| GET | `/control/admin/metrics` | Counters as JSON |

### What a viewer sees

Nothing in the protocol changed for this. When the host ends a session (a kick, sharing stopped, a
device unpaired, a source its policy doesn't allow), the viewer first gets rr-proto's `Notice`
(existing message; code `3`, `NOTICE_ENDED`, "ended by the host", or `2` for a policy refusal) with
text to show, then the session closes. A source switch is a `Config` with the new size and then a
keyframe, as when the picture is scaled for a slow link.
