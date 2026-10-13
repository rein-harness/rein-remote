# The installers on real machines

Rein Remote ships the host app and `rein-remote` together, one installer per platform
(`packaging/`):

| Platform | File | Where it installs | At sign-in |
|---|---|---|---|
| Windows 10 1809+ / 11, x64 | `rein-remote-VERSION-x64.msi` (WiX v5) | per user, no administrator: `%LOCALAPPDATA%\Programs\Rein Remote`; for every user (elevated, `ALLUSERS=1`): `C:\Program Files\Rein Remote` | the app, in the tray (`ReinRemoteHost.exe --background`), from a Run key the installer adds (`STARTATLOGIN=0` leaves it out) |
| Ubuntu 24.04+, Debian 13 | `rein-remote_VERSION_amd64.deb` | `/usr/bin/rein-remote`, `/usr/bin/rein-remote-host-gtk`, the menu entry and icon | off until the user turns it on in the app (Settings → Start at login) |
| Ubuntu 22.04+ and other distributions as new | `rein-remote-VERSION-x86_64.AppImage` | wherever the user puts it | off until turned on in the app (the autostart entry points at the AppImage) |
| macOS 14+ | `rein-remote-VERSION-macos-universal.dmg` | `/Applications/Rein Remote.app`, `rein-remote` inside it (`Contents/MacOS`) | off until turned on in the app (Settings → "Open Rein Remote at login": `SMAppService.mainApp`) |

Two different things start at login, and the apps keep them apart:

- **The app** (its tray or menu-bar icon), which starts `rein-remote host` as its child when no host
  is running. This is what the Windows installer turns on, and what "Show Rein Remote in the tray at
  login" (Linux, `~/.config/autostart/dev.reinharness.RemoteHost.desktop`) and "Open Rein Remote at login"
  (macOS) do.
- **The host alone** (`PUT /control/start-at-login`, [control-api.md](../control-api.md#start-at-login)):
  a scheduled task (Windows), a systemd user unit (Linux) or a LaunchAgent (macOS) running
  `rein-remote host` without the app. The Windows app's "Start at login" toggle is this one. With
  both on, whichever starts second finds the first (a second host refuses to start; the app attaches
  to the running one).

## What CI checks on every pull request

**Windows** (`ci.yml`, `windows-installer`, after `windows-host` built the app): the app's
executables signed with a certificate made on the runner (`tools/sign/windows.ps1 -SelfSigned`), the
.msi built with WiX v5 and signed the same way, then `packaging/windows/smoke.ps1` twice:

- per user (`msiexec /i … /qn`, no `ALLUSERS`): every file in `%LOCALAPPDATA%\Programs\Rein Remote`
  (`ReinRemoteHost.exe`, `rein-remote.exe`, the Windows App SDK), the Start menu shortcut and its
  target, the Run key's command line, one Add/Remove Programs entry, the installed `rein-remote.exe`
  running `host --test-pattern 640x480` and sharing through its control API, and the installed app
  started as the Run key starts it (`--background`), finding that host and logging the
  `rein-remote.exe` beside it. Then `msiexec /x … /qn` **with both still running**: both stopped, the
  folder, shortcut, Run key, registry and Add/Remove Programs entry gone, no reboot asked for.
- for all users (`ALLUSERS=1 STARTATLOGIN=0`): the same in `C:\Program Files\Rein Remote` and the
  all-users Start menu, and no Run key.

**Linux** (`installers.yml`):

- `deb` builds the .deb on Ubuntu 24.04; `deb-test` installs it in a **clean `ubuntu:24.04`
  container** with `apt install ./rein-remote_….deb` (so every dependency comes from its `Depends`),
  runs `rein-remote --version`, `desktop-file-validate` on the menu entry, then
  `packaging/linux/smoke-app.sh`: the installed app on Xvfb with a D-Bus session and no host running
  starts `/usr/bin/rein-remote host` as its child, the host's API answers, the app holds a
  connection to it (its event stream, seen with `ss`), and sharing the test pattern starts. Then
  `apt remove`: every file gone.
- `appimage` builds GTK 4.14 and libadwaita 1.5 from source on Ubuntu 22.04
  (`packaging/linux/gtk-stack.sh`, cached), the app against them, and the AppImage with linuxdeploy,
  then checks that nothing in it needs a glibc newer than 22.04's 2.35. `appimage-test` runs the same
  `smoke-app.sh` with `--appimage-extract-and-run` on **ubuntu-22.04 and ubuntu-24.04** runners that
  have no GTK 4.14 or libadwaita of their own.

**macOS** (`installers.yml`, `dmg`, once `clients/macos-host` is in the tree): the app built
(xcodegen, xcodebuild), `rein-remote` inside it, signed ad hoc with the hardened runtime
(`tools/sign/macos.sh`: `rein-remote` as `com.rein-harness.rein-remote`, then the bundle), the
.dmg made with `hdiutil` and signed, then `packaging/macos/smoke.sh`: mounted, the `Applications`
link checked, the app copied out, `codesign --verify --strict --deep`, the app's `rein-remote host`
sharing a test pattern through its API, the app itself started with no host running and starting the
`rein-remote` inside it as its child, detached.

**What CI can't check**, and this test is for: a real desktop session (the tray and menu bar, a
reboot and the sign-in that follows), SmartScreen and Gatekeeper with a file downloaded from the
web, a real certificate, a clean PC without anything a CI runner has preinstalled (the Visual C++
runtime, .NET), the firewall prompt, Wayland (CI runs X11), and a phone pairing with the installed
app.

## Get the installers

- **A release:** the assets of a `v*` release (`release.yml` adds them, signed when the repository
  has the certificates: [signing.md](signing.md)).
- **A pull request:** its checks' artifacts: `windows-installer` (ci.yml), `linux-deb`,
  `linux-appimage`, `macos-dmg` (installers.yml). These are signed with throwaway certificates
  (Windows) or ad hoc (macOS), i.e. **unsigned** as far as another machine is concerned.
- **From source:** `packaging/windows/build.ps1`, `packaging/linux/build-deb.sh`,
  `packaging/linux/build-appimage.sh`, `packaging/macos/build-dmg.sh` (each says what it needs at
  its top).

Download them **with a browser** (Edge, Safari, Firefox): that's what marks the file as from the
internet (Windows' Mark of the Web, macOS's quarantine attribute), which is what SmartScreen and
Gatekeeper act on. A file copied with `scp` or from a USB stick isn't checked the same way.

## Windows (a real PC, Windows 10 or 11)

Best on a PC (or a fresh VM) that has never had .NET or the Visual C++ runtime installed.

### Install, unsigned (a pull request's artifact)

1. Download `rein-remote-…-x64.msi` with Edge and double-click it.
2. **Expect SmartScreen:** "Windows protected your PC … Unknown publisher". *More info* → *Run
   anyway*. (Edge may first say the file "isn't commonly downloaded": *Keep* → *Keep anyway*.)
   The Windows Installer window then says *Publisher: Unknown* (the PR's self-signed certificate
   isn't trusted on your PC). Note what each dialog says.
3. **No UAC prompt** should appear (a per-user install). If one does, note it: it's a bug.
4. It installs with a progress bar and closes. Check:
   - `%LOCALAPPDATA%\Programs\Rein Remote` has `ReinRemoteHost.exe` and `rein-remote.exe`.
   - Start → *Rein Remote* is there, with the app's icon.
   - Settings → Apps → Installed apps lists *Rein Remote*, its version, publisher *Rein Harness*.
   - `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v "Rein Remote"` shows
     `"C:\Users\…\AppData\Local\Programs\Rein Remote\ReinRemoteHost.exe" --background`.

### First launch

5. Start → *Rein Remote*. The window opens and the icon is in the notification area. The window
   says it's connected to the host; `%APPDATA%\rein-remote\app.log` has
   `host binary C:\Users\…\Programs\Rein Remote\rein-remote.exe`.
6. **Expect the Windows Defender Firewall prompt** for `rein-remote.exe` when sharing starts (it
   listens for viewers). Allow it on private networks. Note the name and publisher it shows.
7. Start sharing; open the shown link on a phone on the same network; the phone shows this screen.

### Start at login, across a reboot

8. Quit the app (tray icon → *Quit*). Restart the PC and sign in.
9. **Expect:** no window, the Rein Remote icon in the notification area (it may be in the overflow
   ^ menu) within a few seconds of the desktop appearing; Task Manager → Startup apps lists *Rein
   Remote* (enabled, publisher shown or blank when unsigned). Clicking the icon opens the window,
   connected. Sharing is as it was left (on if it was on).
10. Settings → *Start at login* (the host's scheduled task): turn it on, reboot, sign in: there is
    still one host (`tasklist | findstr rein-remote` shows one `rein-remote.exe` once the app has
    found it). Leave it on for the uninstall below.
11. Task Manager → Startup apps → *Rein Remote* → Disable, reboot: the app doesn't start. Enable it
    again.

### Uninstall

12. With the app running (tray icon visible) and sharing on: Settings → Apps → *Rein Remote* →
    Uninstall (or `msiexec /x rein-remote-…-x64.msi`).
13. **Expect:** no "files in use" or "restart required" message; the tray icon disappears; the
    folder `%LOCALAPPDATA%\Programs\Rein Remote`, the Start menu entry, the Run key and the
    *Installed apps* entry are gone; `schtasks /Query /TN rein-remote-host` says it doesn't exist
    (the uninstaller removed the host's task). `%APPDATA%\rein-remote` (settings, pairings, the audit
    log) is kept, on purpose.
14. Reboot: nothing of Rein Remote starts.

### For every user, and without the login item

15. From an **elevated** prompt (Terminal → *Run as administrator*; the .msi says it needs no
    elevation, so msiexec doesn't ask on its own and an unelevated `ALLUSERS=1` fails with error
    1925 or similar: note what you see): `msiexec /i rein-remote-…-x64.msi ALLUSERS=1 STARTATLOGIN=0`.
    Expect `C:\Program Files\Rein Remote`, the shortcut in every user's Start menu, no Run key.
    Sign in as another user: Start → Rein Remote works for them too. Uninstall from Installed apps
    (it asks for an administrator), or `msiexec /x` from the elevated prompt.
16. An upgrade: install this version, then a newer one (a later PR's artifact or a release) over it
    with the app running: one *Rein Remote* in Installed apps afterwards, the newer version, the app
    restarted by its Run key at the next sign-in.

### Signed (a release with `WINDOWS_CERTIFICATE`)

17. Repeat 1–4 with the release's .msi. **Expect:** the file's Properties → Digital Signatures shows
    the publisher, *This digital signature is OK*, a timestamp; the Windows Installer window says
    *Publisher: <the certificate's name>*. SmartScreen: with an **EV** certificate, no warning; with an
    **OV** certificate, the warning names the publisher until the certificate has built reputation
    (Microsoft doesn't say how many downloads that takes); note which you saw.
18. `Get-AuthenticodeSignature "$env:LOCALAPPDATA\Programs\Rein Remote\ReinRemoteHost.exe"` and
    `…\rein-remote.exe`: `Valid`, the same signer. (The other DLLs are Microsoft's, signed by
    Microsoft.)

## Ubuntu (a real desktop: 24.04 for the .deb, 22.04 and 24.04 for the AppImage)

### The .deb

1. Download `rein-remote_…_amd64.deb`. `sudo apt install ./rein-remote_…_amd64.deb` (or open it with
   App Center). **Expect:** apt lists the packages it adds (none on a GNOME desktop that already has
   GTK 4 and libadwaita) and no errors. On 22.04 it must **refuse** (`libgtk-4-1 (>= 4.14)` can't be
   satisfied): that's what the AppImage is for.
2. **First launch:** Activities → *Rein Remote* (the icon is the app's). The window opens and
   connects; the tray icon shows on desktops with a StatusNotifier host (KDE, Ubuntu's GNOME with
   its AppIndicator extension; plain GNOME needs the *AppIndicator and KStatusNotifierItem Support*
   extension). `which rein-remote` → `/usr/bin/rein-remote`; the app's Settings show it started
   that one (`ps -o args= -C rein-remote`).
3. **Start at login, across a reboot:** Settings → *Show Rein Remote in the tray at login* on.
   `~/.config/autostart/dev.reinharness.RemoteHost.desktop` now has
   `Exec="/usr/bin/rein-remote-host-gtk" --hidden`. Reboot, sign in: the tray icon, no window, the
   host running. Turn it off: the file is gone, a reboot starts nothing. Then *Start the host at
   login* (the systemd user unit): `systemctl --user status rein-remote-host` after a reboot shows it
   running `/usr/bin/rein-remote host`; open the app: it finds that host. Turn it off again.
4. **Uninstall:** `sudo apt remove rein-remote`. **Expect:** the menu entry gone (Activities no
   longer finds it), `/usr/bin/rein-remote*` gone. What the user turned on stays theirs:
   `~/.config/autostart/…desktop` and `~/.config/systemd/user/rein-remote-host.service` are not
   removed (apt can't reach every user's home): turn them off in the app before removing, or delete
   them; `~/.rein-remote` is kept.
5. **Signature:** the .deb isn't signed itself; check it against the release's `SHA256SUMS` and that
   file against its cosign signature ([signing.md](signing.md)). No warning is shown either way.

### The AppImage (22.04 and 24.04)

6. Download `rein-remote-…-x86_64.AppImage`, `chmod +x` it (or Properties → *Executable as
   Program*), run it. On 22.04 and 24.04 it needs **FUSE 2**: `sudo apt install libfuse2` (22.04) or
   `libfuse2t64` (24.04); without it the AppImage says so; `--appimage-extract-and-run` works
   without. On 24.04 note whether AppArmor blocks it (unprivileged user namespaces are only needed by
   Electron/Chromium AppImages; this one shouldn't hit it).
7. **First launch:** the window opens, connects to the host it starts (the `rein-remote` inside the
   AppImage: `ps -o args= -C rein-remote` shows a `/tmp/.mount_…/usr/bin/rein-remote host`). Both
   light and dark system styles look right (Settings → Appearance). On a Wayland session it runs as
   a Wayland client (`GDK_BACKEND` isn't forced): check the window, the tray icon and copying a link.
8. **Start at login:** *Show Rein Remote in the tray at login* writes
   `Exec="/path/to/rein-remote-…-x86_64.AppImage" --hidden` (the AppImage's own path, not the
   `/tmp/.mount_…` it runs from). Reboot, sign in: the tray icon. *Start the host at login* says it
   can't from an AppImage (its files are gone when it quits) and leaves the switch off: that's
   expected. Moving the AppImage after turning this on breaks it (the entry points at the old
   path): turn it off and on again from the new place.
9. **Uninstall:** turn the login item off, delete the file. Nothing else was installed.

## macOS (a real Mac, macOS 14 or later)

### Install, unsigned (a pull request's artifact: ad hoc)

1. Download `rein-remote-…-macos-….dmg` with Safari and open it. The window shows *Rein Remote* and
   *Applications*: drag the app onto *Applications*. Eject.
2. **Expect Gatekeeper** on the first open: macOS 15 says *"Rein Remote" Not Opened … Apple could
   not verify … is free of malware* with *Done* / *Move to Trash* only. Open System Settings →
   Privacy & Security → *"Rein Remote" was blocked …* → *Open Anyway*, authenticate, then *Open*.
   (macOS 14: Control-click → Open → *Open*.) Note the exact wording.
3. **First launch:** the menu-bar icon (no Dock icon). It starts its `rein-remote` and connects.
   Start sharing a display: **macOS asks for Screen Recording for "Rein Remote"** (the app, the
   responsible process, not `rein-remote`); grant it, quit and reopen when asked. Accessibility the
   same for control. A viewer on a phone sees and controls the screen.

### Start at login, across a reboot

4. The app's Settings → *Open Rein Remote at login* (`SMAppService.mainApp`). System Settings → General → Login
   Items lists *Rein Remote* under *Open at Login*. Restart and sign in: the menu-bar icon appears,
   connected, sharing as it was left, **without** asking for Screen Recording again. Turn it off:
   gone from Login Items; a restart starts nothing.
5. (The host alone at login, `PUT /control/start-at-login`, is the LaunchAgent
   `~/Library/LaunchAgents/com.rein-harness.rein-remote-host.plist`, running the app's
   `Contents/MacOS/rein-remote`. The macOS app uses its login item instead.)

### Uninstall

6. Quit from the menu-bar icon, drag *Rein Remote* from Applications to the Trash. Login Items no
   longer lists it (macOS drops a login item whose app is gone, after a restart at the latest).
   `~/.rein-remote` is kept; the Screen Recording entry stays in Privacy & Security until removed
   with `-`.

### Signed and notarized (a release with the Developer ID secrets)

7. Repeat 1–3 with the release's .dmg. **Expect:** opening the .dmg and the app shows only *"Rein
   Remote" is an app downloaded from the Internet. Are you sure you want to open it?* with *Open*,
   naming Safari and the date; no *Open Anyway* detour.
8. Check, with the Wi-Fi **off** (the tickets are stapled, so no network is needed):

   ```sh
   xcrun stapler validate ~/Downloads/rein-remote-*.dmg       # The validate action worked!
   xcrun stapler validate "/Applications/Rein Remote.app"      # The validate action worked!
   spctl --assess -vvv --type execute "/Applications/Rein Remote.app"
   # accepted, source=Notarized Developer ID, origin=Developer ID Application: … (TEAMID)
   codesign -dvvv "/Applications/Rein Remote.app/Contents/MacOS/rein-remote" 2>&1 | grep -E 'Identifier|Authority|flags'
   # Identifier=com.rein-harness.rein-remote, Authority=Developer ID Application: …, flags=0x10000(runtime)
   ```

9. Update: install a newer signed release over it (replace in Applications). Screen Recording and
   Accessibility are **not** asked again (same identifier, same team). With ad-hoc builds they are,
   every time: that's expected and one reason releases need the Developer ID.

## Record

For each platform, note: the OS version, the file and where it came from (PR run or release), each
dialog's exact text (screenshots welcome), whether the login item came back after the reboot, and
anything left behind after uninstalling.
