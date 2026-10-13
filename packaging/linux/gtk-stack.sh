#!/bin/sh
# GTK 4.14 and libadwaita 1.5 (what rr-host-gtk needs: its `v4_14` and `v1_5` features), built from
# source into PREFIX on Ubuntu 22.04, for the AppImage: an AppImage runs on the glibc it was built
# against and newer, so it's built on the oldest system it supports (22.04: glibc 2.35, GTK 4.6,
# libadwaita 1.1), with what 22.04 lacks built here. Everything older that 22.04 has (harfbuzz,
# fontconfig, freetype, gdk-pixbuf, graphene, epoxy, X11, Mesa) is the system's.
#
#   sudo packaging/linux/gtk-stack.sh deps     # the build's apt packages
#   PREFIX=/opt/rr-gtk packaging/linux/gtk-stack.sh
#
# Pinned tarballs, each checked against its sha256. CI caches PREFIX (keyed on this file's hash).
set -eu
PREFIX=${PREFIX:-/opt/rr-gtk}
SRC=${SRC:-${TMPDIR:-/tmp}/rr-gtk-src}
JOBS=${JOBS:-$(nproc)}

if [ "${1:-}" = deps ]; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  # Not libappstream-dev: 22.04's is 0.15, and libadwaita 1.5 needs AppStream 1.0 (built below).
  apt-get install -y -qq --no-install-recommends build-essential pkg-config python3-pip ca-certificates curl xz-utils \
    gettext libffi-dev zlib1g-dev libmount-dev libpcre2-dev libselinux1-dev \
    libpixman-1-dev libfreetype-dev libfontconfig-dev libpng-dev libx11-dev libxext-dev libxrender-dev \
    libxcb1-dev libxcb-render0-dev libxcb-shm0-dev libharfbuzz-dev libfribidi-dev libxft-dev libthai-dev \
    libexpat1-dev libxml2-dev libepoxy-dev libgraphene-1.0-dev libgdk-pixbuf-2.0-dev libxkbcommon-dev \
    libxrandr-dev libxi-dev libxinerama-dev libxcursor-dev libxdamage-dev libxfixes-dev libjpeg-dev libtiff-dev \
    libegl-dev libgles-dev libdrm-dev sassc gperf libyaml-dev libcurl4-openssl-dev libzstd-dev liblzma-dev librsvg2-common itstool xsltproc docbook-xsl > /dev/null
  # GTK 4.14 needs meson >= 0.63, glib 2.80 >= 1.2 (22.04 has 0.61).
  pip3 install -q 'meson==1.5.2' 'ninja==1.11.1.1' packaging
  exit 0
fi

mkdir -p "$SRC" "$PREFIX"
export PATH="$PREFIX/bin:$PATH"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:$PREFIX/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# build NAME URL SHA256 [meson options...]
build() {
  name=$1 url=$2 sum=$3; shift 3
  if [ -f "$PREFIX/.built-$name" ]; then echo "== $name: already in $PREFIX"; return; fi
  echo "== $name"
  tarball="$SRC/$(basename "$url")"
  [ -f "$tarball" ] || curl -fsSL --retry 3 -o "$tarball" "$url"
  echo "$sum  $tarball" | sha256sum -c --quiet -
  rm -rf "$SRC/$name" "$SRC/$name.x" && mkdir "$SRC/$name.x"
  tar xJf "$tarball" -C "$SRC/$name.x"     # one folder (some tarballs name it ./NAME-VERSION)
  mv "$SRC/$name.x"/*/ "$SRC/$name" && rmdir "$SRC/$name.x"
  meson setup "$SRC/$name/_build" "$SRC/$name" --prefix="$PREFIX" --libdir=lib --buildtype=release \
    --wrap-mode=nodownload "$@" > "$SRC/$name.setup.log" || { tail -60 "$SRC/$name.setup.log"; exit 1; }
  ninja -C "$SRC/$name/_build" -j "$JOBS" > "$SRC/$name.build.log" 2>&1 || { tail -60 "$SRC/$name.build.log"; exit 1; }
  ninja -C "$SRC/$name/_build" install > /dev/null
  rm -rf "$SRC/$name"
  touch "$PREFIX/.built-$name"
}

build glib https://download.gnome.org/sources/glib/2.80/glib-2.80.5.tar.xz \
  9f23a9de803c695bbfde7e37d6626b18b9a83869689dd79019bf3ae66c3e6771 \
  -Dintrospection=disabled -Dtests=false -Dman-pages=disabled -Ddocumentation=false -Dsysprof=disabled -Dnls=disabled
build cairo https://cairographics.org/releases/cairo-1.18.2.tar.xz \
  a62b9bb42425e844cc3d6ddde043ff39dbabedd1542eba57a2eb79f85889d45a \
  -Dtests=disabled -Dgtk_doc=false -Dspectre=disabled
build pango https://download.gnome.org/sources/pango/1.52/pango-1.52.2.tar.xz \
  d0076afe01082814b853deec99f9349ece5f2ce83908b8e58ff736b41f78a96b \
  -Dintrospection=disabled -Dgtk_doc=false -Dsysprof=disabled
build wayland https://gitlab.freedesktop.org/wayland/wayland/-/releases/1.23.1/downloads/wayland-1.23.1.tar.xz \
  864fb2a8399e2d0ec39d56e9d9b753c093775beadc6022ce81f441929a81e5ed \
  -Ddocumentation=false -Dtests=false
build wayland-protocols https://gitlab.freedesktop.org/wayland/wayland-protocols/-/releases/1.38/downloads/wayland-protocols-1.38.tar.xz \
  ff17292c05159d2b20ce6cacfe42d7e31a28198fa1429a769b03af7c38581dbe \
  -Dtests=false
build gtk https://download.gnome.org/sources/gtk/4.14/gtk-4.14.5.tar.xz \
  5547f2b9f006b133993e070b87c17804e051efda3913feaca1108fa2be41e24d \
  -Dx11-backend=true -Dwayland-backend=true -Dbroadway-backend=false -Dmedia-gstreamer=disabled \
  -Dprint-cups=disabled -Dprint-cpdb=disabled -Dvulkan=disabled -Dcloudproviders=disabled -Dsysprof=disabled \
  -Dtracker=disabled -Dcolord=disabled -Dintrospection=disabled -Ddocumentation=false -Dman-pages=false \
  -Dbuild-demos=false -Dbuild-testsuite=false -Dbuild-examples=false -Dbuild-tests=false
build libxmlb https://github.com/hughsie/libxmlb/releases/download/0.3.21/libxmlb-0.3.21.tar.xz \
  642343c9b3eca5c234ef83d3d5f6307c78d024f2545f3cc2fa252c9e14e4efd1 \
  -Dgtkdoc=false -Dintrospection=false -Dtests=false -Dcli=false
build appstream https://www.freedesktop.org/software/appstream/releases/AppStream-1.0.3.tar.xz \
  5ab6f6cf644e7875a9508593962e56bb430f4e59ae0bf03be6be7029deb6baa4 \
  -Dstemming=false -Dsystemd=false -Dvapi=false -Dgir=false -Dsvg-support=false -Dzstd-support=true \
  -Ddocs=false -Dapidocs=false -Dinstall-docs=false -Dcompose=false
build libadwaita https://download.gnome.org/sources/libadwaita/1.5/libadwaita-1.5.6.tar.xz \
  a78d398db1d8a50c5f08c15fd3b813251e14622c0c7ae5fdfe177e2c913e1240 \
  -Dintrospection=disabled -Dvapi=false -Dgtk_doc=false -Dtests=false -Dexamples=false
pkg-config --modversion glib-2.0 gtk4 libadwaita-1
echo "GTK stack in $PREFIX"
