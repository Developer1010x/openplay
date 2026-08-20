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
           "$root/usr/share/polkit-1/rules.d" \
           "$root/usr/share/doc/openplay"

install -m 0755 target/release/openplay-sender   "$root/usr/bin/"
install -m 0755 target/release/openplay-receiver "$root/usr/bin/"

install -m 0644 data/org.openplay.OpenPlay.desktop     "$root/usr/share/applications/"
install -m 0644 data/org.openplay.OpenPlay.metainfo.xml "$root/usr/share/metainfo/"
install -m 0644 data/icons/hicolor/scalable/apps/org.openplay.OpenPlay.svg \
                "$root/usr/share/icons/hicolor/scalable/apps/"

# Wi-Fi Direct needs to talk to wpa_supplicant over the system bus; without
# these two the Miracast P2P path fails with a bare D-Bus access denial.
install -m 0644 data/dbus/org.openplay.wpa.conf     "$root/usr/share/dbus-1/system.d/"
install -m 0644 data/polkit/10-openplay-wpa.rules   "$root/usr/share/polkit-1/rules.d/"

install -m 0644 LICENSE "$root/usr/share/doc/openplay/copyright" 2>/dev/null || true

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

# Plugins are dlopen'd, so shlibdeps cannot find them. gstreamer1.0-pipewire
# supplies pipewiresrc (Linux capture) and -ugly supplies x264enc, the universal
# software encoder fallback; both are hard requirements, not niceties.
# See docs/install.md, which this list must stay in step with.
runtime_deps="gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-pipewire, pipewire, xdg-desktop-portal"

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
 including Wi-Fi Direct on Linux; AirPlay sending is partly working and the
 WebRTC path is not yet wired to the binaries.
EOF

dpkg-deb --build --root-owner-group "$root" "$out_dir/$pkg.deb"

echo "built $out_dir/$pkg.deb"
dpkg-deb --info "$out_dir/$pkg.deb"
