# Verifying hardware encoding on Windows

CI's Windows runners have no GPU, so they test only Media Foundation's software H.264 encoder
(Microsoft's) and the CPU path. This script checks the rest on a real machine: each vendor's encoder
(NVIDIA NVENC, AMD AMF, Intel Quick Sync, all reached through Media Foundation), zero copy from
Windows Graphics Capture into the encoder, the speed and CPU cost, and a window and the whole display
with input.

Run it once per GPU you have. A laptop with two GPUs (Intel plus NVIDIA, say) has two encoders: see
[Machines with two GPUs](#machines-with-two-gpus).

## 0. Setup

You need Windows 10 1903 or later (Windows 11 is best: it can turn off the yellow capture border),
current GPU drivers, [Rust](https://rustup.rs), [Node 22+](https://nodejs.org) and Chrome. Open
PowerShell:

```powershell
git clone https://github.com/rein-harness/rein-remote; cd rein-remote
git checkout phase-3-windows
cargo build --release -p rr-server
$rr = ".\target\release\rein-remote.exe"
```

Write down the machine: Windows version (`winver`), GPU and driver version (Device Manager, or
`nvidia-smi` on NVIDIA), CPU.

## 1. The encoder is found, and its tests pass on it

```powershell
cargo test --release -p rr-server encode_mf -- --nocapture --test-threads=1
```

Expect every test to pass (`test result: ok. 5 passed`), and each test that opens an encoder to
print the one it got, the hardware one:

| GPU | Expected line |
|---|---|
| NVIDIA | `encoder: H.264 in hardware (NVIDIA NVENC via Media Foundation)` |
| AMD | `encoder: H.264 in hardware (AMD AMF via Media Foundation)` |
| Intel | `encoder: H.264 in hardware (Intel Quick Sync via Media Foundation)` |
| none / VM | `encoder: H.264 in software (H264 Encoder MFT via Media Foundation)` |

Also note the lines `keyframe … bytes, forced keyframe … bytes` and `10 frames at 20 Mbit/s: …
bytes; at 0.5 Mbit/s: … bytes` (the bitrate taken live: the second number is well under half the
first).

If a hardware encoder is found but can't start, the tests fall back to the next one and the server
says why on stderr once, e.g. `rein-remote: NVIDIA H.264 Encoder MFT couldn't start: …`. Copy
that line into the report.

## 2. Capture, encode and input, without a viewer

Open Notepad first (`Start-Process notepad`), then:

```powershell
& $rr windows
& $rr windows --json
& $rr selftest --window Notepad --type "rein remote was here"
$env:RR_ENCODER = "mediafoundation"; & $rr selftest --window Notepad --type " via media foundation"
$env:RR_ENCODER = "hardware";        & $rr selftest --window Notepad --type " in hardware"
$env:RR_ENCODER = "software";        & $rr selftest --window Notepad --type " in software"
Remove-Item Env:RR_ENCODER
```

`windows --json` prints `[{"id":…,"app":"notepad.exe","title":"…","width":…,"height":…},…]`,
those five fields and no others (Rein reads it). Each selftest ends like:

```text
rein-remote selftest: 1234×567, frames come, encoded with H.264 in hardware (NVIDIA NVENC via Media Foundation) (a 23456-byte keyframe), and typing changed what's seen
```

and the text appears in Notepad. `RR_ENCODER=hardware` fails (exit code 1) when there's no hardware
encoder, rather than falling back; `software` says `H.264 in software (openh264)`.

The whole display (the primary monitor; no `--window`, no `--display`): maximize Notepad, then

```powershell
& $rr selftest --type " on the whole display"
```

expects `rein-remote selftest: 2560×1440, frames come, encoded with …, and typing changed what's
seen` (the size is the monitor's in physical pixels, whatever the scaling in Settings), and the text
in Notepad: the click landed where the display's center is.

## 3. Speed: encode time, CPU, glass to glass

The test pattern carries its capture time as a barcode, which the browser client reads back to
measure glass to glass (capture to the decoded frame on screen). It comes from memory, so this
measures the encoder with the CPU's color conversion (see step 5 for zero copy).

Terminal 1:

```powershell
& $rr serve --test-pattern 1280x720 --fps 200 --listen 127.0.0.1:7979
```

Terminal 2 (while it runs):

```powershell
node tools/measure.mjs 'http://127.0.0.1:7979/?measure' --seconds 10
```

`measure.mjs` prints one JSON object. Write down:

- `fps`: frames shown a second (expect close to 200 with a hardware encoder);
- `glassMs.p50` / `glassMs.p95`: glass to glass, in ms;
- `server.encode_ms.p50` / `p95`: the server's time to encode a frame (with a pipelined hardware
  encoder, from the frame going in to it coming out);
- `server.grab_ms`: reading the frame;
- the decoder line (hardware or software in the browser).

Terminal 1 prints `rein-remote: encoding H.264 in hardware (… via Media Foundation)` when the viewer
connects: check it's the one expected. It also prints the same stats every 5 s.

CPU: in a third PowerShell, while `measure.mjs` runs, the server's share of one core over 10 s:

```powershell
$p = Get-Process rein-remote; $c0 = $p.TotalProcessorTime; Start-Sleep 10; $p.Refresh()
"{0:N1}% of one core" -f (($p.TotalProcessorTime - $c0).TotalMilliseconds / 100)
```

Then the same three steps with the software encoders, for comparison: restart terminal 1 with
`$env:RR_ENCODER = "software"` (openh264), then `"mediafoundation"` on a machine with no hardware
encoder or with the hardware one disabled. Also run the pattern idle (no viewer connected) and note
the CPU: it should be near zero.

The GPU's encoder engine should be busy only while streaming: Task Manager, Performance, the GPU,
the "Video Encode" graph (or `nvidia-smi dmon -s u`, column `enc`, on NVIDIA).

## 4. A window and the whole display, with a viewer and input

```powershell
& $rr serve --window Notepad --listen 127.0.0.1:7979
```

Open `http://127.0.0.1:7979/` in Chrome. Notepad streams; click into it in the browser and type: the
text appears in the real Notepad (it comes to the front on the first click). Move another window
over Notepad: the stream still shows Notepad's own contents. Resize Notepad: the stream keeps its
size, showing the window's top-left part.

The whole display:

```powershell
& $rr serve --listen 127.0.0.1:7979
```

The browser shows the primary monitor. Clicks land where they're made (open Start, a desktop icon),
typing goes to the focused app, the wheel scrolls. With a second monitor to the left of or above the
primary, clicks must still land on the primary monitor at the right place (the desktop's
coordinates start at the leftmost monitor).

Then measure under real motion: play a video full screen (or drag a window around continuously)
and run `node tools/measure.mjs 'http://127.0.0.1:7979/' --seconds 10` (no `?measure`: glass to
glass needs the test pattern; the frame rate, encode time and CPU still apply), and the CPU snippet
above. Then a still screen (nothing moving for 10 s): `server.still` goes up, and CPU drops to near
zero (a still screen is neither read nor encoded).

## 5. Zero copy

With a hardware encoder that reads GPU memory (all three vendors' Media Foundation encoders are
D3D11-aware), frames from a window or the display never pass through the CPU: the capture's BGRA
texture is copied on the GPU, converted to NV12 by the GPU's video processor on the same device,
and given to the encoder as a texture. It's on when, after a viewer connects to a window or the
display (step 4), the server logs:

```text
rein-remote: encoding H.264 in hardware (NVIDIA NVENC via Media Foundation)
rein-remote: zero copy: frames go from the capture into the encoder on the GPU
```

If instead it logs one of these, frames go through memory (still in hardware, at a CPU cost), and
the reason is in the line; note it in the report:

```text
rein-remote: NVIDIA H.264 Encoder MFT won't read GPU memory (…): frames go through memory
rein-remote: no GPU color conversion for … (…): frames go through memory
```

Also confirm it in the numbers: under motion (step 4), `server.grab_ms.p50` is a small fraction of
a millisecond with zero copy, and the server's CPU is far lower than with `RR_ENCODER=software`
(which reads every frame back into memory). The test pattern never uses zero copy (its frames are
made in memory).

## Machines with two GPUs

Graphics Capture's device is on the default adapter (the one the desktop is drawn on). A hardware
encoder on the other GPU (an NVIDIA GPU in a laptop whose desktop is on Intel graphics) still works,
but can't share the capture's device: expect the "won't read GPU memory" line and frames through
memory. To check the other encoder, set Windows' graphics preference for `rein-remote.exe` to the
other GPU (Settings, System, Display, Graphics) and run steps 1 to 5 again; write down which GPU the
desktop is on.

## What to report

For each GPU: the machine (Windows version, GPU, driver, CPU); the encoder line from step 1 and the
test result; the selftest lines from step 2; from step 3 the fps, glass to glass p50/p95,
`encode_ms` p50/p95 and CPU % for the hardware encoder and for openh264; from step 4 that input
worked on a window and on the display, the CPU under motion and when still; from step 5 whether the
zero-copy line appeared, and `grab_ms` with it.
