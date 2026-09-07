#!/usr/bin/env bash
#
# Build a .deb from an already-compiled release tree.
#
# This is hand-rolled with dpkg-deb rather than cargo-deb so that the runtime
# Depends are computed by dpkg-shlibdeps against the binaries we actually ship,
# instead of being a hand-maintained list that drifts. The GStreamer *plugin*
# packages still have to be listed explicitly: they are dlopen'd at runtime, so
# no ELF NEEDED entry points at them and shlibdeps cannot see them.
#
# Usage: packaging/build-deb.sh [output-dir]
#
# Expects `cargo build --release` to have run first.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:-$repo_root/dist}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"

cd "$repo_root"

# The workspace version is the single source of truth. Parsing it out of the
# [workspace.package] table keeps the .deb from silently shipping a stale one.
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' Cargo.toml | head -1)"
if [ -z "$version" ]; then
  echo "error: could not read version from [workspace.package] in Cargo.toml" >&2
  exit 1
fi

arch="$(dpkg --print-architecture)"
pkg="openplay_${version}_${arch}"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

# mktemp -d gives 0700, and dpkg-deb records the staging root as the package's
# own "./" entry — so without this the .deb ships a 0700 root directory.
chmod 0755 "$root"

for bin in openplay-sender openplay-receiver; do
  if [ ! -x "target/release/$bin" ]; then
    echo "error: target/release/$bin is missing — run 'cargo build --release' first" >&2
    exit 1
  fi
done

install -d "$root/DEBIAN" \
           "$root/usr/bin" \
           "$root/usr/share/applications" \
           "$root/usr/share/metainfo" \
           "$root/usr/share/icons/hicolor/scalable/apps" \
           "$root/usr/share/dbus-1/system.d" \
           "$root/usr/share/doc/openplay"

install -m 0755 target/release/openplay-sender   "$root/usr/bin/"
install -m 0755 target/release/openplay-receiver "$root/usr/bin/"

install -m 0644 data/org.openplay.OpenPlay.desktop     "$root/usr/share/applications/"
install -m 0644 data/org.openplay.OpenPlay.metainfo.xml "$root/usr/share/metainfo/"
install -m 0644 data/icons/hicolor/scalable/apps/org.openplay.OpenPlay.svg \
                "$root/usr/share/icons/hicolor/scalable/apps/"

# Wi-Fi Direct needs to talk to wpa_supplicant over the system bus; without this
# the Miracast P2P path fails with a bare D-Bus access denial. Access is granted
# by group membership (netdev) — see the file for why there is no wider fallback.
#
# No polkit rule is installed alongside it. data/polkit/10-openplay-wpa.rules
# used to be shipped here and was dead weight: it matched
# `action.id == "fi.w1.wpa_supplicant1"`, which is a D-Bus service name, not a
# polkit action id. wpa_supplicant registers no polkit actions at all, so the
# rule could never fire. It has been deleted rather than left to mislead.
install -m 0644 data/dbus/org.openplay.wpa.conf     "$root/usr/share/dbus-1/system.d/"

# Hard-fails like every other install here: a Debian package without a
# copyright file is policy-invalid, so a missing or renamed LICENSE must stop
# the build rather than silently ship an unshippable .deb.
install -m 0644 LICENSE "$root/usr/share/doc/openplay/copyright"

# dpkg-shlibdeps wants a debian/control to exist relative to CWD. Give it a
# throwaway one inside the staging root rather than polluting the repo.
mkdir -p "$root/debian"
cat > "$root/debian/control" <<EOF
Source: openplay
Package: openplay
Architecture: $arch
EOF

shlib_deps="$(
  cd "$root"
  dpkg-shlibdeps -O --ignore-missing-info \
    usr/bin/openplay-sender usr/bin/openplay-receiver 2>/dev/null \
    | sed 's/^shlibs:Depends=//'
)"
rm -rf "$root/debian"

if [ -z "$shlib_deps" ]; then
  echo "error: dpkg-shlibdeps produced no dependencies — refusing to ship a .deb that declares none" >&2
  exit 1
fi

# Everything in the two lists below is dlopen'd at runtime. Nothing here leaves
# an ELF NEEDED entry, so dpkg-shlibdeps cannot see any of it and none of it
# will appear in $shlib_deps. Every entry was confirmed against this tree with
# `dpkg -S` on the .so the loader actually opens — do not add one from memory.
# See docs/install.md, which these lists must stay in step with.

# GStreamer plugin packages. The element factories live in these *packages*; the
# lib*.so that shlibdeps finds is a different thing and does not imply them.
#
#   -base    is listed EXPLICITLY. shlibdeps contributes
#            libgstreamer-plugins-base1.0-0, which is the LIBRARY, while the
#            `appsink` and `videoconvert` factories come from the PACKAGE
#            gstreamer1.0-plugins-base. It was previously satisfied only by
#            accident, because gstreamer1.0-plugins-bad happens to depend on it.
#            Core elements must not ride on an unrelated package's Depends.
#   -nice    supplies the ICE elements webrtcbin needs. Without it webrtcbin
#            still constructs and then refuses every pad request, so the
#            OpenPlay/WebRTC path fails after it looks like it came up.
#   -libav   supplies avdec_h264, the receiver's software decode fallback.
#   -ugly    supplies x264enc, the universal software encoder fallback.
#   -pipewire supplies pipewiresrc, the Linux capture source.
gst_deps="gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-nice, gstreamer1.0-pipewire"

# GUI libraries. eframe/winit/glutin open all of these through libloading, so
# the binaries name them only as strings in .rodata and shlibdeps reports none
# of them. Omitting them is what let the .deb install cleanly and then fail to
# launch on a machine with no other GL/Wayland application already pulling them
# in. To re-derive the list:
#   grep -ao 'lib[A-Za-z0-9_.+-]*\.so\.[0-9]*' target/release/openplay-sender | sort -u
# libxcursor1/libxi6/libxrender1/libxkbcommon-x11-0 are the rest of the set
# winit's X11 backend loads; it fails to initialise if any one is absent.
gui_deps="libegl1, libgl1, libwayland-client0, libwayland-egl1, libxkbcommon0, libxkbcommon-x11-0, libx11-6, libx11-xcb1, libxcb1, libxcursor1, libxi6, libxrender1"

runtime_deps="$gst_deps, $gui_deps, pipewire, xdg-desktop-portal"

cat > "$root/DEBIAN/control" <<EOF
Package: openplay
Version: $version
Section: video
Priority: optional
Architecture: $arch
Depends: $shlib_deps, $runtime_deps
Recommends: wpasupplicant, xdg-desktop-portal-gnome | xdg-desktop-portal-kde | xdg-desktop-portal-wlr
Maintainer: OpenPlay contributors <noreply@github.com>
Homepage: https://github.com/Developer1010x/openplay
Description: Cast your screen to AirPlay, Miracast, or OpenPlay receivers
 OpenPlay mirrors your desktop to nearby receivers. Miracast sending works,
 including Wi-Fi Direct on Linux; the OpenPlay WebRTC path is wired to both
 binaries, and AirPlay sending is partly working.
EOF

dpkg-deb --build --root-owner-group "$root" "$out_dir/$pkg.deb"

echo "built $out_dir/$pkg.deb"
dpkg-deb --info "$out_dir/$pkg.deb"
