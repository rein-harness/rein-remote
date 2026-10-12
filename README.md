# Rein Remote

A fast, modern, cross-platform remote desktop: see and use a screen, a window or a single app on
another machine, from any device, with latency low enough to play a game, and quality that adapts to
whatever connection you have.

- **Every device.** Servers on Windows, macOS and Linux; clients on Windows, macOS, Linux, iOS,
  iPadOS, Android, and the web (embeddable in any site).
- **Out of the box.** Start it, pair once, connect. No ports, codecs or settings to work out.
- **Hardware first, software always.** Hardware video encoding and decoding where the machine has
  it; a software path where it doesn't (no GPU, no drivers, a VM).
- **From one machine to a fleet.** Self-hostable relays, and later sign-in with your identity
  provider, policies and audit logs.

It's what [Rein](https://github.com/rein-harness/rein-harness) uses to show you, live, what its agent
builds: native apps, windows, virtual displays. It works just as well on its own.

## Status

Early: the design is done and being built one verified piece at a time
([ROADMAP.md](ROADMAP.md)). This table lists only what has been verified.

| Piece | Status |
|---|---|
| Protocol (`rr-proto`), versioned per message | done (phase 0: CI on Linux, macOS, Windows, WebAssembly) |
| Server: Linux (X11, a real or virtual display, or one window) | working: the screen or any window (XComposite: shows as it is when covered; a press raises it), shared-memory capture, only what changed, the pointer apart, mouse, wheel and keyboard (phases 1, 3) |
| Server: macOS (any app's window, ScreenCaptureKit) | working: capture only when it changes, mouse (raises the window, like Remote Desktop), keys and text even while it's behind others (phase 3) |
| Server: Windows | todo (phase 3) |
| Client: web (WebCodecs, embeddable `<rein-remote>`) | working: decode (hardware when there is), mouse, wheel, keyboard, touch gestures and the device's on-screen keyboard (phase 1) |
| Packages (npm): `@rein-harness/remote`, `@rein-harness/remote-server` | working: the web client, and the server per platform (`npx rein-remote`), checked against Rein's contract in CI on all three (phase 2) |
| Rein previews | working: a Rein web UI chat shows a native Linux app through Rein Remote, from a phone, through a reverse proxy (phase 2) |
| Client: Windows / macOS / Linux | todo (phase 5) |
| Client: iOS / iPadOS / Android | todo (phase 5) |
| Transport: WebSocket | working, with keyframe-and-skip when the link backs up (phase 1) |
| Transport: WebTransport (QUIC) | working: a stream per frame, self-signed certificate by hash, falls back to the WebSocket by itself (phase 4) |
| Transport: relay (behind NAT, no open ports) | working: `rein-remote relay` (self-hosted), `serve --relay`; NAT hole punching todo (phase 4) |
| Adaptive bitrate, audio | todo (phase 4) |
| Pairing, enterprise sign-in and policy | todo (phases 4, 6) |

## Try it

```sh
rein-remote serve --window Simulator     # one app's window (macOS, Windows, Linux)
rein-remote serve                        # this screen (Linux: $DISPLAY)
rein-remote serve --vnc 127.0.0.1:5901   # a VNC display: a VM, an emulator (--vnc-password P)
rein-remote serve --test-pattern         # a moving test pattern, any platform
```

It prints the links to open, one per way to reach this machine, each carrying this run's key:

```
  your tailnet   https://box.tail1234.ts.net:7979/?token=…   (Tailscale: a real certificate, full quality)
  your tailnet   http://100.101.102.103:7979/?token=…
  this network   http://192.168.1.20:7979/?token=…
  this computer  http://localhost:7979/?token=…
```

Nothing to set up: on a tailnet with HTTPS turned on, rein-remote gets a real certificate for the
machine's name from Tailscale (`tailscale cert`) and serves HTTPS and plain HTTP on the same port.
Browsers give a page their video decoder only over HTTPS (or on localhost); on a plain-HTTP link the
page plays the same H.264 through Media Source instead (a little more delay). Behind your own HTTPS:
`--tls-cert` and `--tls-key`.

Measured (phase 1, software H.264, decoded in Chrome in software):

- **Glass to glass, 1280×720, on one Mac (loopback): 10 ms median, 12 ms at p95.** The test pattern
  draws its capture time as a barcode; the client reads it back from each decoded frame
  (`rein-remote serve --test-pattern`, then open `http://127.0.0.1:7979/?measure`). That ends when
  the frame is drawn in the page; the display's next refresh adds up to one more frame.
- A terminal scrolling full-screen on X11 in a 4-core x86 VM (server-side estimate from the
  client's feedback, which reads high by up to a frame because the server reads replies once a
  frame: 27 ms where the barcode says 10):

| Screen | Capture to decoded (p50 / p95, upper bound) | Read the screen | Encode |
|---|---|---|---|
| 1920×1080 | 26 ms / 36 ms | 1.7 ms (shared memory) | 16 ms |
| 1280×720 | 33 ms / 44 ms | 3 ms (GetImage) | 19 ms |

A still screen is neither read nor encoded (XDamage): one refresh a second. Hardware encoders are
what come next (see the roadmap).

### Optional: a relay, for a machine on no network you share

On a tailnet or a local network you need nothing more (above). Only to reach a machine you share no
network with, run a relay of your own somewhere both can reach:

```sh
rein-remote relay --listen 0.0.0.0:7990 --secret S        # on a machine both can reach (put HTTPS in front for wss://)
rein-remote serve --relay wss://relay.example --relay-secret S   # on the machine to show: prints the viewer's link
```

The relay can't read what it passes: the machine makes a key and puts it in the viewer's link after
`#` (browsers never send that part of an address to a server), and every message is sealed end to
end with AES-256-GCM (keys per session and direction, from HKDF). A wrong key or an altered message
is refused. The viewer's page needs HTTPS (or localhost) for the browser's crypto.

Several relays (a fleet): give `--relay` once per relay (or `--relay A,B`). The machine opens its room
on every one and keeps it open; the link names the one that answered fastest, and the others after
`#` (`&alt=`), where a native viewer tries them in turn. Or put them behind one name (a load balancer
checking each relay's `/healthz`): any of them has the room. See
[docs/verification/relay-fleet.md](docs/verification/relay-fleet.md).

### Health and metrics

- `GET /healthz`: alive, as JSON (`status`, `version`, `uptime_s`, `sessions`, …). On `serve` it needs
  the token like `/stats`; on a relay it's on its port for anyone (no room or key in it).
- `GET /metrics`: Prometheus's text format (viewers, sessions, frames and bytes sent, each relay's
  health, round trip and registrations; on a relay: hosts waiting, sessions, bytes relayed,
  rendezvous answered, requests refused). On `serve`, with the token, best as a bearer
  (`Authorization: Bearer TOKEN`, Prometheus's `authorization: {credentials_file: …}`) so it's not in a
  URL. On a relay, on its own address: `--metrics-listen` (default `127.0.0.1:7991`, this machine only;
  a port alone is 127.0.0.1's; `off`).

### Config by file, and running as a service

```sh
rein-remote serve --config serve.toml            # each key a flag: listen = "0.0.0.0:7979", fps = 60, relay = [...]
rein-remote install-service --config serve.toml  # started with your session (systemd --user, a LaunchAgent, a task at logon)
rein-remote install-service relay --config relay.toml --system   # a relay with the machine (systemd, a LaunchDaemon, a Windows service)
rein-remote install-service … --print            # what it would install, nothing changed
rein-remote uninstall-service [relay] [--system]
```

Secrets stay out of the service's command line: `token = { file = "/etc/rein-remote/token" }`. The
command line wins over the file. See [docs/verification/service-install.md](docs/verification/service-install.md).

### Signed binaries

Releases are signed when the repository has the certificates (macOS: Developer ID and notarized;
Windows: Authenticode, timestamped), and `SHA256SUMS` is signed with cosign (Sigstore,
keyless). To check a download: [docs/verification/signing.md](docs/verification/signing.md).

## How it works

See [ARCHITECTURE.md](ARCHITECTURE.md): the protocol, the three transport tiers (QUIC/WebTransport,
relay, WebSocket over HTTPS), hardware and software codecs, capture and input per platform, pairing,
and how Rein embeds it.

## Building

```sh
cargo build --workspace
cargo test --workspace
```

## License

MIT. Rein Remote never links GPL code (no x264, no GPL ffmpeg): its software encoders are
BSD-licensed, and hardware encoding uses the platforms' own APIs.
