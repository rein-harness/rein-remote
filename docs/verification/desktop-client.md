# Verifying the desktop viewer (`rein-remote-viewer`)

The viewer is one program on Windows, macOS and Linux: rr-client's connection (WebTransport with
datagrams, the WebSocket, or a relay and then direct), a decoder (the platform's hardware one, else
openh264), a window (winit; the picture through softbuffer, or on macOS through Core Animation with
no copy at all), the remote pointer drawn locally, and the mouse, keyboard and gamepads sent back.

What CI checks on every push, on Linux, macOS and Windows runners (`Viewer smoke test` in ci.yml):
the viewer connects to a test-pattern server on the runner, decodes 120 frames in software and with
whatever the runner offers (no GPU on the Linux and Windows runners: the viewer must say so and use
openh264; on Windows also Media Foundation's own decoder, `RR_DECODER=mediafoundation`), the pointer
it sends shows up in the decoded picture (the pattern draws a cross there), and a window opens and
shows frames (Linux: on Xvfb), reading the pattern's barcode for latency. The server's virtual
gamepad is made through uinput on the Linux runner and read back through evdev.

What CI can't check, and this page is for: hardware decoding on a real GPU (DXVA on Windows,
VA-API on Linux), a real desktop's window and input, and real gamepads. Run it on each machine
you have, and fill in the report at the end.

## 0. What was measured on a Mac (for comparison)

A MacBook (Apple A18 Pro, macOS 26), server and viewer on the same machine (same clock, so the
barcode gives glass to glass: capture to the picture handed to the compositor; the display's next
refresh adds up to one more frame):

| Source | Decoder | Glass to glass p50 / p95 | fps | Viewer CPU (of one core) |
|---|---|---|---|---|
| test pattern 1280×720, window | VideoToolbox H.264 | 9 ms / 12 ms | 60 | ~10% |
| test pattern 1280×720, headless | VideoToolbox HEVC | 9 ms / 18 ms | 62 | 5% |
| test pattern 1280×720, headless | openh264 (software), VideoToolbox-encoded stream | 13 ms / 15 ms | 60 | 44% |
| test pattern 1280×720, headless | openh264, openh264-encoded stream | 7 ms / 8 ms | 60 | 15% |
| display 3274×2048, window (changing) | VideoToolbox H.264 | — (capture to decoded 31 ms p50) | 26 | 5–10% |

Before zero copy (the picture copied out of the decoder, scaled into a Retina-sized buffer and
handed to softbuffer) the same window took 34% at 720p and 85–107% on the 3274×2048 display, and
24 ms glass to glass; the numbers above are with the decoder's IOSurface as a layer's contents.

Input on the Mac: the pointer sent by `--move` came back as the server's pointer at that place
(display) or drew the pattern's cross (test pattern, 16–24 ms after it was sent); keys typed into the
viewer's window (66 key events, Shift included) appeared in TextEdit, streamed with `serve --window`
while it was behind other windows, with the right case and line breaks; a click raised TextEdit.
Started with no argument, the viewer listed the Mac's server (mDNS), took its number and the pairing
code it printed, paired and streamed; started again by name (`rein-remote-viewer Mac`) it signed in
with its key, no code. Through `rein-remote relay` the session moved to `direct-after-punch` and
decoded straight on (13 ms / 15 ms).

## 1. Build

Windows (PowerShell), with [Rust](https://rustup.rs):

```powershell
git clone https://github.com/rein-harness/rein-remote; cd rein-remote
git checkout phase-5-desktop
cargo build --release -p rr-server -p rr-viewer
$rr = ".\target\release\rein-remote.exe"; $v = ".\target\release\rein-remote-viewer.exe"
```

Linux (bash), with Rust, and libudev's headers for gamepads:

```sh
sudo apt-get install -y libudev-dev vainfo      # Fedora: systemd-devel libva-utils
git clone https://github.com/rein-harness/rein-remote && cd rein-remote
git checkout phase-5-desktop
cargo build --release -p rr-server -p rr-viewer
rr=./target/release/rein-remote; v=./target/release/rein-remote-viewer
```

Write down the machine: OS and version, GPU and driver (Windows: Device Manager; Linux: `vainfo`'s
"Driver version" line), CPU, display and its refresh rate.

## 2. Decoding, headless (no window)

Start a test-pattern server on the same machine (same clock: the barcode is exact):

```sh
$rr serve --test-pattern 1280x720 --listen 127.0.0.1:7982 --token tok --mdns off
```

In another terminal, each of these must end with a summary and exit 0:

```sh
$v "http://127.0.0.1:7982/?token=tok" --headless --frames 600 --decoder hardware --move 640,500
$v "http://127.0.0.1:7982/?token=tok" --headless --frames 600 --decoder software --move 640,500
$v "http://127.0.0.1:7982/?token=tok" --headless --frames 600 --move 640,500     # auto: hardware first
```

Expect, for the first:

| Machine | `decoded with …` |
|---|---|
| Windows, any GPU with a DXVA H.264 decoder | `Media Foundation H.264 (hardware, DXVA)` |
| Linux, Intel (iHD or i965) or AMD (Mesa) | `VA-API H.264 (hardware, <the driver's name>)` |

and in the summary line: `600 frames` (or more), `(600 read)` — **every frame's barcode read**. A
hardware decoder that gets the stream's references wrong shows it here first: barcodes that don't
read, or glass-to-glass numbers that jump to hundreds of ms (an old picture shown again). Write
down glass to glass p50 / p95 and CPU for hardware and software.

Then the same against a server encoding in hardware where the server machine has one (the default)
and in software (`RR_ENCODER=software`), since the streams differ (Main/High profile with CABAC from
hardware encoders; openh264's screen mode with long-term references):

```sh
RR_ENCODER=software $rr serve --test-pattern 1920x1080 --listen 127.0.0.1:7983 --token tok --mdns off
$v "http://127.0.0.1:7983/?token=tok" --headless --frames 600 --decoder hardware
```

Linux only: VA-API's view of the stream is our own (crates/rr-viewer/src/decode/h264.rs reads the
headers and keeps the references). If frames don't read, run with `--decoder software` to confirm the
stream itself is fine, and attach the server's stream (`$rr selftest --test-pattern 1280x720 --encode
/tmp/s.h264 --frames 60`) to the report.

HEVC (Windows, where the GPU decodes it and "HEVC Video Extensions" are installed): a server that
encodes HEVC in hardware (`serve --codec h265`, a Mac or a Windows machine with a hardware HEVC
encoder) should give `decoded with Media Foundation HEVC (hardware, DXVA)`. Linux decodes H.264 only
in hardware; the viewer asks for H.264 there.

## 3. The window

```sh
$v "http://127.0.0.1:7982/?token=tok" --measure --seconds 20
```

A window opens the stream's size (smaller if the screen is), its title saying the server, the
transport (`webtransport-datagrams`), the decoder, fps and ms glass to glass. Check:

- [ ] The picture fills the window with its aspect kept (resize it: black bars, never stretched).
- [ ] Moving the mouse over it: the pattern's red cross follows the pointer; the pointer you see is
      drawn by the viewer (the system's is hidden over the window).
- [ ] Ctrl+Alt+Enter: full screen and back.
- [ ] At the end it prints frames, fps, glass to glass p50 / p95 and CPU. Write them down; also the
      viewer's CPU in Task Manager / `top` while it runs.

Linux: do this on X11 and on Wayland (the viewer talks to either; softbuffer draws on both).

## 4. A real desktop with input

On a second machine (or the same one), stream a real app and drive it from the viewer:

```sh
$rr serve --window Notepad        # Windows (Linux: --window xterm, or nothing for the whole screen)
$v "<a link it printed>"
```

- [ ] Click into the app: it takes focus (a click raises the window). Double-click selects a word,
      right-click opens its menu, the wheel scrolls (both directions; a trackpad's two fingers too).
- [ ] Type: lower and upper case, digits, punctuation (`!@#$%^&*()`), Enter, Backspace, Tab, the
      arrows, Home/End, an accented letter from a dead key (é), Ctrl+A / Ctrl+C / Ctrl+V inside the
      app. With an input method (Chinese, Japanese) the committed text arrives as text.
- [ ] Switch away from the viewer while holding a key: nothing stays held on the server.
- [ ] By name on the network: `$v` with no argument lists the machines found (mDNS); press its number.
      Not paired yet: type the code the server shows (`serve` prints it with `--pair on`), Enter.
      Next time it connects with no code (`$v <name>` too).
- [ ] Through a relay (`serve --relay …`): the title says `relay`, then `direct-after-punch` when the
      two can reach each other.

## 5. Gamepads

The viewer reads any pad the system knows (XInput and Windows.Gaming.Input on Windows, evdev on
Linux, IOKit on macOS) and sends its whole state when it changes. The server plays it through a
virtual Xbox 360 controller.

Server on Windows: install [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) (the driver)
and put `ViGEmClient.dll` (built from [ViGEmClient](https://github.com/nefarius/ViGEmClient), x64)
next to `rein-remote.exe`. Without them the server says once, when a pad's input first comes:
`rein-remote: the viewer's gamepad can't be played here: ViGEmClient.dll isn't here: install ViGEmBus …`.

Server on Linux: the server needs to write `/dev/uinput`:

```sh
sudo modprobe uinput
echo 'KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"' | sudo tee /etc/udev/rules.d/60-rein-remote-uinput.rules
sudo udevadm control --reload && sudo udevadm trigger
sudo usermod -aG input $USER   # then log in again
```

Server on macOS: there's no public API for a virtual gamepad, so a Mac server can't play one; its
Hello doesn't offer gamepads and the viewer sends none. (A Mac viewer's pads work against a Windows
or Linux server.)

Then, with a pad plugged into the viewer's machine and the viewer connected:

- [ ] The viewer prints `gamepad 0: <its name>`; the server prints `gamepad 0: a virtual Xbox 360
      controller (ViGEmBus|uinput)` on the first press.
- [ ] Windows server: `joy.cpl` (Game Controllers) lists an "Xbox 360 Controller for Windows";
      its Properties page shows every button, both sticks (up is up), both triggers (analog) and the
      d-pad following the real pad. Linux server: `evtest` lists "Rein Remote gamepad 0 (Xbox 360)",
      and it shows `BTN_SOUTH`, `ABS_X`/`ABS_Y` (−32768…32767), `ABS_Z`/`ABS_RZ` (0…255) and the hat
      as you press; a game or Steam sees an Xbox 360 pad.
- [ ] Unplug the pad mid-press: nothing stays pressed on the server. Close the viewer: the virtual pad
      goes away.
- [ ] Two pads: two virtual pads, 0 and 1.

## Report

```
Machine:        (OS, version, CPU, GPU, driver, display Hz)
Server:         (machine, encoder line it printed)
Decoder:        hardware: (the "decoded with" line)      software: openh264
Headless 720p:  hw p50/p95 … ms, … fps, CPU …%, … of … barcodes read
                sw p50/p95 … ms, … fps, CPU …%
Window 720p:    p50/p95 … ms, … fps, CPU …%
Input:          mouse ✓/✗  wheel ✓/✗  keys ✓/✗  text/IME ✓/✗  focus loss ✓/✗
Picker/pairing: ✓/✗      Relay → direct: ✓/✗
Gamepads:       viewer saw the pad ✓/✗   server's virtual pad ✓/✗   buttons/sticks/triggers ✓/✗
Anything odd:
```
