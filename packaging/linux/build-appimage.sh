#!/bin/sh
# Rein Remote's AppImage: the host app and rein-remote in one file that runs on Ubuntu 22.04 and later
# (and other distributions as new). Built on Ubuntu 22.04 against the GTK 4.14 / libadwaita 1.5 that
# packaging/linux/gtk-stack.sh builds into GTK_PREFIX (22.04's own are too old for the app), with
# linuxdeploy bundling every library not in the AppImage exclude list (glibc, X11, Mesa, fontconfig
# and freetype come from the system).
#
#   PKG_CONFIG_PATH=$GTK_PREFIX/lib/pkgconfig cargo build --release -p rr-server -p rr-host-gtk --features rr-host-gtk/gtk
#   GTK_PREFIX=/opt/rr-gtk packaging/linux/build-appimage.sh [OUT_DIR]   # rein-remote-VERSION-ARCH.AppImage
#
# Needs network (linuxdeploy, pinned below, fetches the AppImage runtime). Run it with
# --appimage-extract-and-run where there's no FUSE (containers, CI).
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/appimage}
mkdir -p "$out" && out=$(cd "$out" && pwd)
target=${TARGET_DIR:-$root/target/release}
GTK_PREFIX=${GTK_PREFIX:-/opt/rr-gtk}
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)}
arch=$(uname -m)
tools=${TOOLS_DIR:-$root/target/appimage-tools}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app=$work/AppDir
data=$root/crates/rr-host-gtk/data
id=dev.reinharness.RemoteHost

# linuxdeploy, a release (not "continuous"), checked.
ld_version=1-alpha-20251107-1
case $arch in
  x86_64) ld_sum=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d ;;
  aarch64) ld_sum=620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff ;;
  *) echo "no linuxdeploy for $arch" >&2; exit 1 ;;
esac
linuxdeploy=$tools/linuxdeploy-$ld_version-$arch.AppImage
if [ ! -x "$linuxdeploy" ]; then
  mkdir -p "$tools"
  curl -fsSL --retry 3 -o "$linuxdeploy.part" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/$ld_version/linuxdeploy-$arch.AppImage"
  echo "$ld_sum  $linuxdeploy.part" | sha256sum -c --quiet - || { sha256sum "$linuxdeploy.part"; exit 1; }
  chmod +x "$linuxdeploy.part" && mv "$linuxdeploy.part" "$linuxdeploy"
fi

export PKG_CONFIG_PATH="$GTK_PREFIX/lib/pkgconfig:$GTK_PREFIX/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$GTK_PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
echo "GTK $(pkg-config --modversion gtk4), libadwaita $(pkg-config --modversion libadwaita-1), glib $(pkg-config --modversion glib-2.0)"

install -Dm755 "$target/rein-remote-host-gtk" "$app/usr/bin/rein-remote-host-gtk"
install -Dm755 "$target/rein-remote" "$app/usr/bin/rein-remote"
strip --strip-unneeded "$app/usr/bin/rein-remote-host-gtk" "$app/usr/bin/rein-remote"
install -Dm644 "$data/$id.desktop" "$app/usr/share/applications/$id.desktop"
install -Dm644 "$data/icons/hicolor/scalable/apps/$id.svg" "$app/usr/share/icons/hicolor/scalable/apps/$id.svg"

# GSettings schemas: GTK's own (it aborts on a missing one), compiled.
mkdir -p "$app/usr/share/glib-2.0/schemas"
cp "$GTK_PREFIX"/share/glib-2.0/schemas/*.xml "$app/usr/share/glib-2.0/schemas/"
glib-compile-schemas "$app/usr/share/glib-2.0/schemas"

# gdk-pixbuf's loaders (SVG: the app's icon), next to the libraries, with a cache naming them bare:
# gdk-pixbuf opens them through libgmodule, whose RUNPATH is $ORIGIN (what linuxdeploy-plugin-gtk does).
pixbuf_dir=$(pkg-config --variable=gdk_pixbuf_moduledir gdk-pixbuf-2.0)
loaders=""
for so in "$pixbuf_dir"/*.so; do
  [ -e "$so" ] || continue
  install -Dm755 "$so" "$app/usr/lib/$(basename "$so")"
  loaders="$loaders --library=$app/usr/lib/$(basename "$so")"
done

# Set before AppRun starts the app.
mkdir -p "$app/apprun-hooks"
cat > "$app/apprun-hooks/rein-remote.sh" <<'HOOK'
export GSETTINGS_SCHEMA_DIR="$APPDIR/usr/share/glib-2.0/schemas"
export GDK_PIXBUF_MODULE_FILE="$APPDIR/usr/lib/gdk-pixbuf-loaders.cache"
export XDG_DATA_DIRS="$APPDIR/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
HOOK

mkdir -p "$out"
output="$out/rein-remote-$version-$arch.AppImage"
rm -f "$output"
# shellcheck disable=SC2086
(cd "$work" && OUTPUT=$output LINUXDEPLOY_OUTPUT_VERSION=$version NO_STRIP=1 \
  "$linuxdeploy" --appimage-extract-and-run --appdir "$app" \
  --executable "$app/usr/bin/rein-remote-host-gtk" --executable "$app/usr/bin/rein-remote" \
  --desktop-file "$app/usr/share/applications/$id.desktop" --icon-file "$app/usr/share/icons/hicolor/scalable/apps/$id.svg" \
  $loaders)
# The loaders' cache, written now that they're deployed, with bare names.
GDK_PIXBUF_MODULEDIR="$app/usr/lib" gdk-pixbuf-query-loaders $(ls "$app"/usr/lib/libpixbufloader*.so 2>/dev/null) |
  sed "s|$app/usr/lib/||g" > "$app/usr/lib/gdk-pixbuf-loaders.cache"
# libwayland-client and -cursor are on the exclude list (the system's is expected), but this GTK was
# built against wayland 1.23 and 22.04 has 1.20: ours, beside the rest (RUNPATH $ORIGIN finds them).
for l in libwayland-client.so.0 libwayland-cursor.so.0; do cp -L "$GTK_PREFIX/lib/$l" "$app/usr/lib/$l"; done
echo "== bundled libraries"; ls "$app/usr/lib"
(cd "$work" && OUTPUT=$output LINUXDEPLOY_OUTPUT_VERSION=$version NO_STRIP=1 \
  "$linuxdeploy" --appimage-extract-and-run --appdir "$app" --output appimage)
ls -la "$output"
