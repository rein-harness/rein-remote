# Verifying sound

What to run, and what to see, to check that `rein-remote serve` streams the sound the machine plays
and that the web client plays it in time with the picture. macOS and a Linux container were checked
this way when sound went in (the numbers are in the pull request that added it); Windows and a real
Linux desktop are checked by hand with the steps below. CI covers what a runner can: the Opus encoder
(a tone through libopus and back), the session (sound only to clients that ask for it, video on its
own when there's no sound to be had), and, on Windows, that a runner without an output device still
streams video.

Two tools, both in `tools/`, both need Node 22 or later:

- `listen.mjs URL` connects as a client that plays sound, without a browser, and prints what came
  (audio packets a second, their size, video frames) next to the server's own `/stats`, whose
  `audio` says what it captured: `packets`, `loud_packets` (louder than -60 dBFS) and `level_db`
  (the loudest packet of the last second). Silence is about -120; a sound playing is above -60.
- `measure.mjs URL` opens the page in headless Chrome and prints what the page measured, with sound
  too: packets played and dropped, the offset sound and picture share, and with the test pattern and
  `?measure`, each beep against its flash (`avSyncMs`: positive, the sound comes after the picture).

The stderr lines to look for:

- `rein-remote: sound: what this machine plays` when the first viewer that plays sound connects (the
  capture starts then, and stops a few seconds after the last one goes);
- or `rein-remote: no sound (REASON): the video streams on its own`, where there's none to be had;
- `… connected (… with sound)` (WebSocket) or `(… sound as datagrams)` (WebTransport) for each viewer.

## Windows

On a Windows 10 or 11 machine with speakers or headphones (any output device; it needn't be audible).

1. Build, or take a release build: `cargo build --release -p rr-server`.
2. Start Notepad, then stream it:

   ```powershell
   Start-Process notepad
   .\target\release\rein-remote.exe serve --window Notepad --listen 127.0.0.1:7979
   ```

3. Sound to check against: play something, for instance a system sound on a loop in a second
   PowerShell:

   ```powershell
   while ($true) { [System.Media.SystemSounds]::Asterisk.Play(); Start-Sleep -Milliseconds 700 }
   ```

4. In a third PowerShell, listen for 5 s with nothing playing (stop step 3 first), then with it:

   ```powershell
   node tools\listen.mjs http://127.0.0.1:7979/ --seconds 5
   ```

   Expect, with sound playing: `serverSaysAudio: true`, `audioPerSecond` about 100 (fewer when
   nothing plays at all: WASAPI loopback sends nothing during true silence, which the client hears
   as silence), `audioKbps` about 60–100, `packetSpacingMs.p50` 10, and in `serverAudio`,
   `loud_packets` climbing and `level_db` above -60. With nothing playing, `loud_packets` stays where
   it was and `level_db` reads about -120. The server prints `rein-remote: sound: what this machine
   plays`.

5. In Chrome or Edge, open `http://127.0.0.1:7979/`, click the picture once (browsers start sound
   only after a click; until then the page shows "Tap for sound"), and listen: the system sound plays
   from the page. The bar shows `sound N ms`, and while sound plays, how long the picture waits for it.
6. A machine with no output device (disable it in Sound settings → Output, or a VM without one):
   step 4 shows video frames and no audio packets, and the server prints `rein-remote: no sound (no
   audio output device: …): the video streams on its own`. The picture keeps streaming.
7. Sync, with the test pattern (no capture involved; its tone beeps when it flashes):

   ```powershell
   .\target\release\rein-remote.exe serve --test-pattern --listen 127.0.0.1:7980
   node tools\measure.mjs "http://127.0.0.1:7980/?measure" --seconds 30 --warmup 5
   ```

   Expect `avSyncMs.p50` within ±20 ms and `p95abs` close to it, `audio.dropped` a few at most.
   With `?measure&sync=off` the picture doesn't wait for the sound: `glassMs` drops, and `avSyncMs`
   shows how far behind the sound is then.

## Linux desktop

On a desktop with PulseAudio or PipeWire (with pipewire-pulse, as Ubuntu, Fedora and Debian have by
default), X11 (Xorg, or Xwayland for an X11 app's window).

1. `cargo build --release -p rr-server`, then stream the screen (or a window):

   ```sh
   ./target/release/rein-remote serve --listen 127.0.0.1:7979          # $DISPLAY
   ./target/release/rein-remote serve --window Firefox --listen 127.0.0.1:7979
   ```

   The sound is the default output's monitor (`@DEFAULT_MONITOR@`): everything the machine plays,
   whichever window is shown. Another source: `REIN_REMOTE_AUDIO_SOURCE=NAME` (a name from
   `pactl list short sources`).

2. Play something on a loop:

   ```sh
   while true; do paplay /usr/share/sounds/freedesktop/stereo/bell.oga; sleep 0.5; done
   ```

   (or `pactl load-module module-sine frequency=440`, and `pactl unload-module module-sine` after).

3. `node tools/listen.mjs http://127.0.0.1:7979/ --seconds 5`: expect `audioPerSecond` 100 (the
   monitor sends silence too), `packetSpacingMs.p50` 10, `loud_packets` climbing and `level_db`
   above -60 while it plays, about -120 when it doesn't.
4. In Chrome or Firefox (Firefox has WebCodecs' AudioDecoder from version 130), open
   `http://127.0.0.1:7979/`, click once, listen.
5. Without libpulse (`sudo apt remove libpulse0` in a throwaway VM, or a container without it): the
   server runs as before, and the first viewer gets `rein-remote: no sound (libpulse-simple isn't
   installed (…)): the video streams on its own`.
6. Sync: as on Windows, step 7.

## macOS

ScreenCaptureKit captures the sound with the same Screen Recording permission as the picture: with
`--display` (or no source), everything the machine plays; with `--window` or `--app`, that app's
sound only. rein-remote's own sound is left out.

```sh
./target/release/rein-remote serve --listen 127.0.0.1:7979 &
node tools/listen.mjs http://127.0.0.1:7979/ --seconds 4                      # silence: loud_packets 0
afplay /System/Library/Sounds/Ping.aiff & node tools/listen.mjs http://127.0.0.1:7979/ --seconds 4
```

## A container (Linux, no desktop)

What was run for the pull request, in Docker (Colima on a Mac): PulseAudio with a null sink as the
default output, a sine wave played into it, Xvfb for the picture.

```sh
pulseaudio -D --exit-idle-time=-1
pactl load-module module-null-sink sink_name=rr && pactl set-default-sink rr
Xvfb :20 -screen 0 1280x720x24 &
DISPLAY=:20 rein-remote serve --display :20 --listen 0.0.0.0:7979 --token T --mdns off --pair off &
node tools/listen.mjs 'http://HOST:7979/?token=T' --seconds 4          # silence
pactl load-module module-sine sink=rr frequency=440
node tools/listen.mjs 'http://HOST:7979/?token=T' --seconds 4          # the tone
```
