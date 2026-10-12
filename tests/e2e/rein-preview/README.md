# Rein previews through Rein Remote, end to end

Phase 2's check, in Docker: a Rein machine (Ubuntu 24.04: Xvnc displays with an xterm, Rein's web UI
on 127.0.0.1 behind Caddy on HTTPS) and a phone (headless Chromium at 390×844, DPR 3, touch) that can
reach Rein only through the proxy. The phone opens a chat, adds a preview of `:1`, and must see it
stream through Rein Remote on the first open (rein-remote.js and the stream WebSocket proxied by
Rein), type into the xterm (`echo hi > /tmp/p2-proof`, checked inside the Rein machine), and open
`:2` with `/preview :2` the same way; a password-protected `:3` falls back to Rein's frames. It also
checks that the page never receives rein-remote's port, token or a VNC password.

```sh
REIN_SRC=~/projects/rein-harness ./run.sh     # PASS / FAIL per check; screenshots and logs in out/
```

Needs Docker (on a Mac: `colima start`). `REIN_SRC` is a Rein checkout (default `../rein-harness`
next to this repo) with rein-harness#678 (the preview fixes) in it.
