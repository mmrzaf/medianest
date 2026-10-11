#!/bin/sh
# Runs every quality gate: formatting, lints, tests and a release build.
set -eu
cd "$(dirname "$0")/.."

echo "==> rustfmt"
cargo fmt --all -- --check

echo "==> clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> tests"
cargo test --workspace --locked

echo "==> release build"
cargo build --workspace --release --locked

echo "All checks passed."
