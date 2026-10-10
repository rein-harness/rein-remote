# Roadmap

Built one proven slice at a time. A phase is done when its **verified** line is true on real
machines (or CI runners) — not when the code compiles. The README's status table is kept to what
has been verified.

## Phase 0 — foundations

- Cargo workspace, MIT license, this roadmap and [ARCHITECTURE.md](ARCHITECTURE.md).
- `rr-proto`: frame format and v0 message types, encode/decode, version negotiation.
- CI: build and test on Linux, macOS and Windows; `rr-proto` also for `wasm32`.
- **Verified:** CI green on all three, `rr-proto` round-trips every message.

## Phase 1 — the first stream (Linux server → web client)

- Linux capture of an X display (a real one or a virtual display the server starts).
- Software H.264 (openh264), low-latency settings, keyframe on request.
- WebSocket transport with keyframe-and-skip when the connection backs up.
- Web client: WebCodecs decode, canvas present, pointer/keyboard/touch input, cursor apart.
- **Verified:** in a Linux container, an app on a virtual display streams to headless Chrome; input
  reaches the app; glass-to-glass latency measured with a timestamp overlay and written down.
- **Status:** the stream works and is measured: an xterm on Xvfb streamed to Chrome, a click
  focused it and typed text (capitals and symbols included) ran. With the terminal scrolling
  full-screen at 1280×720 on a 4-core VM: 33 ms median / 44 ms p95 capture to decoded, 3 ms to
  read the screen, 19 ms to encode (BGRA→YUV went from 8.8 ms to 0.7 ms with our own conversion).
  Touch works on a phone-size screen (a swipe scrolled the terminal's history, a tap and the
  device's keyboard typed and ran a command). Still to do in this phase: the cursor sent apart, MIT-SHM +
  XDamage capture, and multi-slice threaded encoding (needs our own binding to openh264's C API;
  the crate's wrapper encodes one slice).

## Phase 2 — Rein previews through Rein Remote

- npm packages: `@rein-harness/remote` (the web client) and per-platform server binaries.
- Rein uses it for native-app and window previews (falls back to its current frames path).
- **Verified:** a Rein web UI chat shows a native Linux app through Rein Remote, from a phone-size
  browser, through a reverse proxy.

## Phase 3 — macOS and Windows servers

- macOS: ScreenCaptureKit (window, app, display), VideoToolbox H.264/HEVC, CGEvent input.
- Windows: Windows Graphics Capture, Media Foundation / NVENC / AMF / QSV, SendInput.
- Hardware detection with the software fallback.
- **Verified:** a window on each streams with input (macOS on a real Mac; Windows on CI runners and a
  real machine), hardware and software paths both.

## Phase 4 — fast transport and adaptation

- QUIC (native) and WebTransport (browsers), datagrams for video.
- LAN discovery (mDNS), pairing codes, mutual keys.
- Rendezvous with NAT hole punching; the relay (self-hostable).
- Delay-based bitrate controller; frame rate and resolution adaptation; intra refresh.
- Opus audio, synchronized with video.
- **Verified:** latency and quality measured on LAN, on a throttled link (tc netem: loss, delay,
  low bandwidth) and through the relay; a fast-moving game scene stays smooth on the bad link.

## Phase 5 — native clients

- Desktop: Windows, macOS, Linux (Rust core, platform hardware decode, gamepads).
- Mobile: iOS and iPadOS (Swift, VideoToolbox), Android (Kotlin, MediaCodec); touch, keyboard,
  gamepads, pointer on iPad.
- **Verified:** each connects, streams and controls on real devices.

## Phase 6 — larger deployments

- Sign-in with an identity provider (OIDC, SAML), policies, audit logs, session recording.
- A relay fleet, health and metrics, headless install and config by file.
- Signed and notarized binaries on every platform.
- **Verified:** a multi-user deployment behind an identity provider, with policies enforced.
