# Manual test: high refresh (a 120–240 Hz display) and 200 fps glass to glass

What CI and the development Mac can't show: a viewer on a display faster than 60 Hz, with the
server pacing to it. What's already measured is in ROADMAP.md (phase 3); this checks the rest.

## Needs

- A server machine with a hardware encoder (a Mac with Apple silicon, or Windows/Linux with an
  NVIDIA, AMD or Intel GPU; see `windows-hardware-encoders.md` and `linux-vaapi-kms.md`).
- A viewer with a 120 Hz or faster display (a ProMotion MacBook or iPad, a 144/165/240 Hz monitor)
  and Chrome or Edge (WebCodecs). For the 200 Hz line of the roadmap: a 200 Hz or faster display.
- Both on the same machine for glass to glass (the barcode carries the server's clock), or two
  machines with their clocks synced (`sntp -sS time.apple.com` / `w32tm /resync`), for the rest.

## 1. The client measures its display, the server paces to it

On the server:

```sh
rein-remote serve --test-pattern 1280x720 --listen 0.0.0.0:7979 --webtransport off
```

On the viewer (the fast display, the browser window on it, not minimized): open
`http://SERVER:7979/?measure` (or `http://127.0.0.1:7979/?measure` on the same machine).

Expect:

- the server's log: `rein-remote: … connected (H264 1280×720 at N fps, its display R Hz)` where R is
  the display's real rate (e.g. 119.9, 143.9, 239.8) and N is R rounded, at most 240 (60 when the
  server encodes in software: `RR_ENCODER=software` shows that cap);
- the page's status line: `… frames (N fps, display R Hz) …`, with N within a few frames of R while
  the pattern moves.

Write down R, N, and the server's CPU (`top -pid $(pgrep rein-remote)` on macOS, `top -p` on
Linux, Task Manager on Windows) while it streams and after the page is closed.

## 2. 200 fps glass to glass

On one machine with a hardware encoder:

```sh
rein-remote serve --test-pattern 1280x720 --fps 200 --listen 127.0.0.1:7979 --webtransport off &
node tools/measure.mjs 'http://127.0.0.1:7979/?measure' --seconds 10
```

`tools/measure.mjs` prints `fps` (frames shown a second), `glassMs.p50/p95` (capture to decoded and
drawn, read from the barcode in each frame), the decoder, and the server's `/stats` (`encode_ms`,
`grab_ms`). Pass: fps ≥ 195, glass p50 ≤ 15 ms. Then open the same URL in Chrome on the 200 Hz
display itself and check the status line shows ~200 fps and `display 2xx Hz`.

CPU, both: idle (`rein-remote serve --display …` with nothing changing on screen, 30 s) and under
motion (the test pattern at 200 fps), from `ps -o time= -p PID` before and after a timed interval.

## 3. A still screen costs nothing

```sh
rein-remote serve --listen 127.0.0.1:7979    # the screen (macOS: the main display; Linux: $DISPLAY)
```

Open the page, then leave the screen untouched for 30 s: the server's `/stats?token=…` `frames`
must not grow (only `still` does), and its CPU time stays flat.
