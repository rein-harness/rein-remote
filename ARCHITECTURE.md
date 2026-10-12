# Rein Remote architecture

Rein Remote is a remote-desktop and app-streaming system: one fast, modern protocol to see and use
a screen, a window or a single app on another machine, from any device. It runs on its own, and it's
what [Rein](https://github.com/rein-harness/rein-harness) uses to show you what its agent builds.

This document is the design. [ROADMAP.md](ROADMAP.md) is the order it's built in; the README's
status table says what works today.

## Goals

- **Every device.** Servers on Windows, macOS and Linux. Clients on Windows, macOS, Linux, iOS,
  iPadOS, Android, and the web (a JavaScript/WebAssembly client any website can embed).
- **Game-grade.** Low enough latency that a game feels local on a good link, and adapts on a bad one:
  the quality and frame rate follow the connection, the picture never falls behind.
- **Works out of the box.** Start the server, open the client, pair once. No ports to open, no
  codecs to pick, no settings to understand.
- **Scales.** The same pieces run one machine for one person or a fleet behind your own relays,
  with your own identity provider and policies.
- **Always works.** Hardware encoding and decoding where the machine has it, a software path where it
  doesn't (no GPU, no drivers, a VM, a container).

## Components

```text
           server (Windows / macOS / Linux)                       client (any)
  ┌──────────────────────────────────────────────┐        ┌───────────────────────────┐
  │ capture ─► encode ─► packetize ─┐            │        │      ┌─► decode ─► present │
  │ (screen,   (HW: NVENC/QSV/AMF/  │  transport │◄──────►│ ─────┤                    │
  │  window,    VideoToolbox/MF/    │  (QUIC,    │        │      └─► audio            │
  │  display)   VA-API; SW: open-   │  WebTrans- │        │  input ◄─ keyboard, mouse, │
  │             h264/libvpx/SVT-AV1)│  port, WS, │        │           touch, gamepad   │
  │ audio ─► Opus ──────────────────┤  relay)    │        └───────────────────────────┘
  │ input ◄─ inject (SendInput,     │            │
  │          CGEvent, uinput/XTest) ◄────────────┤
  └──────────────────────────────────────────────┘
```

| Crate / folder | What it is |
|---|---|
| `crates/rr-proto` | The protocol: message types, binary framing, versioning. No I/O, no platform code; compiles to WebAssembly, so the web client shares it. |
| `crates/rr-client` | The native client core (no UI, no decoding): connects over QUIC/WebTransport, the WebSocket or a relay (then direct through NAT), pairs and signs in, finds servers by mDNS, and yields the stream as events. The desktop and mobile apps build on it; `rein-remote connect` uses it to measure. |
| `crates/rr-server` | The server, binary `rein-remote`: capture, encode, transport, input injection, pairing, the relay. One binary per platform. |
| `clients/web` | The web client: WebCodecs decode (hardware), WebAudio, input, the protocol from `rr-proto` as WebAssembly. Embeddable with one `<script>` and a `<rein-remote>` element. |
| `clients/native` | Desktop clients (Windows, macOS, Linux), sharing a Rust core with platform decode and presentation. Later: iOS/iPadOS (Swift, VideoToolbox) and Android (Kotlin, MediaCodec) on the same core. |
| `relay/` | The relay and rendezvous server, for connections that can't go direct. Self-hostable, one binary. |

## Protocol (v0)

Binary, little-endian, length-prefixed frames. Every frame is `kind: u8, version: u8, flags: u8, len: u32, body`: the message, its version, flags (a video frame's keyframe bit, readable without parsing the body), and the body's length.
The same frames go over every transport. Streams are independent: video can drop frames without
holding up input or audio.

| Kind | Direction | Body |
|---|---|---|
| `Hello` | both | protocol version, capabilities (codecs it can encode or decode, hardware or not, max size and rate, audio, input kinds) |
| `Config` | server → client | the chosen codec, size, frame rate, color space; sent again when they change |
| `Video` | server → client | one encoded frame: keyframe flag, timestamp (µs, server clock), frame number, data |
| `Audio` | server → client | one Opus packet, timestamp |
| `Cursor` | server → client | position, and the shape when it changes (sent apart from the video, so moving the pointer costs nothing) |
| `Input` | client → server | pointer (absolute or relative), button, wheel, key (scan code and key), text, touch points, gamepad state |
| `Feedback` | client → server | what arrived and when (frame number, receive and decode times, loss): drives the bitrate and the frame rate |
| `KeyframeRequest` | client → server | the client lost a reference frame (or just joined) |
| `Clipboard` | both | text (later images and files) |
| `Resize` | client → server | the size the client wants (the server resizes a virtual display, or scales) |
| `Ping`/`Pong` | both | round-trip time |

### Compatibility

Peers of different versions keep working together, whichever side was updated first. The rules:

- **A new message** gets a new kind. A reader skips kinds it doesn't know, by their length.
- **A new field** goes at the end of its message, and the message's version goes up. A newer reader
  fills in a default when an older peer's frame stops before the field; an older reader reads what
  it knows and ignores the rest. So bodies only grow at the end, and byte blobs (video, audio, the
  cursor's image) are length-prefixed, so fields can follow them.
- **Values a reader doesn't know** (a newer codec in `Hello`, a newer input kind, a newer button)
  are skipped, never an error.
- **Anything else** (a field that changes meaning or type, one removed): a new kind, next to the old
  one, which is kept as long as peers may need it. `Hello` carries the range of protocol versions
  each side speaks, and both use the highest they share.

The crate's tests hold this: a future version of each message, with extra fields, read by this
build; an older message, missing a field added since, read with its default.

## Transport

Three tiers, picked automatically, fastest first:

1. **QUIC (UDP)** between native apps, **WebTransport** from browsers. Direct when a path exists: the
   same network (found with mDNS), or through NAT with hole punching through the rendezvous server.
   Video as unreliable datagrams (a late frame is a dropped frame, never a delay); input, control and
   clipboard on reliable streams.
2. **Relay.** When no direct path exists (symmetric NAT, strict firewalls), traffic goes through a
   relay (a WebSocket there today), sealed end to end with a key the relay never sees. Self-hostable;
   a deployment can run several.

### Through NAT: from the relay to a direct path

A session always starts on the relay, and moves off it when it can:

1. The relay also listens on UDP, on its own port. For a room it has open, it answers "what address
   do you see me from?" (rr-proto's `punch`: a probe that names the room by a hash, answered with the
   address it came from and never with more bytes than the probe).
2. `serve --relay` binds its WebTransport socket itself and asks the relay from that socket before
   QUIC takes it. That gives it its address beyond its NAT. From then on, a clone of the socket sends
   a keepalive to the relay every 15 s so the NAT keeps the mapping. The keepalive is never answered:
   an answer would land on a socket QUIC owns.
3. A native viewer says `FEATURE_DIRECT` in its Hello. Inside the sealed session, the host sends a
   `DirectOffer`: its candidates (the address the relay saw, then its own networks' addresses), its
   WebTransport certificate's SHA-256, and a ticket its gate accepts once. The relay can't read or
   change any of it.
4. The viewer learns its own outside address the same way, from the UDP socket it will use for QUIC,
   and answers with a `DirectAnswer` holding its candidates.
5. Both send punch packets to each other's candidates for a few seconds. The host sends from a clone
   of its QUIC socket. Each NAT then takes the other side's packets as answers to its own. Punch and
   keepalive packets are too small for QUIC to answer when they land on a QUIC socket.
6. The viewer opens WebTransport straight to the host's candidates, all at once, from the same
   socket. It accepts only the certificate with the offered hash, and signs in with the ticket. On
   success it says Hello there, and the relayed session closes. On failure it stays on the relay.

This works through the NATs most homes and phones have, which keep one outside port per socket
whoever it talks to. Through a NAT that gives each destination its own port (symmetric NATs, some
carrier-grade NATs), the address the relay saw isn't one the other side can reach, so the session
stays relayed. The same happens when UDP is blocked between the two, or when the relay's UDP port
isn't reachable (an HTTPS proxy in front of it passes only TCP). **Browsers can't punch**, because a
page has no UDP socket of its own, so they stay on the relay. `tests/nat/run.sh` checks all of this
between two NAT routers in Docker; docs/verification/nat-hole-punching.md is for real NATs.
3. **WebSocket over HTTPS.** Through anything that passes web traffic: a corporate proxy, a reverse
   proxy in front of a server (Caddy, nginx, Cloudflare), Tailscale Serve. Reliable and ordered, so
   the server drops video frames itself when the connection backs up (keyframe-and-skip). This is
   also how Rein's web UI carries a preview: inside the connection it already has.

## Video: encoding, and adapting to the link

- **Codec negotiation** in `Hello`: H.264 everywhere (every device decodes it in hardware), HEVC and
  AV1 where both sides can, VP9 as a software-friendly alternative.
- **Hardware encoders:** NVENC (NVIDIA), AMF (AMD), Quick Sync / oneVPL (Intel), Media Foundation
  (Windows, any vendor), VideoToolbox (macOS), VA-API (Linux). **Software fallback** when none works
  (no GPU, missing drivers, a VM or container): openh264 (H.264), libvpx (VP8/VP9), SVT-AV1 (AV1).
- **Low-latency settings:** no B-frames, one-frame lookahead at most, intra refresh instead of big
  periodic keyframes (no bitrate spikes), slice output so the client can start decoding before the
  frame is done.
- **Adaptation:** the client's `Feedback` (arrival times, queueing delay, loss) drives a delay-based
  controller (like WebRTC's), which sets the bitrate, then the frame rate, then the resolution, in
  that order of preference, per session. A static desktop gets nearly free; a game gets everything
  the link has. Content-aware: text-heavy screens prefer sharpness, motion prefers frame rate.
- **Cursor apart:** the client draws the cursor itself from `Cursor` messages, at the client's own
  frame rate.

Licensing: Rein Remote is MIT. GPL code (x264, ffmpeg's GPL parts) is never linked. The software
encoders above are BSD-licensed; hardware encoders are the operating systems' and vendors' own APIs.

Patents: H.264 and HEVC are covered by patent pools. Cisco pays the H.264 royalties for its own
prebuilt openh264 binaries, not for openh264 built from source, so a distributor should either load
Cisco's binary at runtime (the openh264 crate's `libloading` mode) or rely on the platform's
encoder; VP9 and AV1 are royalty-free and are the safe software default to ship.

## Capture and input, per platform

| | Capture | Input |
|---|---|---|
| Windows | Windows Graphics Capture (a window or a screen; DXGI Desktop Duplication as a fallback) | `SendInput`; ViGEm-style virtual gamepads later |
| macOS | ScreenCaptureKit (a window, an app, or a display) | `CGEvent` (needs the Accessibility permission) |
| Linux | PipeWire screen cast (Wayland), XShm/XDamage (X11), or a virtual display Rein Remote starts itself (headless servers) | XTest (X11), `uinput` (Wayland, needs permission) |

macOS asks the user once to allow Screen Recording (and Accessibility for input) for the server
binary; a signed binary keeps the permission across updates (unsigned builds ask again after each).

## Pairing and security

- **Pairing:** the server shows a short code (and a QR code); the client enters or scans it once. Both
  sides then keep each other's keys: later connections are mutual-key authenticated with nothing to
  type. Rein pairs with its own helper automatically (same machine, a one-time token).
- **Encryption:** QUIC/WebTransport and HTTPS are TLS 1.3; the relay forwards encrypted traffic it
  can't read.
- **Least privilege:** a client can be allowed to view only, or view and control; per window or per
  display. Input injection is refused unless allowed.
- **Enterprise (later):** OIDC/SAML sign-in, policies (who can reach which machines, clipboard and
  file rules), audit logs, recording, and a fleet of relays.

## How Rein uses it

Rein starts the `rein-remote` server binary on demand (shipped through npm as per-platform optional
packages, the way esbuild ships its binaries) to stream a preview: a native window, an app on a
virtual display, or a browser tab. The video rides the web UI's own connection (tier 3), so a preview
works through Tailscale, a domain or a proxy with nothing to configure, and the web UI shows it with
the `rr-web` client. Rein's current preview path (screenshot frames) stays as the fallback for
machines where the server can't run.

## Standalone

`rein-remote serve` on the machine to reach, a client anywhere; or embed the web client:

```html
<script type="module" src="https://…/rein-remote.js"></script>
<rein-remote src="wss://my-server.example/remote" token="…"></rein-remote>
```
