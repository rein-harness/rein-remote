# Verifying VA-API and KMS zero copy on Linux

CI's Linux runners have no GPU, so CI tests only what can be tested without one: the structs the
encoder hands libva (sizes and offsets against libva's headers), the H.264 headers it writes (a
stream built from them decodes in ffmpeg to exactly the pixels put in), and the fallback to software
when there's no libva or no GPU. This script checks the rest on a real machine: VA-API encoding on
an Intel or AMD GPU, the GPU's colour conversion, the screen read through KMS and imported as a
DMA-BUF (zero copy), the speed and CPU cost, and input.

Run it once per GPU you have. A machine with two GPUs: see [Two GPUs](#two-gpus).

## 0. Setup

Ubuntu 24.04 or Debian 12 (other distributions: the same packages under their names, see the
notes). Intel: Broadwell (5th generation) or later; AMD: GCN or later.

```sh
# The VA-API driver for your GPU, libva, and tools to check it.
sudo apt install vainfo libva2 libva-drm2 ffmpeg intel-gpu-tools   # all GPUs (intel-gpu-tools: Intel only)
sudo apt install intel-media-va-driver-non-free   # Intel, Broadwell and later (iHD; Ubuntu: multiverse)
sudo apt install i965-va-driver-shaders           # Intel, older than Broadwell (i965)
sudo apt install mesa-va-drivers                  # AMD (radeonsi)
# Rust, Node 22+ and Chrome, then:
git clone https://github.com/rein-harness/rein-remote && cd rein-remote
git checkout phase-3-linux
cargo build --release -p rr-server
rr=$PWD/target/release/rein-remote
groups | grep -q render || echo "add yourself to the render group: sudo usermod -aG render $USER (then log in again)"
```

Notes: Fedora's `mesa-va-drivers` has H.264 encoding turned off (patents): install
`mesa-va-drivers-freeworld` from RPM Fusion. Ubuntu's `intel-media-va-driver` (without
`-non-free`) encodes only on some generations; the non-free one encodes on all.

Write down the machine: distribution and kernel (`uname -r`), GPU (`lspci -nn | grep -i vga`),
the session (`echo $XDG_SESSION_TYPE`: x11 or wayland), and `vainfo`'s first lines (driver and
version).

## 1. The GPU encodes H.264 and converts colour

```sh
vainfo 2>&1 | grep -E "Driver version|H264(Main|ConstrainedBaseline).*Enc|VideoProc"
```

Expect at least one of

```text
      VAProfileH264Main               : VAEntrypointEncSliceLP
      VAProfileH264Main               : VAEntrypointEncSlice
```

and `VAProfileNone : VAEntrypointVideoProc` (the GPU's colour conversion). Rein Remote uses
`EncSliceLP` (low power) when it's there, else `EncSlice`; Main, else Constrained Baseline. No
`Enc` line: this driver doesn't encode (see the notes above), and the server will encode in
software. Copy these lines into the report.

## 2. The encoder's tests, on the GPU

```sh
cargo test --release -p rr-server vaapi -- --nocapture --test-threads=1
```

Expect `test result: ok`, and from `opens_or_says_why` (with a GPU it encodes three frames):

```text
encoder: H.264 in hardware (VA-API: Intel iHD driver for Intel(R) Gen Graphics - 24.1.0 (), Main, low power, CBR)
keyframe 1234 bytes, then 56 bytes, forced keyframe 1234 bytes
```

(AMD: `VA-API: Mesa Gallium driver 24.0.5 for AMD Radeon …`.) No `encoder:` line means it couldn't
open one: run step 3, which prints why.

## 3. The encoder on its own, its stream checked

The test pattern (from memory: the GPU converts it), 300 frames as fast as they go, saved:

```sh
RR_ENCODER=vaapi $rr selftest --test-pattern 1920x1080 --encode /tmp/va.h264 --frames 300
RR_ENCODER=software $rr selftest --test-pattern 1920x1080 --encode /tmp/sw.h264 --frames 300
```

Expect, for VA-API:

```text
rein-remote selftest: 1920×1080, 300 frames by H.264 in hardware (VA-API: …, Main, low power, CBR): 2.10 ms a frame (median 2.00, slowest 6.00), 1.20 ms of CPU a frame, 1 keyframe(s), 456789 bytes in /tmp/va.h264
```

Copy both lines into the report (ms a frame, CPU ms a frame, for VA-API and software). With
`RR_ENCODER=vaapi` it fails (exit code 1) with the reason rather than falling back, for example
`VA-API: /dev/dri/renderD128: Permission denied (os error 13) (add this user to the render group)`.

If the log says `rein-remote: VA-API: no colour conversion on the GPU (…): converting on the CPU`,
note it: it works, but the CPU converts each frame (and `--kms` won't work).

Check the stream, as a decoder sees it:

```sh
ffprobe -v error -show_entries stream=profile,level,width,height,has_b_frames,color_range,color_space,color_primaries,color_transfer -of compact /tmp/va.h264
ffmpeg -v error -i /tmp/va.h264 -f null - && echo "decodes cleanly"
ffprobe -v error -show_entries frame=key_frame,pict_type -of csv /tmp/va.h264 | sort | uniq -c
ffplay /tmp/va.h264
```

Expect `profile=Main` (or `Constrained Baseline`), `width=1920|height=1080`, `has_b_frames=0`,
`color_range=tv|color_space=smpte170m|color_primaries=bt709|color_transfer=iec61966-2-1` (our SPS;
`unknown` for all four means the driver wrote its own SPS: note it), `decodes cleanly`, one
`frame,1,I` and 299 `frame,0,P`. In ffplay: the gradient (blue-green to purple) with a white block
moving left to right and the barcode along the bottom, colours like the software stream's
(`ffplay /tmp/sw.h264`), no green or pink cast, no blocks or smearing.

## 4. A real desktop over X, with input

In an X11 session (on Wayland, `$DISPLAY` is Xwayland: it only shows X apps; use `--kms`, step 6):

```sh
RR_ENCODER=vaapi $rr serve --display $DISPLAY --listen 127.0.0.1:7979
```

Expected log:

```text
rein-remote: X display :0: capture in shared memory, changes from XDamage
```

and when the browser connects (open `http://127.0.0.1:7979/`):

```text
rein-remote: encoding H.264 in hardware (VA-API: …, Main, low power, CBR)
rein-remote: Chrome … connected (H264 1920×1080 at 60 fps, its display 60.0 Hz)
```

Check: the desktop shows, with the right colours; moving a window is smooth; click into a terminal
and type: the text appears; the pointer follows. Every 5 s the server prints its stats
(`"encode_ms":{"p50":…}`): note `encode_ms` and `grab_ms`. Then without `RR_ENCODER` (auto): the
same `encoding … VA-API` line (hardware is the default).

## 5. Speed: encode time, CPU, glass to glass

The test pattern carries its capture time as a barcode the browser client reads back, which measures
glass to glass (capture to the decoded frame on screen). Terminal 1:

```sh
RR_ENCODER=vaapi $rr serve --test-pattern 1280x720 --fps 200 --listen 127.0.0.1:7979
```

Terminal 2:

```sh
node tools/measure.mjs 'http://127.0.0.1:7979/?measure' --seconds 10
```

It prints one JSON object. Write down `fps` (expect close to 200), `glassMs.p50` / `glassMs.p95`,
`server.encode_ms.p50` / `p95` (here: upload, GPU conversion, encode and readback), and
`server.grab_ms`. Terminal 1 prints `rein-remote: encoding H.264 in hardware (VA-API: …)` when the
page connects: check it.

CPU, in terminal 3 while it runs: the server's share of one core over 10 s.

```sh
pid=$(pgrep -f 'rein-remote serve' | head -1); hz=$(getconf CLK_TCK)
t0=$(awk '{print $14+$15}' /proc/$pid/stat); sleep 10; t1=$(awk '{print $14+$15}' /proc/$pid/stat)
echo "$(( (t1 - t0) * 100 / hz / 10 ))% of one core"
```

The GPU, while it runs: Intel `sudo intel_gpu_top` (the `Video` engine busy, `VideoEnhance` for the
conversion; `Render/3D` near idle); AMD `amdgpu_top` or `radeontop` (`VCN`/`UVD` busy).

Then the same with `RR_ENCODER=software` (it holds 60 fps: `--fps 60`), for comparison.

## 6. Zero copy: the screen through KMS

The screen as the GPU scans it out: the framebuffer exported as a DMA-BUF, imported by VA-API, the
colour converted and the frame encoded on the GPU. The CPU never touches the pixels. It needs
CAP_SYS_ADMIN (root), and works on the console, under X and under a Wayland compositor alike.

Without root first, to see the message:

```sh
$rr selftest --kms --encode /tmp/kms.h264
```

expects `rein-remote selftest: reading the screen through KMS needs CAP_SYS_ADMIN: run it as root
(sudo -E rein-remote serve --kms), or give the binary the capability (sudo setcap cap_sys_admin+ep
rein-remote)`. (If instead it says `--kms: no lit screen (/dev/dri/card0: Permission denied …)`,
the card itself is closed to you: add yourself to the `video` group, or just use sudo.)

Then as root (`-E` keeps `$DISPLAY` and `$XAUTHORITY`, for input through X):

```sh
sudo -E $rr selftest --kms --encode /tmp/kms.h264 --frames 300
```

Expected:

```text
rein-remote: KMS eDP-1 on /dev/dri/card1: 1920×1080, frames to the GPU's encoder as they are (DMA-BUF)
rein-remote: X display $DISPLAY: input with XTest, the pointer from XFixes
rein-remote: VA-API: frames straight from the screen's buffer (XR24, modifier 0x100000000000002), no copy
rein-remote: --kms encodes with H.264 in hardware (VA-API: …)
rein-remote: VA-API: frames straight from the screen's buffer (XR24, modifier 0x100000000000002), no copy
rein-remote selftest: 1920×1080, 300 frames by H.264 in hardware (VA-API: …), 300 straight from the GPU's memory: 1.50 ms a frame (…), 0.10 ms of CPU a frame, 1 keyframe(s), … bytes in /tmp/kms.h264
```

(The `frames straight from the screen's buffer` line shows twice: once from the check `--kms` makes
as it starts, a frame imported and encoded so a screen the GPU can't take fails before anyone
connects, and once from the encoder that does the work.)

**The zero-copy path is active** when you see both the `frames straight from the screen's buffer …
no copy` line and `300 straight from the GPU's memory` in the result, and the CPU per frame is a
small fraction of step 3's (nothing is read, converted or uploaded by the CPU). Copy all of those
lines, the modifier in particular (`0x0` is linear; Intel's tiled and compressed layouts are
`0x1000000000000xx`, AMD's are `0x200000…`). With no connector given it takes the first lit one;
pick one with `--kms HDMI-A-1` (a wrong name lists the lit ones).

Check the picture: `ffplay /tmp/kms.h264` shows your screen as it was (no stripes or scrambled
tiles: those mean the layout, the modifier, was misread: report it with the modifier).

Then stream it:

```sh
sudo -E $rr serve --kms --listen 127.0.0.1:7979
```

Open `http://127.0.0.1:7979/` (from this machine, or `--listen 0.0.0.0:7979` and the printed link
from another). The same log lines as above when it starts, then `encoding H.264 in hardware
(VA-API: …)` when the page connects. Check the picture and colours, that moving a window is smooth,
and (under X) that clicking and typing work and the pointer shows. Under Wayland or on the console,
the screen streams but takes no input, and the pointer (a separate cursor plane) isn't in the
picture: expected, note it.

Speed: with the page open, the CPU one-liner from step 5 (expect far less than step 4's X capture of
the same screen: a few percent of a core at 60 fps), and the server's stats every 5 s: `grab_ms`
near 0 (reading the framebuffer's handle, not its pixels) and `encode_ms`.

Glass to glass through KMS (optional): run a test-pattern server on another port (`$rr serve
--test-pattern 1920x1080 --listen 127.0.0.1:7980`), open `http://127.0.0.1:7980/` full screen in
Chrome on this machine (the pattern's size equal to the screen's, zoom 100%), and measure the KMS
server: `node tools/measure.mjs 'http://127.0.0.1:7979/?measure'`. The barcode's time is when the
first server drew the frame, so `glassMs` covers the first stream, the display, KMS capture,
encoding and decoding: an upper bound for the KMS path.

## 7. What to report

| | VA-API | software |
|---|---|---|
| `vainfo` driver, entrypoints (step 1) | | — |
| encoder name (step 2/3) | | — |
| ms a frame / CPU ms a frame, 1080p (step 3) | | |
| stream: profile, colour fields, decodes cleanly, I then P (step 3) | | — |
| X desktop: picture, colours, input (step 4) | | — |
| test pattern: fps, glassMs p50/p95, encode_ms p50/p95, CPU % (step 5) | | |
| KMS: connector, format, modifier, CPU ms a frame, picture right (step 6) | | — |
| KMS serve: CPU %, grab_ms, encode_ms, input (step 6) | | — |

And every error line, verbatim.

## Two GPUs

Each is tried in turn: render nodes in order (`/dev/dri/renderD128`, `renderD129`, …), the first
that encodes H.264 wins. To pick one: `RR_VAAPI_DEVICE=/dev/dri/renderD129`. With `--kms`, the
encoder is opened on the same GPU as the screen's card (a tiled framebuffer can only be read by the
GPU that made it); `RR_KMS_DEVICE=/dev/dri/card1` picks the card. Run steps 1 to 3 once per render
node.

NVIDIA: the `nvidia-vaapi-driver` only decodes, so VA-API finds no encoder and the server encodes in
software (NVENC is a separate backend, not part of this).

## Troubleshooting

- `VA-API: libva isn't installed (…)`: install `libva2 libva-drm2` and the driver (step 0).
- `VA-API: no GPU render node`: no `/dev/dri/renderD*` (a VM or container without the GPU passed
  through).
- `… has no H.264 encoder`: the driver decodes only (Fedora's Mesa, iHD without `-non-free` on some
  generations, NVIDIA): see step 0's notes.
- `--kms: VA-API: importing the screen's buffer (XR24, modifier …) failed (…)`, at start: the driver
  can't import that layout. Report the line and the GPU.
- `the GPU's colour conversion failed (…): converting on the CPU`: the driver lists video processing
  but can't convert BGRX to NV12. Frames from memory still encode (the CPU converts); `--kms` won't.
- To see libva's own messages: `LIBVA_MESSAGING_LEVEL=2` (they're kept out of the log otherwise).
- A different driver than the one autodetected: `LIBVA_DRIVER_NAME=iHD` (or `i965`, `radeonsi`).
