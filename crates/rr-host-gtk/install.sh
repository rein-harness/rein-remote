#!/bin/sh
# Installs the Linux host app for this user (or under PREFIX): the app and rein-remote side by side,
# its .desktop file and icon. Build first:
#
#   cargo build --release -p rr-server -p rr-host-gtk --features rr-host-gtk/gtk
#   crates/rr-host-gtk/install.sh            # ~/.local
#   sudo PREFIX=/usr/local crates/rr-host-gtk/install.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
prefix=${PREFIX:-$HOME/.local}
target=${TARGET_DIR:-$root/target/release}
install -Dm755 "$target/rein-remote-host-gtk" "$prefix/bin/rein-remote-host-gtk"
install -Dm755 "$target/rein-remote" "$prefix/bin/rein-remote"
mkdir -p "$prefix/share/applications"
sed "s|^Exec=rein-remote-host-gtk|Exec=$prefix/bin/rein-remote-host-gtk|" "$here/data/dev.reinharness.RemoteHost.desktop" \
  > "$prefix/share/applications/dev.reinharness.RemoteHost.desktop"
install -Dm644 "$here/data/icons/hicolor/scalable/apps/dev.reinharness.RemoteHost.svg" \
  "$prefix/share/icons/hicolor/scalable/apps/dev.reinharness.RemoteHost.svg"
command -v update-desktop-database > /dev/null && update-desktop-database -q "$prefix/share/applications" || true
command -v gtk4-update-icon-cache > /dev/null && gtk4-update-icon-cache -q -t "$prefix/share/icons/hicolor" || true
echo "installed in $prefix: rein-remote-host-gtk, rein-remote, the menu entry and icon"
