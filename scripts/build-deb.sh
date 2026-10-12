#!/bin/sh
# Builds a Debian package: medianest_<version>_<arch>.deb in dist/.
# Needs: cargo, dpkg-dev, and the -dev packages listed in README.md.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(dpkg --print-architecture)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

DESTDIR="$stage/root" PREFIX=/usr sh scripts/install.sh
# Debian keeps the license as the package's copyright file.
mkdir -p "$stage/root/usr/share/doc/medianest"
mv "$stage/root/usr/share/licenses/medianest/LICENSE" "$stage/root/usr/share/doc/medianest/copyright"
rm -r "$stage/root/usr/share/licenses"

# Library dependencies of the binaries, as dpkg-shlibdeps sees them.
mkdir -p "$stage/debian"
printf 'Source: medianest\n\nPackage: medianest\nArchitecture: any\n' > "$stage/debian/control"
depends=$(cd "$stage" && dpkg-shlibdeps -O root/usr/bin/medianest root/usr/bin/medianest-cli 2>/dev/null \
    | sed -n 's/^shlibs:Depends=//p')

mkdir -p "$stage/root/DEBIAN"
size=$(du -sk "$stage/root" | cut -f1)
cat > "$stage/root/DEBIAN/control" <<CONTROL
Package: medianest
Version: $version
Architecture: $arch
Maintainer: mmrzaf <mohammr.farkhonde12@sharif.edu>
Installed-Size: $size
Depends: $depends
Recommends: ffmpeg, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav, librsvg2-common
Section: graphics
Priority: optional
Homepage: https://github.com/mmrzaf/medianest
Description: photo and video library for GNOME
 MediaNest copies photos and videos from cameras, phones and folders into
 one library, sorted by date, and verifies every copy before anything else
 happens. Albums, tags, search, places, a review inbox, regular integrity
 checks and catalog backups.
CONTROL

mkdir -p dist
out="dist/medianest_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$stage/root" "$out" >/dev/null
echo "Built $out"
