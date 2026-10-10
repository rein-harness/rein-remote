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
| Client: Windows / macOS / Linux | todo (phase 5) |
| Client: iOS / iPadOS / Android | todo (phase 5) |
| Transport: WebSocket | working, with keyframe-and-skip when the link backs up (phase 1) |
| Transport: WebTransport (QUIC) | working: a stream per frame, self-signed certificate by hash, falls back to the WebSocket by itself (phase 4) |
| Transport: relay (behind NAT, no open ports) | working: `rein-remote relay` (self-hosted), `serve --relay`; NAT hole punching todo (phase 4) |
| Adaptive bitrate, audio | todo (phase 4) |
| Pairing, enterprise sign-in and policy | todo (phases 4, 6) |

## Try it

```sh
cargo build --release -p rr-server
./target/release/rein-remote serve --display :0        # a Linux X display (or a virtual one: Xvfb :20)
./target/release/rein-remote serve --test-pattern      # any platform: a moving test pattern
# then open http://127.0.0.1:7979 in a browser
```

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

### From anywhere, through a relay

```sh
rein-remote relay --listen 0.0.0.0:7990 --secret S        # on a machine both can reach (put HTTPS in front for wss://)
rein-remote serve --relay wss://relay.example --relay-secret S   # on the machine to show: prints the viewer's link
```

The relay can't read what it passes: the machine makes a key and puts it in the viewer's link after
`#` (browsers never send that part of an address to a server), and every message is sealed end to
end with AES-256-GCM (keys per session and direction, from HKDF). A wrong key or an altered message
is refused. The viewer's page needs HTTPS (or localhost) for the browser's crypto.

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
