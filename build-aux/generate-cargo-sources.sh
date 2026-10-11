#!/bin/sh
# Writes build-aux/cargo-sources.json, which lets flatpak-builder fetch
# every crate before the offline build. Run it whenever Cargo.lock changes.
#
# Needs flatpak-cargo-generator.py from
# https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo
set -eu
cd "$(dirname "$0")"
python3 "${FLATPAK_CARGO_GENERATOR:-flatpak-cargo-generator.py}" ../Cargo.lock -o cargo-sources.json
echo "Wrote build-aux/cargo-sources.json"
