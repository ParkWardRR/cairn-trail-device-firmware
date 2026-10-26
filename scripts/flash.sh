#!/usr/bin/env bash
# Build and flash one flavour to the board over USB.
#
#   scripts/flash.sh verbose|release [port]
#
# The board's own USB-Serial-JTAG lets espflash reset it. Two situations need
# help, and both are handled here:
#   - stock Meshtastic (TinyUSB CDC): a 1200-baud open reboots it into the ROM
#     bootloader;
#   - after that reboot the RTC keeps a "force download boot" bit set
#     (RTC_CNTL_OPTION1, 0x6000812C), so every reset returns to the bootloader
#     until it is cleared.
set -euo pipefail
cd "$(dirname "$0")/.."
FLAVOUR=${1:?usage: scripts/flash.sh verbose|release [port]}
PORT=${2:-$(ls /dev/cu.usbmodem* 2>/dev/null | head -1)}
[ -n "$PORT" ] || { echo "no /dev/cu.usbmodem* port"; exit 1; }
if [ -f "$HOME/export-esp.sh" ]; then . "$HOME/export-esp.sh"; fi

case "$FLAVOUR" in
  verbose) FEATURES=(--features verbose) ;;
  release) FEATURES=() ;;
  *) echo "flavour must be verbose or release"; exit 1 ;;
esac

# bash 3.2 (macOS) treats an empty array as unbound under `set -u`.
(cd firmware && cargo build --release ${FEATURES[@]+"${FEATURES[@]}"})
ELF=firmware/target/xtensa-esp32s3-none-elf/release/cairn-trail

# A/B app slots and the `cairn` identity partition (firmware/partitions.csv).
espflash flash --chip esp32s3 --port "$PORT" --partition-table firmware/partitions.csv "$ELF"

# Clear the force-download bit if a 1200-baud reboot left it set, then reset
# into the application through the watchdog.
PORT=$(ls /dev/cu.usbmodem* | head -1)
esptool --port "$PORT" --before no-reset --after no-reset read-mem 0x6000812C 2>/dev/null \
  | grep -q "= 0x00000001" && {
    echo "clearing RTC force-download bit"
    esptool --port "$PORT" --before no-reset --after watchdog-reset write-mem 0x6000812C 0 >/dev/null
  } || true
echo "flashed ${FLAVOUR} to ${PORT}"
