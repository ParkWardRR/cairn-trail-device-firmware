<!-- cairn-nav:start -->
<p align="center"><b>Cairn is a family of seven repositories.</b> Each builds, tests and releases on its own; they agree through the shared <a href="https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/main/contracts">contracts</a>, and they share one <a href="https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md">roadmap</a>.</p>

| Part | Repository | What it does | Stack | Docs | Issues | CI |
|---|---|---|---|---|---|---|
| Front door | [cairn-driving-log-selfhosted](https://github.com/ParkWardRR/cairn-driving-log-selfhosted) | Docs, roadmap, shared protocol contracts | Markdown · Go tools | [docs](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/issues) | [CI](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/actions) |
| Dongle | [cairn-esp32-device-firmware](https://github.com/ParkWardRR/cairn-esp32-device-firmware) | In-car recorder: OBD-II, GNSS, IMU to encrypted SD bundles | C++ · C · Rust | [docs](https://github.com/ParkWardRR/cairn-esp32-device-firmware/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-esp32-device-firmware/issues) | [CI](https://github.com/ParkWardRR/cairn-esp32-device-firmware/actions) |
| Trail device | **[cairn-trail-device-firmware](https://github.com/ParkWardRR/cairn-trail-device-firmware)** ◀ you are here | Handheld recorder for hiking, walking and driving without the dongle | Rust · esp-hal · Embassy | [docs](https://github.com/ParkWardRR/cairn-trail-device-firmware/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-trail-device-firmware/issues) | [CI](https://github.com/ParkWardRR/cairn-trail-device-firmware/actions) |
| Phone | [cairn-ios-companion-app](https://github.com/ParkWardRR/cairn-ios-companion-app) | BLE relay, GPS assist, server client | Swift · SwiftUI | [docs](https://github.com/ParkWardRR/cairn-ios-companion-app/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-ios-companion-app/issues) | [CI](https://github.com/ParkWardRR/cairn-ios-companion-app/actions) |
| Server | [cairn-vehicle-server](https://github.com/ParkWardRR/cairn-vehicle-server) | Verifies, decrypts, stores; serves app and dashboard | Go | [docs](https://github.com/ParkWardRR/cairn-vehicle-server/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-vehicle-server/issues) | [CI](https://github.com/ParkWardRR/cairn-vehicle-server/actions) |
| Dashboard | [cairn-vehicle-web-dashboard](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard) | Browser UI: trips, places, engine, health | Nuxt · TypeScript | [docs](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard/issues) | [CI](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard/actions) |
| Modules | [cairn-modules](https://github.com/ParkWardRR/cairn-modules) | Interpretation, separated from the logging core: one package per module | YAML · Rust | [readme](https://github.com/ParkWardRR/cairn-modules#readme) | [issues](https://github.com/ParkWardRR/cairn-modules/issues) | [CI](https://github.com/ParkWardRR/cairn-modules/actions) |

<sub>Shared: [Roadmap](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md) · [Install](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/INSTALL.md) · [Architecture](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/docs/architecture.md) · [Threat model](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/docs/threat-model.md) · [Trust model](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/docs/trust-model-v3.md) · [Contracts](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/main/contracts) · [Archive of the original monorepo](https://github.com/ParkWardRR/cairn-original-monorepo-archive)</sub>
<h1 align="center">Cairn trail device firmware</h1>
<p align="center"><strong>Firmware for a pocket GNSS recorder — the LILYGO T-Beam Supreme — for hiking, walking, and driving without the Cairn dongle. Rust on esp-hal and Embassy.</strong></p>

<p align="center">
  <a href="https://github.com/ParkWardRR/cairn-trail-device-firmware/releases/latest"><img src="https://img.shields.io/github/v/release/ParkWardRR/cairn-trail-device-firmware?style=flat-square&label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/CI-local_(scripts%2Fci.sh)-2E86C1?style=flat-square" alt="Local CI">
  <img src="https://img.shields.io/badge/target-ESP32--S3_·_T--Beam_Supreme-2E86C1?style=flat-square" alt="ESP32-S3, T-Beam Supreme">
  <img src="https://img.shields.io/badge/Rust-esp--hal_1.2_·_Embassy-2E86C1?style=flat-square" alt="Rust, esp-hal 1.2, Embassy">
  <img src="https://img.shields.io/badge/LoRa-off-7F8C8D?style=flat-square" alt="LoRa off">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Blue_Oak_1.0.0-2E86C1?style=flat-square" alt="Blue Oak Model License 1.0.0"></a>
</p>

---

**v0.1.0 is a capture logger.** Power it on and it records everything the board can sense —
GNSS, every satellite it sees, accelerometer and gyroscope, pressure with elevation and climb,
the magnetic field, battery and power rails, the GNSS time pulse — each to its own CSV file in a
new session folder on the SD card. The LoRa radio stays off. A screen shows what it is doing,
the BOOT button marks a spot, and a USB console reads the card without taking it out.

It is the first step of the [Cairn](https://github.com/ParkWardRR/cairn-driving-log-selfhosted)
trail device. The CSV files are a bring-up format: the next release writes Cairn's sealed,
encrypted `format/v3` bundles instead, so a hike reaches your own server the way the car's drives
do.

**Contents:**
[Quick start](#quick-start) ·
[What it records](#what-it-records) ·
[Using it](#using-it) ·
[Status](#status-what-is-proven) ·
[Hardware](#hardware) ·
[Where it is going](#where-it-is-going) ·
[Building and releasing](#building-and-releasing) ·
[Repository layout](#repository-layout) ·
[Contributing](#contributing)

## Quick start

You need a **LILYGO T-Beam Supreme** (ESP32-S3, u-blox MAX-M10S, 1.3" OLED) and a microSD card
formatted **FAT32** (macOS Disk Utility's *MS-DOS (FAT)* works with either partition scheme; exFAT
does not).

1. Download `cairn-trail-<version>-<commit>-release.bin` from the
   [latest release](https://github.com/ParkWardRR/cairn-trail-device-firmware/releases/latest) and
   check it against `SHA256SUMS`.
2. Back up what is on the board first:
   `esptool --port /dev/cu.usbmodem… read-flash 0 ALL tbeam-backup.bin`.
3. Flash: `esptool --chip esp32s3 --port /dev/cu.usbmodem… write-flash 0x0 cairn-trail-…-release.bin`.

Coming from stock **Meshtastic**, the board's TinyUSB port ignores esptool's reset. Open the port
at 1200 baud to reboot it into the bootloader, and afterwards clear the RTC's force-download bit
(`esptool … write-mem 0x6000812C 0`) or every reset returns to the bootloader.
[`scripts/flash.sh`](scripts/flash.sh) does both. Details:
[docs/firmware.md](docs/firmware.md#building-flashing-releasing--all-local).

Insert the card with USB unplugged, power on, and a new `S0001` folder starts filling.

## What it records

One folder per power-on, `S0001`, `S0002`, …; every row starts with `t_ms`, milliseconds since
boot, so rows line up across files. UTC is a column wherever a source supplies it.

| File | Rate | What |
|---|---|---|
| `GNSS.CSV` | 1 Hz | Everything in u-blox UBX-NAV-PVT: time, fix, position, altitudes, the receiver's own accuracy estimates, velocity, heading, DOP |
| `SAT.CSV` | 5 s | Every satellite: constellation, signal strength, elevation, azimuth, used in the fix |
| `IMU.CSV` | 50 Hz | Accelerometer (±8 g), gyroscope (±512 °/s), temperature |
| `BARO.CSV` | 4 Hz | Pressure, temperature, humidity — and elevation, anchored to GNSS, with climb and descent |
| `MAG.CSV` | 10 Hz | Magnetic field and an uncalibrated heading |
| `POWER.CSV` | 5 s | Battery, USB and system voltage, charging, fuel gauge, which power rails are on |
| `PPS.CSV` | 1 Hz | The GNSS time pulse against the board's clock (once there is a fix) |
| `EVENTS.CSV` | — | Boot identity, sensor bring-up, the SD card's layout, GNSS configuration, fixes gained and lost, marks, buttons, errors |
| `INFO.TXT` | — | Firmware version and commit, MAC, session |

Every column is described in [docs/firmware.md](docs/firmware.md#what-it-records).

## Using it

| | |
|---|---|
| **BOOT** (short) | Mark the spot — written to `EVENTS.CSV` with position and elevation |
| **BOOT** (hold 1.5 s) or **PWR** (tap) | Screen on/off. The release build turns it off after 60 s |
| **PWR** (hold 6 s) | Power off |
| **Screen** | Session and recording state, fix and satellites, position, GNSS and barometric altitude, climb, pressure, battery |
| **USB console** | `stat`, `ls`, `cat <file>` (with CRC-32), `flush`, `stop` (card safe to remove), `start`, `mark`, `reboot` |

Pull a session and check it from a Mac:

```sh
PY=$(ls /opt/homebrew/Cellar/esptool/*/libexec/bin/python | head -1)   # any Python with pyserial
$PY scripts/console.py --session S0003 --out pulled/S0003   # every file, CRC-checked
python3 scripts/verify.py pulled/S0003 --stationary          # rates, columns, plausible values
```

A `cat` pauses logging while it streams (about 20 KB/s); the gap shows in the files.

## Status: what is proven

Cairn's three words, kept apart: **Proven** ran on the real board with named evidence; **Built**
exists and passes its tests; **Designed** is a plan.

| | |
|---|---|
| **Proven** (2026-10-25, the owner's board, indoors on USB) | Every stream at its rate (GNSS 1.00 Hz, IMU 50.0 Hz, barometer 4.00 Hz, magnetometer ~10 Hz, power every 5 s); files pulled back over USB with matching CRC-32s; `verify.py` passing for both the verbose and the release build ([evidence](evidence/)); the screen, the BOOT mark and the PWR toggle by eye; the LoRa rail off in every power row; macOS GPT-formatted cards |
| **Built, not yet proven** | A GNSS fix — and with it PPS, setting the clock from GNSS, anchoring elevation — because the board has not seen the sky; running on an 18650; long runs; skipping the IMU's first second after start-up (merged after the last verified run) |
| **Designed** | Sealed `format/v3` bundles, activities (hike, walk, drive), Bluetooth and Wi-Fi offload to a Cairn server, battery-life work, secure boot — [the plan](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/docs/trail-device-plan.md) |

## Hardware

**LILYGO T-Beam Supreme** — an ESP32-S3 core module on a carrier board.

| Part | Chip | |
|---|---|---|
| MCU | ESP32-S3, 8 MB flash | Rust, `esp-hal` + Embassy |
| GNSS | u-blox MAX-M10S | UBX only, configured fresh every boot |
| IMU | QMI8658 | On the SPI bus shared with the SD card |
| Barometer | Bosch BME280 | Pressure, temperature, humidity |
| Magnetometer | QMC6309 (V3.1) / QMC6310 (V3.0) | |
| Power | X-Powers AXP2101 | Rails, charging, fuel gauge, PWR key |
| Clock | NXP PCF8563 | Own backup cell |
| Display | SH1106, 128×64 | |
| LoRa | Semtech SX1262 | **Unused and unpowered**: its rail (ALDO3) is switched off at every boot — the PMU's power-up default turns it on |

The full rail map, pins, registers and what was read off a real unit are in
[docs/hardware.md](docs/hardware.md). Bring-up found six board-level traps, each fixed and
written down in [docs/firmware.md](docs/firmware.md#what-bring-up-found) — among them an IMU
register that locks its SPI port until power-off if written whole, and an I²C scan that wedges
the magnetometer. What eight other firmware projects for this board taught, and what their
licences let us take, is [docs/prior-art.md](docs/prior-art.md).

## Where it is going

Cairn keeps one roadmap, in the front door: the trail device is phases
[T1–T8](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md#t1t8--the-trail-device--designed).

- **T1 — bring-up.** Done, but for a fix under open sky and current measurements with a meter.
- **T2 — the sealed record.** `format/v3` bundles from `cairn-format`, the Rust implementation
  shared with the dongle's emulator, in place of the CSV files.
- **T3–T6.** Enrolment and the first hike on the dashboard; hike/walk/drive detection; battery
  life (receiver power save, cutting rails in sleep); Bluetooth offload through the phone and
  Wi-Fi uplink at home.
- **T8.** Secure boot and flash encryption — this chip supports them, unlike the dongle's — on a
  sacrificial second board first. (T7, LoRa, is deferred: the radio stays off.)

How the device fits Cairn — a *person* carrier instead of a car, `format/v3` unchanged, the same
server — is designed in
[docs/trail-device-plan.md](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/docs/trail-device-plan.md).

## Building and releasing

Everything runs locally: there is no hosted CI for this repository.

```sh
espup install --targets esp32s3   # once: Espressif's Xtensa Rust toolchain
cargo install espflash --locked   # once (rustc ≥ 1.95)
. ~/export-esp.sh

scripts/ci.sh                     # format, clippy (both flavours), host tests, both builds,
                                  # merged images + SHA256SUMS in dist/
scripts/flash.sh verbose          # debug build: every record echoed, diagnostics, screen always on
scripts/flash.sh release          # the quiet build
scripts/release.sh v0.1.0 docs/releases/v0.1.0.md   # clean tree only: CI, tag, GitHub release
```

## Repository layout

| Path | What |
|---|---|
| [`firmware/`](firmware/) | The firmware (`no_std`, Xtensa). `verbose` feature for the debug build |
| [`crates/trail-core/`](crates/trail-core/) | Host-tested logic, no hardware: UBX parser and builders, BME280 compensation, elevation fusion and climb, battery curve, RTC, GPT reading |
| [`scripts/`](scripts/) | `ci.sh`, `flash.sh`, `release.sh`; `console.py` (capture, commands, CRC-checked pulls); `verify.py` (check a session) |
| [`docs/firmware.md`](docs/firmware.md) | The manual: columns, console, flashing, SD cards, verification, bring-up findings |
| [`docs/hardware.md`](docs/hardware.md) | The board, from schematics and from the real unit |
| [`docs/prior-art.md`](docs/prior-art.md) | Lessons from other firmware for this board, and their licences |
| [`docs/bringup.md`](docs/bringup.md) | The staged bring-up procedure |
| [`docs/releases/`](docs/releases/) | Release notes |
| [`evidence/`](evidence/) | Logs and verification output from the real board |
| [`contracts.lock`](contracts.lock) | The Cairn contract release this repository builds against |

## Contributing

- Commit as the project's configured identity; the global identity guard blocks anything else.
- Run `scripts/ci.sh` before pushing. A release is cut only from a clean tree by
  `scripts/release.sh`.
- Claim *Proven* only with evidence from the board: add the log to `evidence/`.
- A change that needs a contract change starts in the
  [front door](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/main/contracts).
- Only MIT/Apache/BSD-licensed code may be ported (see [docs/prior-art.md](docs/prior-art.md));
  Meshtastic, SoftRF and the other GPL projects are read for facts only.
- **Do not add a roadmap here.** Cairn keeps exactly one, in the
  [front door](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md).

## License

[Blue Oak Model License 1.0.0](LICENSE). The AXP2101, QMI8658, QMC6309/6310 and PCF8563 register
facts come from Lewis He's MIT-licensed XPowersLib and SensorLib.
