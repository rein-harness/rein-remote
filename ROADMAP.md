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
  only its abstract socket (the file missing) connects too. Multi-slice threaded encoding is in
  (our own binding to openh264's C API): a frame in 4 slices on 4 cores, 8.6 ms at 1080p where one
  slice took 12.9 ms (measured with phase 4's controller work, below).

## Phase 2 — Rein previews through Rein Remote

- npm packages: `@rein-harness/remote` (the web client) and per-platform server binaries.
- Rein uses it for native-app and window previews (falls back to its current frames path).
- **Verified:** a Rein web UI chat shows a native Linux app through Rein Remote, from a phone-size
  browser, through a reverse proxy.
- **Status:** verified. The packages: `@rein-harness/remote` (the web client, with its types) and
  `@rein-harness/remote-server` (`npx rein-remote …`, `binaryPath()` for programs), which depends on
  one package per platform binary (optional dependencies: npm installs only the machine's own),
  built by `npm/build.mjs`, packed, installed and checked against Rein's contract (`npm/contract.mjs`:
  the port line on stderr, the token, `/rein-remote.js`, Hello/Config/keyframe, `--pipe`'s JPEGs in
  and JSON input out, `windows --json`) in CI on Linux, macOS and Windows; `release.yml` publishes
  them on a version tag. Rein's previews, end to end in Docker (`tests/e2e/rein-preview`): Rein's web
  UI behind Caddy over HTTPS, a 390×844 phone-size Chromium (DPR 3, touch) that can reach only the
  proxy; a chat's preview of an xterm on a VNC display streamed through Rein Remote on its first
  open, a tap and the phone's keyboard ran a command in it, `/preview :2` streamed the same way, a
  password-protected display fell back to Rein's frames, and the page never received rein-remote's
  token, port or a VNC password. (This found three bugs in Rein, fixed in rein-harness#678.) In that
  VM (4 shared cores, software decoding at DPR 3) the phone showed 2–26 fps: CPU-bound there, a lower
  bound, not the stream's.

## Phase 3 — macOS and Windows servers

- macOS: ScreenCaptureKit (window, app, display), VideoToolbox H.264/HEVC, CGEvent input.
- Windows: Windows Graphics Capture, Media Foundation / NVENC / AMF / QSV, SendInput.
- Hardware detection with the software fallback.
- VNC as a source (any OS): a VM's or an emulator's screen as video. **Status:** in (RFB 3.3–3.8,
  none or VNC's password, raw and copy-rect, BGRX so rows go in as they come); verified against
  Xvnc with a password in a Linux container: an xterm streamed and a typed command ran.
- Frames from another program (`--pipe WxH`: length-prefixed JPEGs on stdin, the viewer's input as
  JSON lines on stdout), for Rein's headless browser: web-app previews as video. **Status:** in;
  verified with JPEGs fed at 20 fps (47 frames in 3 s) and a click coming back as events.
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
- **Status (phase 3 finished):** macOS streams a whole display (`serve` alone, or `--display N`;
  `rein-remote displays`) or an app's windows (`--app NAME`), at the display's real pixel scale: a
  3274×2048 display goes from ScreenCaptureKit into VideoToolbox with no copy, 3.3% of one core
  while the screen changes, 22 ms to encode. HEVC (`--codec h265`): Chrome decodes it in hardware,
  60 fps, 21 ms glass to glass. Hardware detection: `encode::open` tries the hardware encoders the
  machine lists (VTCopyVideoEncoderList, Media Foundation's hardware MFTs, VA-API's entry points)
  and falls back to openh264; the encoder is warmed up at start (the first VideoToolbox session
  takes 1.2–2 s). VideoToolbox at 1080p: 12.2 ms a frame, 2.1 ms of it CPU. High refresh: the
  client measures its display's refresh and tells the server (Hello v1), which paces to it (`--fps
  auto`, up to 240; 60 in software): a 59.9 Hz display got 60 fps. 200 fps from the test pattern on
  this Mac's VideoToolbox (A18 Pro, headless Chrome, which shows frames faster than its 60 Hz
  screen): 198 fps, 15 ms p50 glass to glass, under light load; 148 fps, 16 / 24 ms p50 / p95 with
  the server at 9% of one core, measured again at load average 63 (other builds running). A still
  screen sends and encodes nothing: a still `--pipe` source sent 0–1 frames in 10–30 s (the old
  build sent 10 every 10 s) and used 0.3% of one core. Windows (Graphics Capture + SendInput +
  Media Foundation, D3D11 textures straight into the encoder) and Linux VA-API (`serve --kms`,
  DMA-BUF into the encoder, its H.264 headers checked by ffmpeg in CI) are in; Windows works on CI
  runners with Microsoft's encoder. Needs hardware, with exact manual tests:
  [GPU encoders and zero copy on Windows](docs/verification/windows-hardware-encoders.md),
  [VA-API and KMS on Linux](docs/verification/linux-vaapi-kms.md),
  [a real 200 Hz display](docs/verification/high-refresh.md),
  [macOS display and app input](docs/verification/macos-display-app-input.md) (needs the
  Accessibility permission). Not done: PipeWire capture (KMS was chosen), NVENC on Linux (VA-API
  was chosen), and change detection under KMS (a still screen is encoded again there).
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
  - **Status:** no setup on a tailnet or a local network: `serve` listens on every interface behind a
    random token and prints a link per way in (tailnet name, tailnet address, local network,
    localhost). With Tailscale's HTTPS on, a real certificate for the machine's name (`tailscale
    cert`) and HTTPS on the same port as HTTP (the first byte says which), WebTransport with it too;
    on plain HTTP (no WebCodecs there) the page plays the H.264 through Media Source. Verified on a Mac
    on a tailnet: the https:// tailnet link 15 ms glass to glass over WebTransport; the http://
    tailnet and LAN links ~31 fps at 46–53 ms through Media Source; no token, 403. mDNS discovery is
    next.
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
