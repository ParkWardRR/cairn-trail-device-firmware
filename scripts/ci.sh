#!/usr/bin/env bash
# Local CI: everything a pull request has to pass, run on this machine.
# There is no hosted CI for this repository; the owner chose local CI/CD.
#
#   scripts/ci.sh            # check, test, build both flavours, stage dist/
#
# Writes dist/: the verbose and release ELF files, merged flashable images
# (bootloader + partition table + app, flash at 0x0), and SHA256SUMS.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)

step() { printf '\n== %s\n' "$*"; }

if [ -f "$HOME/export-esp.sh" ]; then
  # shellcheck disable=SC1091
  . "$HOME/export-esp.sh"
fi

step "trail-core: format"
(cd crates/trail-core && cargo +stable fmt --check)

step "trail-core: clippy"
(cd crates/trail-core && cargo +stable clippy --all-targets -- -D warnings)

step "trail-core: host tests"
(cd crates/trail-core && cargo +stable test)

step "trail (host tool): format, clippy, tests, build"
(cd tools/trail && cargo +stable fmt --check)
(cd tools/trail && cargo +stable clippy --all-targets -- -D warnings)
(cd tools/trail && cargo +stable test)
(cd tools/trail && cargo +stable build --release)

# The firmware builds cairn-format from the dongle repository at a pinned
# commit. Its full conformance run -- every format/v3 vector, with the emulator
# that commit carries -- is checked here for exactly that commit; the board
# checks three of them itself (console: vectors).
step "cairn-format: every format/v3 vector at the pinned commit"
REV=$(sed -n 's/^cairn-format = .*rev = "\([0-9a-f]*\)".*/\1/p' firmware/Cargo.toml)
[ -n "$REV" ] || { echo "no cairn-format rev in firmware/Cargo.toml"; exit 1; }
FMT_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/cairn-trail/cairn-format-$REV"
if [ ! -d "$FMT_DIR/.git" ]; then
  rm -rf "$FMT_DIR"
  git clone --quiet https://github.com/ParkWardRR/cairn-esp32-device-firmware "$FMT_DIR"
  git -C "$FMT_DIR" checkout --quiet "$REV"
fi
(cd "$FMT_DIR" && scripts/fetch-contracts.sh >/dev/null 2>&1 &&
  cd emulator && cargo +stable run --release --quiet -- conformance | tail -1)

step "firmware: format"
(cd firmware && cargo fmt --check)

step "firmware: clippy (release and verbose)"
(cd firmware && cargo clippy --release -- -D warnings)
(cd firmware && cargo clippy --release --features verbose -- -D warnings)

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' firmware/Cargo.toml | head -1)
COMMIT=$(git rev-parse --short=10 HEAD)
DIRTY=$(git status --porcelain -- firmware crates | grep -q . && echo "-dirty" || true)
NAME="cairn-trail-${VERSION}-${COMMIT}${DIRTY}"

rm -rf dist && mkdir -p dist
ELF=firmware/target/xtensa-esp32s3-none-elf/release/cairn-trail

for flavour in verbose release; do
  step "firmware: build ${flavour}"
  if [ "$flavour" = verbose ]; then
    (cd firmware && cargo build --release --features verbose)
  else
    (cd firmware && cargo build --release)
  fi
  cp "$ELF" "dist/${NAME}-${flavour}.elf"
  espflash save-image --chip esp32s3 --merge --skip-padding --flash-size 8mb \
    --partition-table firmware/partitions.csv \
    "$ELF" "dist/${NAME}-${flavour}.bin" >/dev/null
done

step "artifacts"
(cd dist && shasum -a 256 ./*.elf ./*.bin > SHA256SUMS && cat SHA256SUMS)
ls -l dist
printf '\nCI passed: %s\n' "$NAME"
