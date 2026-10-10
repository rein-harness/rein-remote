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
  device's keyboard typed and ran a command). The pointer goes apart from the video (XFixes): its
  image once per shape, its place when it moves, drawn by the client at the local mouse at once.
  Latency is measured from the client's per-frame feedback (capture time to decoded, less half the
  round trip; this reads high by up to a frame, as the server reads replies once a frame) and,
  glass to glass, with a timestamp overlay: the test pattern draws its capture time as a barcode
  and the client reads it from each decoded frame, 10 ms median / 12 ms p95 at 1280×720 on one
  Mac (645 of 645 frames read). Capture is in shared memory (MIT-SHM 1.2, the
  segment passed as a file descriptor; GetImage where there's none), and XDamage skips a still
  screen: at 1920×1080 scrolling full-screen, 1.7 ms to read the screen, 16 ms to encode,
  26 ms / 36 ms capture to decoded; a still terminal sent 8 frames in 8 seconds. A display with
  only its abstract socket (the file missing) connects too. Still to do in this phase: multi-slice
  threaded encoding (needs our own binding to openh264's C API; the crate's wrapper encodes one
  slice).

## Phase 2 — Rein previews through Rein Remote

- npm packages: `@rein-harness/remote` (the web client) and per-platform server binaries.
- Rein uses it for native-app and window previews (falls back to its current frames path).
- **Verified:** a Rein web UI chat shows a native Linux app through Rein Remote, from a phone-size
  browser, through a reverse proxy.

## Phase 3 — macOS and Windows servers

- macOS: ScreenCaptureKit (window, app, display), VideoToolbox H.264/HEVC, CGEvent input.
- Windows: Windows Graphics Capture, Media Foundation / NVENC / AMF / QSV, SendInput.
- Hardware detection with the software fallback.
- Zero-copy on the GPU, capture to encoder: DXGI / Windows Graphics Capture textures into NVENC,
  AMF or QSV; ScreenCaptureKit IOSurfaces into VideoToolbox; on Linux, PipeWire or KMS DMA-BUF
  into VA-API or NVENC. The CPU never copies a frame.
- High refresh: the client reports its display's real refresh rate (measured, not assumed) and the
  server paces to it, up to 240 Hz. Budget at 200 Hz: 5 ms a frame end to end.
- Near-zero cost: a still screen sends and encodes nothing (no periodic refresh while nothing
  changes); under motion a few percent of one core with a hardware encoder. Software encoding
  stays the fallback, at the frame rate the CPU can hold.
- **Status:** macOS windows stream and take input on a real Mac (Calculator: clicks computed
  7 + 2 = 9 with the app starting behind Terminal; keys typed into it in the background).
  `rein-remote windows` lists them. Linux windows too (XComposite, so a covered window streams as
  it is; a press raises and focuses it): verified with one xterm covered by another, its own
  contents streamed and a command typed into it ran. Windows windows (Graphics Capture + SendInput):
  CI's selftest on Notepad shows typing change it. Hardware encoding on macOS (VideoToolbox, the
  default there; RR_ENCODER=software for openh264): on an Intel Mac at 1080p with full motion,
  12.6 ms of CPU a frame against 19.8 ms in software; glass to glass 14 ms median at 720p (10 ms in
  software on the same Mac). Zero copy on macOS: ScreenCaptureKit's frames go into VideoToolbox
  where they are (GPU memory, no copy); streaming a terminal window changing ~15 times a second
  costs rein-remote 1.5% of one core (4.8% in software). The pointer's real shape on macOS too
  (the system's arrow, I-beam…), sent apart and hidden off the window. Next: Media
  Foundation/NVENC on Windows and VA-API on Linux.
- **Verified:** a window on each streams with input (macOS on a real Mac; Windows on CI runners and a
  real machine), hardware and software paths both; 200 fps measured glass to glass on a machine
  with a hardware encoder, with the server's CPU use written down (idle and under motion).

## Phase 4 — fast transport and adaptation

- QUIC (native) and WebTransport (browsers), datagrams for video.
  - **Status:** WebTransport is in: on the same port number over UDP, with a 14-day self-signed
    certificate the page is given by hash (`/info`), the same token as the WebSocket; each video frame
    on a stream of its own, control and input on one stream; the page uses it when the browser has
    it and falls back to the WebSocket when it doesn't open (verified: blocked UDP falls back by
    itself). Measured in a Linux container (Chrome there, netem on the loopback, a log-scrolling
    terminal): 2% loss, 30 ms RTT: p95 73 ms against 120 over the WebSocket (p50 alike, ~50 ms);
    5% loss: p95 110 against 126. On a clean link the same as the WebSocket (14 ms glass to glass at
    720p on a Mac). Rein's web UI keeps the WebSocket (through its own HTTPS proxy). Next: video as
    datagrams with forward error correction, so a lost packet costs no retransmission.
- LAN discovery (mDNS), pairing codes, mutual keys.
- Rendezvous with NAT hole punching; the relay (self-hostable).
  - **Status:** the relay is in: `rein-remote relay` (self-hosted, `--secret` for who may host) pairs
    a machine that connected out (`serve --relay ws(s)://…`, a random 24-character room) with a
    viewer who opens its page; the machine reconnects for the next viewer. Verified on one Mac:
    14 ms glass to glass through it, two viewers one after the other, a host with the wrong secret
    refused (403). End to end through it: the key in the link after # (the relay never sees it),
    AES-256-GCM a message, keys per session and direction (HKDF over a salt sent first), replays
    and changes refused; verified: the right key streams (14 ms glass to glass), a wrong key is
    refused at both ends, a link without its key says so. Still to do: NAT hole punching
    (straight over QUIC when it works).
- Delay-based bitrate controller; frame rate and resolution adaptation; intra refresh.
  - **Status:** a delay-based controller (each frame's round trip on the server's own clock, its
    rise over the lowest of the last 10 s is queuing; a backed-up socket counts too) cuts the
    bitrate and frame rate at once and climbs back slowly; both encoders change bitrate live (our own
    binding to openh264, which also encodes in 4 slices on 4 cores: 8.6 ms at 1080p, was 12.9).
    The socket's send buffer holds ~50 ms, so congestion shows within a frame; input and feedback are
    handled the moment they arrive, not once a frame; no keyframe after a backlog. Measured with tc
    (netem 40 ms + tbf) in a Linux container, a log-scrolling terminal at 1280×720: 22 ms p50 on an
    open link (28 before), ~48 ms at 1.5 Mbit/s and 800 kbit/s with no false alarms; full-screen
    random text over 1.5 Mbit/s (more than the link can carry): 229 ms p50 adapting against 505 ms
    not. Still to tune: the target oscillates above capacity on such content, and the first keyframe
    takes a second on a 300 kbit/s link (resolution adaptation and a smaller first keyframe are next).
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
