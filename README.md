<h1 align="center">Cairn Trail — a pocket GPS logger</h1>
<p align="center"><strong>Open firmware that turns a LILYGO T-Beam Supreme into a self-contained GPS and sensor logger for hiking, walking and the odd drive. Power it on and it records your track, your elevation and everything else the board can sense to plain CSV files on a microSD card. No app, no account, no cloud, no radio.</strong></p>

<p align="center">
  <a href="https://github.com/ParkWardRR/cairn-trail-device-firmware/releases/latest"><img src="https://img.shields.io/github/v/release/ParkWardRR/cairn-trail-device-firmware?style=flat-square&label=release" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/board-LILYGO_T--Beam_Supreme-2E86C1?style=flat-square" alt="LILYGO T-Beam Supreme">
  <img src="https://img.shields.io/badge/GNSS-u--blox_MAX--M10S-2E86C1?style=flat-square" alt="u-blox MAX-M10S">
  <img src="https://img.shields.io/badge/output-CSV_on_microSD-27AE60?style=flat-square" alt="CSV on microSD">
  <img src="https://img.shields.io/badge/Rust-esp--hal_·_Embassy-2E86C1?style=flat-square" alt="Rust, esp-hal, Embassy">
  <img src="https://img.shields.io/badge/LoRa-off-7F8C8D?style=flat-square" alt="LoRa off">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Blue_Oak_1.0.0-2E86C1?style=flat-square" alt="Blue Oak Model License 1.0.0"></a>
</p>

---

**Contents:**
[What it does](#what-it-does) ·
[What you need](#what-you-need) ·
[Install](#install) ·
[Using it](#using-it) ·
[The files](#the-files) ·
[Getting your data off](#getting-your-data-off) ·
[What is proven](#what-is-proven) ·
[Under the hood](#under-the-hood) ·
[What is next](#what-is-next) ·
[Later: connecting it to a Cairn server](#later-connecting-it-to-a-cairn-server) ·
[Building from source](#building-from-source) ·
[Contributing](#contributing)

## What it does

- **Logs GPS.** Every second: position, altitude, speed, heading, and the receiver's own estimate
  of how accurate each of those is. u-blox MAX-M10S, on its default constellations (GPS, Galileo, BeiDou, QZSS).
- **Logs every satellite** it can see every five seconds — signal strength, elevation, azimuth,
  whether it was used — so a bad fix can be explained afterwards, not just noticed.
- **Tracks elevation properly.** A barometer measures height changes far more steadily than GPS
  does; GPS then sets the absolute level. Climb and descent are counted through a 3 m dead band,
  so standing still never adds up to a hill.
- **Logs motion and heading:** accelerometer and gyroscope 50 times a second, a compass 10 times a
  second.
- **Logs its own health:** battery, USB and charging, temperature, which power rails are on.
- **Marks the spot** when you press a button.
- **Shows what it is doing** on the built-in screen: fix and satellites, position, altitude,
  climb, battery.
- **Keeps the LoRa radio switched off.** The T-Beam is sold as a radio board; this firmware never
  powers the radio, which saves battery and means it transmits nothing.
- **Writes plain CSV.** One folder per outing, one file per sensor. Open them in a spreadsheet,
  Python, R, QGIS — anything.

## What you need

| | |
|---|---|
| **Board** | [LILYGO T-Beam Supreme](https://lilygo.cc/) with the **u-blox MAX-M10S** GNSS option (the L76K version is not supported). Tested on the V3.1 carrier; V3.0 is supported in code but untested |
| **microSD card** | Formatted **FAT32**. On a Mac, Disk Utility → *MS-DOS (FAT)*, either partition scheme. Cards over 32 GB usually ship as exFAT and need reformatting |
| **Power** | USB-C, or an 18650 cell in the holder (battery operation is not yet proven — see [What is proven](#what-is-proven)) |
| **To install** | A computer with [`esptool`](https://docs.espressif.com/projects/esptool/) (`pip install esptool` or `brew install esptool`) and a USB-C cable |

## Install

1. Download `cairn-trail-<version>-<commit>-release.bin` and `SHA256SUMS` from the
   [latest release](https://github.com/ParkWardRR/cairn-trail-device-firmware/releases/latest),
   and check the file: `shasum -a 256 -c SHA256SUMS --ignore-missing`.
2. **Back up what is on the board now**, so you can always go back:
   ```sh
   esptool --port /dev/cu.usbmodem… read-flash 0 ALL tbeam-backup.bin
   ```
3. Flash:
   ```sh
   esptool --chip esp32s3 --port /dev/cu.usbmodem… write-flash 0x0 cairn-trail-…-release.bin
   ```
4. Unplug, put the card in, plug back in. A folder `S0001` appears on the card and starts filling.

**Coming from Meshtastic?** Its USB port ignores esptool's automatic reset. Either hold **BOOT**
and tap **RST** before flashing, or open the serial port at 1200 baud, which reboots the board into
its bootloader. That second route leaves a "force download" flag set, so afterwards every reset
returns to the bootloader until you clear it:
`esptool --port … --before no-reset --after watchdog-reset write-mem 0x6000812C 0`.
[`scripts/flash.sh`](scripts/flash.sh) handles both. To go back to Meshtastic, write your backup to
`0x0`.

## Using it

| Control | What it does |
|---|---|
| **BOOT** — short press | **Mark** this spot: time, position and elevation go into `EVENTS.CSV` |
| **BOOT** — hold 1.5 s | Screen on or off |
| **PWR** — tap | Screen on or off. The screen turns itself off after a minute to save power; any button wakes it |
| **PWR** — hold 6 s | Power off |

The screen shows six lines: session and whether it is recording · fix type, satellites and
accuracy · latitude and longitude · GPS and barometric altitude · climb, descent and pressure ·
battery or USB.

Each power-on starts a new session folder (`S0001`, `S0002`, …). Files are flushed every 10 seconds,
so switching off loses at most the last few seconds. For a clean stop before pulling the card, send
`stop` over USB (below).

## The files

Every row starts with `t_ms`, milliseconds since power-on, so rows from different files line up.
Times of day are in UTC, from the GPS.

| File | How often | Columns after `t_ms` |
|---|---|---|
| `GNSS.CSV` | every second | `itow_ms`, `utc`, `valid`, `t_acc_ns`, `fix_type`, `fix_ok`, `psm`, `num_sv`, `lat`, `lon`, `height_m`, `hmsl_m`, `h_acc_m`, `v_acc_m`, `vel_n_ms`, `vel_e_ms`, `vel_d_ms`, `speed_ms`, `heading_deg`, `s_acc_ms`, `head_acc_deg`, `pdop`, `invalid_llh` |
| `SAT.CSV` | every 5 s, one row per satellite | `itow_ms`, `gnss_id`, `sv_id`, `cno_dbhz`, `elev_deg`, `azim_deg`, `pr_res_m`, `quality`, `used`, `health` |
| `BARO.CSV` | 4 a second | `pressure_pa`, `temp_c`, `humidity_pct`, `qnh_pa`, `anchored`, `elevation_m`, `gain_m`, `loss_m` |
| `IMU.CSV` | 50 a second | `ax_mg`, `ay_mg`, `az_mg`, `gx_dps`, `gy_dps`, `gz_dps`, `temp_c` |
| `MAG.CSV` | 10 a second | `mx`, `my`, `mz`, `range_g`, `heading_deg_uncal` |
| `POWER.CSV` | every 5 s | `vbat_mv`, `vbus_mv`, `vsys_mv`, `battery`, `charging`, `vbus_good`, `soc_gauge_pct`, `soc_curve_pct`, `die_temp_c`, `dcdc`, `ldo` |
| `PPS.CSV` | every second, once there is a fix | `t_us` — the GPS time pulse against the board's clock |
| `EVENTS.CSV` | as things happen | `event`, `detail` — start-up, sensor checks, the card, GPS configuration, fix gained and lost, marks, buttons, errors |
| `INFO.TXT` | once | Firmware version and build, board serial (MAC), session |

Real rows from the board, sitting on a desk:

```csv
t_ms,pressure_pa,temp_c,humidity_pct,qnh_pa,anchored,elevation_m,gain_m,loss_m
100660,101051.78,26.09,62.89,101325.0,0,22.72,0.0,0.0

t_ms,ax_mg,ay_mg,az_mg,gx_dps,gy_dps,gz_dps,temp_c
60637,6,-132,968,-3.0,1.6,-0.2,26.58

t_ms,event,detail
190830,mark,"#1 fix_ok=0 lat_e7=0 lon_e7=0 elevation_m=23.2"
```

Until the receiver has a fix it reports placeholder values (a 2021 date, accuracy of thousands of
kilometres, `fix_ok=0`); they are written exactly as received, never cleaned up. Use rows with
`fix_ok=1`. Elevation reads against standard sea-level pressure (`anchored=0`) until GPS has given
30 good vertical fixes, then switches to the GPS-anchored level (`anchored=1`) without counting the
change as climb. Every column is documented in [docs/firmware.md](docs/firmware.md#what-it-records).

## Getting your data off

**Pull the card** and read it on any computer.

**Or over USB, without opening the case.** The board appears as a serial port; type commands at
115200 baud, or use the bundled script:

| Command | |
|---|---|
| `stat` | Everything the logger knows right now |
| `ls` | Sessions and files with sizes |
| `cat S0003/GNSS.CSV` | Send a file, framed and with a CRC-32 checksum |
| `stop` / `start` | Close the session so the card is safe to pull / open a new one |
| `flush` · `mark` · `reboot` | |

```sh
pip install pyserial
python3 scripts/console.py --cmd stat --cmd ls
python3 scripts/console.py --session S0003 --out data/S0003    # every file, checksums verified
python3 scripts/verify.py data/S0003                           # sanity-check a session
```

A transfer pauses logging while it runs (about 20 KB/s, so an hour of IMU data takes a few
minutes); the gap is visible in the files.

## What is proven

Tested on 2026-10-25 on a real board (V3.1 carrier), indoors, on USB power. Each build was left
recording, every file was pulled back with matching checksums, and
[`scripts/verify.py`](scripts/verify.py) checked rates, columns and values. The logs are in
[`evidence/`](evidence/).

| Works, with evidence | Not yet proven |
|---|---|
| GPS stream at 1.00 a second, all columns every row | **A GPS fix** — the board has not yet seen open sky, so position, the time pulse, setting the clock from GPS and anchoring elevation are untested |
| Satellite rows every 5 s (up to 18 satellites seen through a roof) | **Running on a battery** and charging one |
| Barometer at 4.00 a second, elevation steady to under a metre at rest | Long runs (hours, a full card) |
| Accelerometer and gyroscope at 50.0 a second, ~1 g at rest | Battery life |
| Compass at ~10 a second; power every 5 s, radio off in every row | Skipping the IMU's first second after power-on (fixed after the last test run) |
| Screen, mark button and screen toggle, checked by eye; macOS-formatted cards | |

## Under the hood

**Hardware.** The T-Beam Supreme is an ESP32-S3 module on a carrier board:

| Part | Chip | |
|---|---|---|
| Processor | ESP32-S3, 8 MB flash | |
| GPS | u-blox MAX-M10S | Configured from scratch at every power-on, 1 Hz, pedestrian dynamics |
| Barometer | Bosch BME280 | 16× oversampling, IIR filter ×16 |
| Accelerometer + gyroscope | QST QMI8658 | ±8 g, ±512 °/s, 62.5 Hz with low-pass filtering |
| Compass | QST QMC6309 (V3.1) / QMC6310 (V3.0) | Uncalibrated |
| Power | X-Powers AXP2101 | Charging, fuel gauge, the power button |
| Clock | NXP PCF8563 | Own backup cell; set from GPS once there is a fix (not yet seen) |
| Screen | SH1106 OLED, 128×64 | |
| Radio | Semtech SX1262 LoRa | **Never powered.** The power chip switches it on by default at power-up; the firmware turns it off first thing |

**Software.** Rust, `no_std`, on [`esp-hal`](https://github.com/esp-rs/esp-hal) and
[Embassy](https://embassy.dev). The GPS reader runs at interrupt priority so a slow card write can
never make it drop data; one task owns each bus. The logic that does not need hardware — the GPS
protocol parser, barometer maths, elevation, battery curve, clock handling, partition reading —
lives in [`crates/trail-core`](crates/trail-core/) with host tests.

Getting there found six board-level traps, each written up in
[docs/firmware.md](docs/firmware.md#what-bring-up-found): an IMU register that locks its SPI port
until power-off if written whole, an I²C address scan that wedges the compass, an SD card that
corrupts the shared bus unless it is initialised first, the radio powered on by default, the GPS
keeping a previous firmware's settings, and a clock that was 25 days wrong but looked fine. The
board's full wiring is in [docs/hardware.md](docs/hardware.md); lessons from eight other firmware
projects for this board are in [docs/prior-art.md](docs/prior-art.md).

## What is next

- **A fix under open sky**, and battery-life measurements with a meter.
- **Battery life:** GPS power-save modes and cutting power to idle parts.
- **Trips, not just power cycles:** detect walking, hiking and driving, and start and stop on
  movement.
- **A tamper-evident record:** encrypted, signed sessions on the card in place of plain CSV
  (Cairn's `format/v3`), so a recording can be proven unaltered later. CSV export stays.

## Later: connecting it to a Cairn server

The logger is complete on its own: the card is the product. On top of it, optionally, it can be
one device in [**Cairn**](https://github.com/ParkWardRR/cairn-driving-log-selfhosted), a
self-hosted logbook that already records drives from a car's OBD-II port. That integration is
planned, not built:

- sessions written as Cairn's sealed `format/v3` bundles instead of CSV;
- sent to your own Cairn server by Bluetooth through the Cairn iPhone app, or by Wi-Fi at home,
  and deleted from the card only once the server has signed for them;
- shown on the Cairn dashboard as hikes and walks beside the car's drives.

The design is in the Cairn front door:
[docs/trail-device-plan.md](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/docs/trail-device-plan.md);
the schedule is phases
[T1–T8 of the Cairn roadmap](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md#t1t8--the-trail-device--designed).
This repository pins the Cairn contracts it will use in [`contracts.lock`](contracts.lock).

<!-- cairn-nav:start -->
<p align="center"><b>Cairn is a family of seven repositories.</b> Each builds, tests and releases on its own; they agree through the shared <a href="https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/main/contracts">contracts</a>, and they share one <a href="https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md">roadmap</a>.</p>

| Part | Repository | What it does | Stack | Docs | Issues | CI |
|---|---|---|---|---|---|---|
| Front door | [cairn-driving-log-selfhosted](https://github.com/ParkWardRR/cairn-driving-log-selfhosted) | Docs, roadmap, shared protocol contracts | Markdown · Go tools | [docs](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/issues) | [CI](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/actions) |
| Dongle | [cairn-esp32-device-firmware](https://github.com/ParkWardRR/cairn-esp32-device-firmware) | In-car recorder: OBD-II, GNSS, IMU to encrypted SD bundles | C++ · C · Rust | [docs](https://github.com/ParkWardRR/cairn-esp32-device-firmware/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-esp32-device-firmware/issues) | [CI](https://github.com/ParkWardRR/cairn-esp32-device-firmware/actions) |
| Trail device | **[cairn-trail-device-firmware](https://github.com/ParkWardRR/cairn-trail-device-firmware)** ◀ you are here | Pocket GPS and sensor logger; a Cairn device later | Rust · esp-hal · Embassy | [docs](https://github.com/ParkWardRR/cairn-trail-device-firmware/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-trail-device-firmware/issues) | local |
| Phone | [cairn-ios-companion-app](https://github.com/ParkWardRR/cairn-ios-companion-app) | BLE relay, GPS assist, server client | Swift · SwiftUI | [docs](https://github.com/ParkWardRR/cairn-ios-companion-app/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-ios-companion-app/issues) | [CI](https://github.com/ParkWardRR/cairn-ios-companion-app/actions) |
| Server | [cairn-vehicle-server](https://github.com/ParkWardRR/cairn-vehicle-server) | Verifies, decrypts, stores; serves app and dashboard | Go | [docs](https://github.com/ParkWardRR/cairn-vehicle-server/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-vehicle-server/issues) | [CI](https://github.com/ParkWardRR/cairn-vehicle-server/actions) |
| Dashboard | [cairn-vehicle-web-dashboard](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard) | Browser UI: trips, places, engine, health | Nuxt · TypeScript | [docs](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard/tree/main/docs) | [issues](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard/issues) | [CI](https://github.com/ParkWardRR/cairn-vehicle-web-dashboard/actions) |
| Modules | [cairn-modules](https://github.com/ParkWardRR/cairn-modules) | Interpretation, separated from the logging core: one package per module | YAML · Rust | [readme](https://github.com/ParkWardRR/cairn-modules#readme) | [issues](https://github.com/ParkWardRR/cairn-modules/issues) | [CI](https://github.com/ParkWardRR/cairn-modules/actions) |
<!-- cairn-nav:end -->

## Building from source

```sh
# once
cargo install espup espflash --locked   # espflash needs rustc 1.95 or newer
espup install --targets esp32s3         # Espressif's Rust toolchain for the ESP32-S3

# every time
. ~/export-esp.sh
scripts/ci.sh                           # checks, tests, both builds; flashable images in dist/
scripts/flash.sh release                # build and flash the normal build
scripts/flash.sh verbose                # the debug build: everything echoed over USB, screen always on
```

There is no hosted CI: [`scripts/ci.sh`](scripts/ci.sh) runs formatting, clippy for both builds,
the host tests and both firmware builds on your machine, and [`scripts/release.sh`](scripts/release.sh)
cuts a release from a clean tree.

| Path | |
|---|---|
| [`firmware/`](firmware/) | The firmware |
| [`crates/trail-core/`](crates/trail-core/) | Hardware-free logic, with host tests |
| [`scripts/`](scripts/) | Build, flash and release; `console.py` and `verify.py` for your data |
| [`docs/`](docs/) | The manual ([firmware.md](docs/firmware.md)), the board ([hardware.md](docs/hardware.md)), prior art, bring-up, release notes |
| [`evidence/`](evidence/) | Logs from the real board |

## Contributing

Issues and pull requests are welcome. Run `scripts/ci.sh` before sending a change, and back any
claim that something works on the board with a log in `evidence/`. Code may be ported only from
MIT, Apache or BSD-licensed projects; the GPL projects in [docs/prior-art.md](docs/prior-art.md)
are read for facts, never copied.

## License

[Blue Oak Model License 1.0.0](LICENSE). The register details for the power chip, IMU, compass and
clock come from Lewis He's MIT-licensed XPowersLib and SensorLib.
