#!/usr/bin/env bash
# Build build/signifiage_<version>_<arch>.deb from a release binary.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --locked

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
[ -n "$VERSION" ] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
ARCH="$(dpkg --print-architecture)"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

install -Dm755 target/release/signifiage "$STAGE/usr/bin/signifiage"
install -Dm644 data/io.github.ta4anx.Signifiage.desktop \
  "$STAGE/usr/share/applications/io.github.ta4anx.Signifiage.desktop"
install -Dm644 data/io.github.ta4anx.Signifiage.svg \
  "$STAGE/usr/share/icons/hicolor/scalable/apps/io.github.ta4anx.Signifiage.svg"
install -Dm644 data/io.github.ta4anx.Signifiage.metainfo.xml \
  "$STAGE/usr/share/metainfo/io.github.ta4anx.Signifiage.metainfo.xml"
install -Dm644 LICENSE "$STAGE/usr/share/doc/signifiage/LICENSE"
install -Dm644 README.md "$STAGE/usr/share/doc/signifiage/README.md"

INSTALLED_SIZE="$(du -sk "$STAGE/usr" | cut -f1)"

mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: signifiage
Version: $VERSION
Section: video
Priority: optional
Architecture: $ARCH
Installed-Size: $INSTALLED_SIZE
Maintainer: Signifiage contributors <signifiage@example.invalid>
Homepage: https://github.com/ta4anx/signifiage
Depends: libgtk-4-1 (>= 4.18), libadwaita-1-0 (>= 1.6), libgdk-pixbuf-2.0-0, libgstreamer1.0-0 (>= 1.22), libgstreamer-plugins-base1.0-0 (>= 1.22), gstreamer1.0-gtk4, gstreamer1.0-plugins-base (>= 1.22), gstreamer1.0-plugins-good (>= 1.22), gstreamer1.0-plugins-bad (>= 1.22), gstreamer1.0-pipewire, gstreamer1.0-libav, desktop-file-utils, gtk-update-icon-cache
Recommends: xdg-desktop-portal, xdg-desktop-portal-gnome, pipewire-pulse
Description: Signage, stage playback and queue console
 Signifiage designs signage and stage scenes, plays playlists of text, image,
 video and capture sources on selected monitors, keeps a called name queue fed
 from spreadsheets or in-app entry, runs a soundboard and teleprompter, and can
 run standalone on a signage PC with a LAN web remote.
CONTROL

cat > "$STAGE/DEBIAN/postinst" <<'SCRIPT'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
fi
SCRIPT

cat > "$STAGE/DEBIAN/postrm" <<'SCRIPT'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
fi
SCRIPT
chmod 0755 "$STAGE/DEBIAN/postinst" "$STAGE/DEBIAN/postrm"

mkdir -p build
DEB="build/signifiage_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz -b "$STAGE" "$DEB" >/dev/null
( cd build && sha256sum "$(basename "$DEB")" > "$(basename "$DEB").sha256" )
echo "built $(pwd)/$DEB"
