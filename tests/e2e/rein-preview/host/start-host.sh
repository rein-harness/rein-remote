#!/bin/bash
# The Rein machine: two X displays with an xterm each (Xvnc = display + VNC server), Rein's web UI on
# 127.0.0.1:9333 only (password mode), and Caddy on :8443 (HTTPS, tls internal) in front of it.
set -u
mkdir -p /p2/logs /work
export HOME=/root
for n in 1 2; do
  Xvnc :$n -geometry 1280x800 -depth 24 -SecurityTypes None -localhost -rfbport $((5900+n)) -AlwaysShared -desktop "p2 display :$n" >/p2/logs/xvnc$n.log 2>&1 &
done
# Display :3 has a VNC password: rein-remote can't stream it without one (Rein gives it none for "+ Preview"),
# so it's the frames path, where the page asks for the password.
printf '%s\n' "${VNC_PW:-p2-vnc-pw}" | vncpasswd -f > /p2/vncpass && chmod 600 /p2/vncpass
Xvnc :3 -geometry 1024x640 -depth 24 -SecurityTypes VncAuth -PasswordFile /p2/vncpass -localhost -rfbport 5903 -AlwaysShared -desktop "p2 display :3" >/p2/logs/xvnc3.log 2>&1 &
for i in $(seq 50); do [ -e /tmp/.X11-unix/X1 ] && [ -e /tmp/.X11-unix/X2 ] && [ -e /tmp/.X11-unix/X3 ] && break; sleep 0.1; done
DISPLAY=:3 xterm -geometry 60x16+0+0 -fa 'DejaVu Sans Mono' -fs 22 -bg '#3a1010' -fg '#ffffff' -T 'p2 xterm on :3' -e bash >/p2/logs/xterm3.log 2>&1 &
# The native app: an xterm filling the display, a big font so it reads on a phone.
DISPLAY=:1 xterm -geometry 80x22+0+0 -fa 'DejaVu Sans Mono' -fs 22 -bg '#101418' -fg '#e6e6e6' -T 'p2 xterm on :1' -e bash >/p2/logs/xterm1.log 2>&1 &
DISPLAY=:2 xterm -geometry 80x22+0+0 -fa 'DejaVu Sans Mono' -fs 22 -T 'p2 xterm on :2' -e bash >/p2/logs/xterm2.log 2>&1 &
# Password mode (the mode for a server reached from other machines), set up ahead of time.
node /p2/webui-config.mjs "${REIN_USER:-p2}" "${REIN_PASS:-phase-two-proof}"
rein --ui --host 127.0.0.1 --port 9333 >/p2/logs/rein.log 2>&1 &
for i in $(seq 100); do curl -sf http://127.0.0.1:9333/api/state >/dev/null && break; sleep 0.1; done
caddy run --config /p2/Caddyfile --adapter caddyfile >/p2/logs/caddy.log 2>&1 &
echo "host up" > /p2/logs/ready
wait
