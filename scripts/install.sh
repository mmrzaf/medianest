#!/bin/sh
# Builds MediaNest and installs it.
#
#   scripts/install.sh                     # into /usr/local (may need sudo)
#   PREFIX=$HOME/.local scripts/install.sh # for the current user only
#   DESTDIR=/tmp/root PREFIX=/usr scripts/install.sh   # staging, for packages
set -eu
cd "$(dirname "$0")/.."

PREFIX=${PREFIX:-/usr/local}
DESTDIR=${DESTDIR:-}
TARGET=${CARGO_TARGET_DIR:-target}
APP_ID=io.github.mmrzaf.MediaNest
root="$DESTDIR$PREFIX"

if [ "${SKIP_BUILD:-0}" != 1 ]; then
    cargo build --release --locked -p medianest-app -p medianest-cli
fi

install -Dm755 "$TARGET/release/medianest" "$root/bin/medianest"
install -Dm755 "$TARGET/release/medianest-cli" "$root/bin/medianest-cli"
install -Dm644 "data/$APP_ID.desktop" "$root/share/applications/$APP_ID.desktop"
install -Dm644 "data/$APP_ID.metainfo.xml" "$root/share/metainfo/$APP_ID.metainfo.xml"
install -Dm644 "data/icons/hicolor/scalable/apps/$APP_ID.svg" \
    "$root/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -Dm644 "data/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg" \
    "$root/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
install -Dm644 LICENSE "$root/share/licenses/medianest/LICENSE"

for lang in $(grep -v '^#' po/LINGUAS 2>/dev/null || true); do
    install -d "$root/share/locale/$lang/LC_MESSAGES"
    msgfmt --check -o "$root/share/locale/$lang/LC_MESSAGES/medianest.mo" "po/$lang.po"
done

if [ -z "$DESTDIR" ]; then
    gtk-update-icon-cache -qtf "$root/share/icons/hicolor" 2>/dev/null || true
    update-desktop-database -q "$root/share/applications" 2>/dev/null || true
fi
echo "Installed MediaNest into $root"
