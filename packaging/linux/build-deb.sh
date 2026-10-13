#!/bin/sh
# Rein Remote's .deb: rein-remote and the host app (rein-remote-host-gtk) side by side in /usr/bin,
# the menu entry and icon. Built on Ubuntu 24.04 (its GTK 4.14 and libadwaita 1.5 are what the app
# needs), so it installs on Ubuntu 24.04 and later and Debian 13; Ubuntu 22.04 has the AppImage.
#
#   cargo build --release -p rr-server -p rr-host-gtk --features rr-host-gtk/gtk
#   packaging/linux/build-deb.sh [OUT_DIR]          # rein-remote_VERSION_ARCH.deb
#
# No /etc/xdg/autostart entry and no maintainer scripts: the app's own Settings turn on "Show Rein
# Remote in the tray at login" (~/.config/autostart, per user) and "Start the host at login" (a
# systemd user unit), so nothing starts for every user unasked. The desktop and icon caches are
# refreshed by dpkg's triggers (desktop-file-utils, hicolor-icon-theme).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/deb}
target=${TARGET_DIR:-$root/target/release}
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)}
arch=$(dpkg --print-architecture)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg=$work/pkg
data=$root/crates/rr-host-gtk/data

install -Dm755 "$target/rein-remote" "$pkg/usr/bin/rein-remote"
install -Dm755 "$target/rein-remote-host-gtk" "$pkg/usr/bin/rein-remote-host-gtk"
strip --strip-unneeded "$pkg/usr/bin/rein-remote" "$pkg/usr/bin/rein-remote-host-gtk"
install -Dm644 "$data/dev.reinharness.RemoteHost.desktop" "$pkg/usr/share/applications/dev.reinharness.RemoteHost.desktop"
install -Dm644 "$data/icons/hicolor/scalable/apps/dev.reinharness.RemoteHost.svg" \
  "$pkg/usr/share/icons/hicolor/scalable/apps/dev.reinharness.RemoteHost.svg"
install -d "$pkg/usr/share/doc/rein-remote"
{
  echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
  echo "Upstream-Name: rein-remote"
  echo "Source: https://github.com/rein-harness/rein-remote"
  echo
  echo "Files: *"
  echo "License: MIT"
  sed 's/^$/./; s/^/ /' "$root/LICENSE"
} > "$pkg/usr/share/doc/rein-remote/copyright"

# The libraries the binaries link (dpkg-shlibdeps), and GTK and libadwaita at the versions the app's
# features need (it would start and fail on older ones).
mkdir -p "$work/debian"
echo "Source: rein-remote" > "$work/debian/control"
shlibs=$(cd "$work" && dpkg-shlibdeps -O "$pkg/usr/bin/rein-remote" "$pkg/usr/bin/rein-remote-host-gtk" 2>/dev/null |
  sed -n 's/^shlibs:Depends=//p' | tr ',' '\n' | sed 's/^ *//' | grep -Ev '^(libgtk-4-1|libadwaita-1-0) ' | paste -sd, - | sed 's/,/, /g')
depends="$shlibs, libgtk-4-1 (>= 4.14), libadwaita-1-0 (>= 1.5)"

mkdir -p "$pkg/DEBIAN"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: rein-remote
Version: $version
Architecture: $arch
Maintainer: Rein Remote <https://github.com/rein-harness/rein-remote/issues>
Installed-Size: $(du -sk "$pkg/usr" | cut -f1)
Depends: $depends
Recommends: libpulse0, libva2, libva-drm2
Suggests: tailscale, ffmpeg
Section: net
Priority: optional
Homepage: https://github.com/rein-harness/rein-remote
Description: Share this computer's screen with a phone, a browser or another computer
 Rein Remote: rein-remote (the server: serve, host, relay) and the host app, a GTK window and tray
 icon to start and stop sharing, pair devices, see who's connected and change the settings.
 .
 PulseAudio's client library (libpulse0) is for sharing sound; libva for hardware video encoding.
CONTROL
mkdir -p "$out"
deb="$out/rein-remote_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build -Zxz "$pkg" "$deb"
dpkg-deb --info "$deb"
dpkg-deb --contents "$deb"
