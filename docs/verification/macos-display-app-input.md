# Manual test: macOS whole display and `--app`, with input

Capture of a whole display and of one app (all its windows) is verified on a real Mac (see
ROADMAP.md, phase 3); input into them needs the Accessibility permission for the app that runs
rein-remote, which the development terminal didn't have. This checks input on both.

## Setup

1. System Settings → Privacy & Security → **Screen Recording**: allow your terminal (or the app that
   runs rein-remote). Then **Accessibility**: allow it too. (No restart needed for Accessibility.)
2. Check: `swift -e 'import ApplicationServices; print(AXIsProcessTrusted())'` prints `true` when run
   from that terminal.
3. Build: `cargo build --release -p rr-server`.

## The whole display

```sh
./target/release/rein-remote displays                 # 1 is the main display
./target/release/rein-remote selftest --display 1 --type "rein remote was here"
```

With TextEdit open in front with an empty document, the selftest clicks the middle of the screen and
types: expect `rein-remote selftest: W×H, frames come, and typing changed the window`.

Then streamed, from another device on the same network:

```sh
./target/release/rein-remote serve --display 1
```

Open the printed "this network" link on a phone or another computer. Expect: the desktop shows; the
pointer moves where you point; a click on a Dock icon opens that app; typing goes to the focused app.

## One app

```sh
open -a Calculator
./target/release/rein-remote serve --app Calculator
```

Expect: only Calculator's window(s) on an otherwise empty picture (no desktop, no other apps). Click
7, +, 2, = in the viewer: Calculator shows 9, and is brought to the front by the first click (when it
was behind another app). Type `5*3` then Enter: it shows 15.

## Hardware and software encoders

Repeat the display test with `RR_ENCODER=software` (openh264) and without (VideoToolbox), and with
`--codec h265` (HEVC; Chrome and Safari decode it in hardware on a Mac). The log's
`rein-remote: encoding …` line says which is used.
