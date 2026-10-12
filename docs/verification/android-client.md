# Verifying the Android client

The app is `clients/android`: Kotlin with no extra libraries (platform Views), on the same Rust core
as the iOS app (`crates/rr-ffi`, rr-client behind a C ABI), reached through JNI (`crates/rr-jni`,
`librr_jni.so`, built with cargo-ndk). The emulator could check most of it, and it did (below, with
the numbers). A real phone and its hardware are checked by hand with the steps after that.

## Building

On macOS or Linux, with Rust, a JDK 17 or later, and the Android SDK (platform 35, build-tools, the
NDK):

```sh
rustup target add aarch64-linux-android     # x86_64-linux-android too, for an x86 emulator
cargo install cargo-ndk
cd clients/android
./build-core.sh                              # the core into app/src/main/jniLibs (arm64-v8a; ./build-core.sh arm64-v8a x86_64)
./gradlew assembleDebug                      # app/build/outputs/apk/debug/app-debug.apk
./gradlew testDebugUnitTest                  # the unit tests (no device)
```

Run `./build-core.sh` again after changing anything in `crates/`. `ANDROID_HOME` must point at the
SDK. The NDK is found there, or through `ANDROID_NDK_HOME`.

For measuring, use a release build. A debuggable build checks every JNI call and runs slower. The
release build is signed with the debug key, for testing only, and nothing is published:
`./gradlew -Prr.testBuildType=release assembleRelease assembleReleaseAndroidTest`.

## What the emulator checks (and CI, on every push)

The emulator reaches the machine it runs on at `10.0.2.2`, which maps to that machine's loopback.
The server therefore listens on loopback with a token, which is the safe form:

```sh
rein-remote serve --test-pattern 1280x720 --listen 127.0.0.1:7979 --token t --mdns off &
emulator -avd <avd> -no-window -no-audio -gpu swiftshader_indirect -no-snapshot &
adb install -r -t app/build/outputs/apk/release/app-release.apk
adb install -r -t app/build/outputs/apk/androidTest/release/app-release-androidTest.apk
adb shell am instrument -w -r -e rrUrl 'http://10.0.2.2:7979/?token=t' \
  dev.reinharness.remote.test/androidx.test.runner.AndroidJUnitRunner
adb logcat -s rein-remote          # the selftest: line, and what the decoder did
```

(Or `./gradlew connectedReleaseAndroidTest -Prr.testBuildType=release
-Pandroid.testInstrumentationRunnerArguments.rrUrl='http://10.0.2.2:7979/?token=t'`.)

- **Units** (`app/src/test`, on the JVM): the touch gestures (tap, swipe, hold and drag, two-finger
  tap, a cancelled touch), key names and HID usages, the on-screen keyboard's composed words sent
  as differences, gamepads in the standard layout (buttons, hat, analog triggers, sticks), Annex B
  and parameter sets (H.264, HEVC), the barcode reader (also at half size), the cross, links and
  TXT records, the Opus header.
- **The whole way** (`StreamTests.streamsAndControls`, the self-test): the app connects through the
  core and decodes with MediaCodec onto the SurfaceView. It sends the pointer to three places and
  reads the screen's own buffer back (PixelCopy) until the test pattern's red cross is at each
  place: the input reached the server and came back in the video. Then it measures for 8 to 10 s.
- **A tap through the input system** (`aTapThroughTheViewClicks`): a touch injected the way a finger's
  comes (UiAutomation) goes through the view and the gestures as a click, sent at the right stream
  pixel. The cross then shows where it landed.
- **Pairing** (`pairsThenSignsIn`, with `-e rrPairUrl http://10.0.2.2:7980/ -e rrPairCode CODE`
  against `serve --listen 127.0.0.1:7980 --pair on`): the app pairs with the code and streams. It
  then connects again with no code, signed in with the key it kept.
- `-e rrCodec HEVC` against `serve --codec h265`; `-e rrWebTransport off` for the WebSocket;
  `-e rrDecoder c2.android.avc.decoder` for a decoder by name.

The same self-test runs from the app itself, for a device:

```sh
adb shell am start -n dev.reinharness.remote/.StreamActivity -e url "'http://<server>:7979/?token=…'" --ez selftest true
```

Other extras: `--ez measure true` (the numbers on screen), `-e seconds 10`, `--ez audio false`,
`--ez webtransport false`, `--ez hevc true`, `-e decoder NAME`. The line shows in the bar and in
`adb logcat -s rein-remote`.

### Measured in the emulator

Setup: an Android 15 emulator (google_apis, arm64-v8a, 2 GB, swiftshader, `-no-audio`) on an Apple
silicon Mac with 8 GB. Other work ran on the same Mac the whole time: load average 12 to 22 on 6
cores, and 6 GB of swap in use. The server sent the test pattern at 60 fps. Release build, runs of
10 s:

| Decoder (as the app reports it) | Path | Picture | fps | Glass to glass p50 / p95 | Decode p50 / p95 | Pointer to cross seen |
|---|---|---|---|---|---|---|
| `c2.goldfish.h264.decoder` (H.264) | WebTransport datagrams | 1280×720 | 59.6 | 13 ms / 29 ms | 5.6 ms / 29 ms | 52–62 ms |
| `c2.goldfish.h264.decoder` (H.264) | WebTransport datagrams | 640×360 | 51.6–52.4 | 8–11 ms / 25–40 ms | 3.1–4.4 ms / 41–60 ms | 53–57 ms |
| `c2.android.avc.decoder` (H.264, software) | WebTransport datagrams | 640×360 | 27–48.5 | 51–76 ms / 96–192 ms | 45–71 ms / 96–209 ms | 151–202 ms |
| `c2.android.hevc.decoder` (HEVC, software) | WebTransport datagrams | 640×360 | 54.3 | 12 ms / 57 ms | 5.1 ms / 52 ms | 60–68 ms |
| `c2.goldfish.h264.decoder` (H.264) | WebSocket | 640×360 | 12.5 (during a load spike) | 25 ms / 210 ms | 10.5 ms / 139 ms | 53–82 ms |

- **Which decoder this is.** `c2.goldfish.*` is the emulator's codec. Android reports it as
  hardware, but it hands the stream to the Mac through the emulator's pipe and decodes it there.
  `c2.android.*` is Android's own software decoder, running on the emulated CPU. Neither is a
  phone's hardware decoder (Qualcomm's `c2.qti.*`, Samsung's `c2.exynos.*`, MediaTek's `c2.mtk.*`,
  Pixel's `c2.google.*` or `c2.exynos.*`), which is the point of the real-phone steps below.
- **Glass to glass** is the test pattern's barcode (its capture time). The app reads the barcode
  from the screen's own buffer, so it measures the frame that is actually there. Each value runs to
  the moment that frame was handed to the display (`releaseOutputBuffer`, render now). The
  display's next refresh adds up to one more frame. "glass_seen" in the self-test's line is the
  same, but to when the copy of the screen saw the frame there: an upper bound that includes the
  copy. The emulator's clock isn't the Mac's, so times are put on one clock with the core's
  estimate of the offset, from its pings (good to about half the round trip, under 2 ms here).
- **The picture's size.** The busy Mac made the emulator late, the server's adaptation lowered the
  picture to 640×360, and it stayed there in most runs. The barcode is read at any of the sizes the
  server scales to.
- **Pointer to cross seen** is from sending a pointer move to the first copy of the screen with the
  cross there. It covers the way to the server, a frame's capture, and that frame's way back, plus
  up to one copy (about 50 ms apart).
- **Sound.** Opus was decoded by `c2.android.opus.decoder` (977 of the 1081 packets that arrived in
  one 10 s run; the self-test doesn't say whether the rest were dropped or still queued). Nothing was heard: with `-no-audio`, the
  emulator's AudioTrack never advances (its playback head stayed at 0 ms), so the player kept
  dropping what it couldn't play rather than falling behind. On a phone, check it by ear (below).
- **Pairing**: paired with the code, then streamed again signed in with the key. The server listed
  the emulator under `rein-remote paired`. The app's files are owner-only (0600 files, 0700 folder)
  in `no_backup/rein-remote`, which Android keeps out of backups and device transfers.
- **Through the UI**: the server list, then Add a computer with a link typed in, then Connect, then
  streaming. The stats button shows the decoder, fps, the path, the round trip, decoding, and sound.

This is a pipeline check, not a phone's numbers. The emulator decodes on the Mac or in software,
shares the Mac's loopback (no Wi-Fi), has no speaker, gamepad, keyboard or real touch, and ran here
on a machine that was busy with other work.

CI (`android` and `android-emulator` in .github/workflows/ci.yml) builds the APK, runs the unit
tests, and runs the instrumented tests in an x86_64 emulator (KVM) on a Linux runner, against a
server on the runner.

## On a real Android phone or tablet

What the emulator can't check: the phone's hardware decoder and its latency, Wi-Fi, real touch, the
on-screen keyboard (Gboard, Samsung Keyboard), a hardware keyboard, a mouse, a gamepad, the
speaker, and finding servers on the network (mDNS doesn't cross the emulator's network).

### Install

1. On the phone: Settings > About phone, tap Build number seven times (Developer options), then
   Settings > System > Developer options > USB debugging on. Connect it by USB and allow the
   computer. `adb devices` lists it.
2. Build for it: `clients/android/build-core.sh` (arm64-v8a, every phone since about 2017), then
   `./gradlew -Prr.testBuildType=release assembleRelease assembleReleaseAndroidTest` and
   `adb install -r -t app/build/outputs/apk/release/app-release.apk` (and the `-androidTest` APK to
   run the tests on the phone). Android 8.0 (API 26) or later.

### The server

On a Mac, Linux or Windows machine on the same Wi-Fi:

```sh
rein-remote serve --test-pattern 1280x720 --listen 0.0.0.0:7979     # for the numbers
rein-remote serve --window TextEdit --listen 0.0.0.0:7979           # input on a real app (macOS)
rein-remote serve --listen 0.0.0.0:7979                             # the screen (Linux: $DISPLAY; Windows)
```

It prints its links (each carries a token), a pairing code and a QR code, and advertises itself on
the network.

### What to check

Write down the phone, its Android version and the server's machine, how they're connected, and,
for each item, what happened.

1. **Finding it.** Open Rein Remote. The server shows under "On this network" by its name within a
   few seconds (Android's own DNS-SD; no permission is asked for). A guest Wi-Fi or one that
   isolates clients shows nothing: use a link instead (item 4).
2. **Pairing by code.** Tap the server, enter the code it printed, then Connect. The stream starts,
   and the server prints `paired with <the phone's model>`. Go back, then tap the server again: it
   says "paired" and connects with no code.
3. **Pairing by its QR code.** The app has no camera scanner. Instead, scan the QR code with the
   phone's camera app, copy the link it shows (`http://…/#pair=CODE&id=…`), and paste it into Add a
   computer. The code inside the link is used.
4. **A link.** Add a computer, then paste a link the server printed (`http://…/?token=…`). With
   Tailscale and its HTTPS on, try the `https://` tailnet link too: a real certificate, checked
   against Mozilla's roots that the core carries. Through a relay (`serve --relay …`), use its
   `…/?room=…#k=…` link. The server says whether a direct path was punched; the stats show
   `direct-after-punch` or `relay`.
5. **Hardware decoding and latency** (the test pattern), with the phone's clock on network time
   (Settings > System > Date & time > automatic). The app corrects for a clock that's off, but a
   good clock makes the number easier to trust.

   ```sh
   adb shell am start -n dev.reinharness.remote/.StreamActivity -e url "'http://<server>:7979/?token=…'" --ez selftest true -e seconds 15
   adb logcat -s rein-remote | grep selftest
   ```

   Expect `PASS`, `codec=H.264_1280×720_c2.<vendor>…_(hardware…)` (with `low_latency` where the
   decoder offers it: Android 11 and later), fps near 60, and the cross seen at all three points.
   Write down the whole line. Three runs give a spread. Then with `serve --codec h265` on a Mac (or
   a machine with a hardware HEVC encoder): the app offers HEVC only where the phone decodes it in
   hardware, and the line says `HEVC …`. Also with `--ez webtransport false` (the WebSocket). If
   glass to glass sits a whole frame or more above decode plus the network, the decoder is holding
   frames back: write down which decoder it was. The server's stream says it never reorders
   (`max_num_reorder_frames=0`), so the decoder shouldn't hold any.
6. **Touch** (on a real app: TextEdit, a terminal, a browser):
   - A tap clicks: the caret moves there.
   - A swipe scrolls what's under the finger, and the content follows it.
   - Hold half a second (a buzz), then move: that drags (selects text).
   - A two-finger tap right-clicks: a context menu appears between the fingers.
7. **The on-screen keyboard.** Press the keyboard button, then type words, press Enter and Delete.
   They arrive as typed, with no autocorrect. With Gboard's word-by-word composing, a corrected word
   arrives corrected and isn't typed twice. Enter arrives as Enter, Delete as Backspace. Emoji and
   accented letters arrive as text. Try Samsung Keyboard too, if there is one.
8. **A hardware keyboard** (USB-C or Bluetooth): letters, Shift for capitals, arrows, Escape, Tab,
   Enter, Backspace, F-keys, Ctrl+C / Ctrl+V in a terminal. Android keeps some keys for itself
   (Meta alone, the Back key).
9. **A mouse or trackpad** (a tablet, a Chromebook, or a phone with a Bluetooth mouse):
   - Moving without clicking moves the remote pointer, drawn by the app in the server's shape.
   - Left, right and middle clicks work, and the wheel scrolls.
   - Click and drag selects.
10. **A gamepad** (an Xbox, PlayStation, Switch Pro or any HID pad, paired over Bluetooth or USB),
    against a Windows or Linux server set up to play gamepads (ViGEmBus, or write access to
    `/dev/uinput`: see [desktop-client.md](desktop-client.md), section 5). A Mac server can't play
    them and says so.
    - With the stats on, a line `gamepads: 1 · buttons 0x… · left x,y · sent` changes as buttons and
      sticks move: A is 0x1, B 0x2, the d-pad's right 0x8000, Home 0x10000; up on the left stick is
      negative.
    - On the first press, the server prints `gamepad 0: a virtual Xbox 360 controller (…)`.
    - `joy.cpl` (Windows) or `evtest` (Linux) shows every button, both sticks (up is up), both
      triggers (analog, and pressed past half way) and the d-pad.
    - Switch the pad off mid-press: nothing stays pressed.
    - Pads that report the d-pad as a hat (most) and as keys (some) should both work. Write down
      the pad.
11. **Sound** (the test pattern plays a tone, with a beep each second at the white flash, top left):
    heard from the speaker, with the beep together with the flash. The stats count packets decoded,
    played and dropped; played should keep up with decoded. With a real machine, play something on
    it and it plays here.
12. **Rotation, split screen, and leaving the app.**
    - Turn the phone, or use split screen: the picture stays fitted, and taps land where they're
      aimed.
    - Go home and come back: the picture comes back within a keyframe (the decoder is made again
      for the new surface).
13. **Battery and heat** (optional): 10 minutes of the test pattern. Note the battery used and
    whether the phone got warm. A hardware decoder should barely show.
