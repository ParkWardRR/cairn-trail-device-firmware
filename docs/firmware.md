# Firmware v0.1: the capture logger

Everything the board can sense, each stream to its own CSV file on the SD card, with the LoRa
radio held off. It is the bring-up tool for T1 and the source of real data for designing T2 —
**not** Cairn's sealed record: nothing here is encrypted, signed or receipt-gated. That is
`format/v3`, which replaces these files in T2.

Rust on `esp-hal` 1.2 and Embassy (`esp-rtos` 0.4), for the ESP32-S3 with Espressif's Xtensa
toolchain. Two flavours from one tree: **verbose** (`--features verbose`: debug logs, every record
echoed to the console, register diagnostics at boot, display always on) and **release** (info logs,
a status line a minute, display off after 60 s).

## What it records

A new directory `Snnnn` on the card each boot, numbered past the highest one there. Every row of
every file starts with `t_ms`, milliseconds since boot, which orders rows across files; UTC is a
column where a source supplies it, never the index.

| File | Rate | Columns after `t_ms` |
|---|---|---|
| `GNSS.CSV` | 1 Hz | UBX-NAV-PVT, everything: `itow_ms`, `utc`, `valid` flags, `t_acc_ns`, `fix_type`, `fix_ok`, `psm`, `num_sv`, `lat`, `lon` (exact decimal from the 1e-7 integer), `height_m`, `hmsl_m`, `h_acc_m`, `v_acc_m`, `vel_n/e/d_ms`, `speed_ms`, `heading_deg`, `s_acc_ms`, `head_acc_deg`, `pdop`, `invalid_llh`. Values the receiver has not resolved are written as it reports them, never cleaned up |
| `SAT.CSV` | every 5 s | UBX-NAV-SAT, one row per satellite: `gnss_id`, `sv_id`, `cno_dbhz`, `elev_deg`, `azim_deg`, `pr_res_m`, `quality`, `used`, `health` |
| `IMU.CSV` | 50 Hz | QMI8658: `ax/ay/az_mg` (±8 g), `gx/gy/gz_dps` (±512 °/s), `temp_c`. The sensor runs at 62.5 Hz with its low-pass filters on |
| `BARO.CSV` | 4 Hz | BME280 (pressure ×16 oversampling, IIR ×16): `pressure_pa`, `temp_c`, `humidity_pct`, then the fused elevation: `qnh_pa`, `anchored`, `elevation_m`, `gain_m`, `loss_m` (3 m hysteresis) — see `trail_core::elevation` |
| `MAG.CSV` | 10 Hz | QMC6309 (QMC6310 on V3.0 carriers), ±8 G: raw `mx/my/mz`, `range_g`, `heading_deg_uncal` (uncalibrated: no hard/soft-iron correction yet) |
| `POWER.CSV` | every 5 s | AXP2101: `vbat_mv`, `vbus_mv`, `vsys_mv`, `battery`, `charging`, `vbus_good`, `soc_gauge_pct` (the PMU's gauge), `soc_curve_pct` (from an 18650 OCV curve), `die_temp_c`, and the `dcdc` / `ldo` rail-enable bytes — so the LoRa rail (ALDO3, bit 2 of `ldo`) is shown off in every row |
| `PPS.CSV` | 1 Hz | The receiver's time pulse on GPIO 6, timestamped in µs since boot (`t_us`): how the two clocks drift. Empty until the receiver has a fix — it emits no pulse before one |
| `EVENTS.CSV` | as they happen | `event`, `detail`: boot and build identity, the PMU's rails as found and as set, the RTC as found and any correction from GNSS, every IMU init step, I²C devices found, the SD card and its partition layout, the GNSS receiver's firmware and configuration acknowledgements, fix acquired and lost, button presses, marks, write errors |
| `INFO.TXT` | once | Firmware version, flavour and commit (a dirty tree says so), MAC, session |

Rows are buffered per file and written in 2 KiB blocks; every file is flushed (its directory entry
updated) when the session opens and every 10 s after, so a power cut loses at most the last ten
seconds. A record producers cannot hand over is dropped and counted (`dropped` in `stat`), never
blocked on.

## Buttons and display

| Input | Action |
|---|---|
| BOOT, short press | **Mark**: an event with the current position and elevation; wakes the display |
| BOOT, hold 1.5 s | Display on/off |
| PWR, short press | Display on/off |
| PWR, hold 6 s | Hardware power-off by the PMU (register `0x27` reads `0x14` on this unit) |

The display shows the session and whether it is recording, the fix, position, GNSS and barometric
altitude, climb and descent with pressure, and battery or USB state.

## The console

Over the board's own USB port (USB-Serial-JTAG; no driver needed). `scripts/console.py` wraps it.

| Command | Does |
|---|---|
| `stat` | Every subsystem's latest state in one screen |
| `ls` | Sessions and files with sizes |
| `cat <S0007/GNSS.CSV>` or `cat GNSS.CSV` (open session) | Stream a file between `BEGIN` and `END` markers, with its CRC-32 |
| `flush` | Write and flush every buffer now |
| `stop` / `start` | Close the session (card safe to remove) / open a new one |
| `mark` | A mark, as the BOOT button |
| `reboot` | Close the session and restart |

**A `cat` pauses logging** while it streams (about 20 KB/s; a 800 KB IMU file takes 40 s): the
storage task owns the bus and the thread-mode tasks wait for it. The gap is visible in the files'
`t_ms`. GNSS keeps running on its interrupt-priority executor and is buffered meanwhile.

```sh
PY=$(ls /opt/homebrew/Cellar/esptool/*/libexec/bin/python | head -1)   # has pyserial
$PY scripts/console.py --cmd stat --cmd ls
$PY scripts/console.py --session S0007 --out pulled/S0007     # every file of a session
$PY scripts/console.py --pull S0007/GNSS.CSV --out pulled/S0007  # or one at a time
python3 scripts/verify.py pulled/S0007 --stationary
```

## Building, flashing, releasing — all local

There is no hosted CI for this repository: the owner chose local CI/CD.

```sh
espup install --targets esp32s3          # once: the Xtensa toolchain
cargo install espflash --locked          # once (needs rustc ≥ 1.95)
. ~/export-esp.sh

scripts/ci.sh                            # format, clippy (both flavours), host tests, both builds,
                                         # merged images and SHA256SUMS in dist/
scripts/flash.sh verbose                 # or: release
scripts/release.sh v0.1.0 notes.md       # clean tree only: CI, tag, GitHub release with dist/
```

`dist/` holds, per flavour, the ELF and a merged image (bootloader + partition table + app) to
write at `0x0`:
`esptool --chip esp32s3 write-flash 0x0 cairn-trail-<version>-<commit>-release.bin`.

**From stock Meshtastic**, the first flash needs the board in its ROM bootloader. Its TinyUSB
serial port does not answer esptool's reset, but opening it at 1200 baud reboots it into the
bootloader; that reboot also sets the RTC's *force download boot* bit (`RTC_CNTL_OPTION1`,
`0x6000812C`), after which every reset returns to the bootloader until the bit is cleared.
`scripts/flash.sh` clears it. A full backup of the stock image is in `~/cairn-backups`
(SHA-256 `1a1310d3…05c36`); restoring it is `esptool write-flash 0x0 <backup>`.

## AssistNow

u-blox AssistNow supplies the orbit data a receiver otherwise has to download from the satellites
themselves, which takes minutes from cold. The logger takes it from the card:

- **At boot** the GNSS task reads the receiver's identity — UBX-MON-VER and UBX-SEC-UNIQID, the
  chip's unique ID — and keeps both whole frames; `gnssid` on the console prints them in hex
  (`EVENTS.CSV` records the unique ID as `gnss_uniqid`).
- **Registration** (`scripts/assistnow.py register`, once per board) posts them to Thingstream's
  zero-touch-provisioning endpoint with the device profile's ZTP token:
  `{"token": …, "messages": {"UBX-SEC-UNIQID": "<hex>", "UBX-MON-VER": "<hex>"}}` — a format
  confirmed from the API's validation errors (`messages` must be an object of hex strings). The
  response, which carries the chipcode bound to this receiver, is kept in
  `~/.config/cairn/assistnow.json` (0600).
- **Download** (`fetch`) asks the service the response names for data with that chipcode and keeps
  it only if it is well-formed UBX; **install** copies it to `ASSIST.UBX` in the card's root.
- **On the board**, the storage task reads `ASSIST.UBX` a little per tick and hands it frame by
  frame to the GNSS task, which writes it to the receiver after configuring it (UBX input on,
  `CFG-NAVSPG-ACKAIDING` so each message is answered with UBX-MGA-ACK), 8 ms apart. `stat` shows
  frames queued, sent, accepted and not used; `EVENTS.CSV` records `assist_file` and
  `assist_sent`. The file stays on the card; the receiver ignores data that has expired.

Credentials live in `~/.config/cairn/thingstream.env` (mode 0600; the script refuses a file others
can read) and never in this repository. **Status: built, not yet run** — identity capture,
registration, download and the feed all wait for the board to be plugged in.

## SD cards

FAT16 or FAT32. A card formatted by macOS Disk Utility with its default scheme (GUID partition
map: an EFI partition, then the data partition) works: the firmware reads the GPT and presents the
first basic-data partition to the FAT library as if it were an MBR partition, never writing block 0
(`trail_core::gpt`). exFAT, which large cards ship with, does not; reformat as *MS-DOS (FAT)*.

## How v0.1.0 was verified

On 2026-10-25, on the owner's unit (V3.1 carrier, ESP32-S3 rev 0.2, MAX-M10S SPG 5.10, 62.6 GB
SDXC formatted FAT32 under a GUID map), indoors, on USB power with no cell fitted. Each build was
flashed from a clean commit, left to record, and every file of its session pulled back over USB
with matching CRC-32s; then `scripts/verify.py --stationary`. The output is in
[`evidence/`](../evidence/).

| | Verbose build (`b54d1a8`, session S0009) | Release build (`3ea9c46`, session S0012) |
|---|---|---|
| GNSS | 1.00 Hz over 264 s, 24 columns every row | 1.00 Hz over 183 s |
| IMU | 50.04 Hz; at rest \|a\| ≈ 1 g, median \|gyro\| < 5 °/s | 50.01 Hz; \|a\| mean 987 mg |
| Barometer | 4.00 Hz; elevation steady to under a metre | 4.00 Hz |
| Magnetometer | 9.75 Hz | 9.78 Hz |
| Power | every 5 s, ALDO3 off in every row | every 5 s, ALDO3 off in every row |
| Events | boot, rails, RTC, IMU steps, SD layout, session, GNSS, **the BOOT mark and a PWR display toggle pressed by hand** | boot, rails, RTC, IMU steps, SD layout, session, GNSS, a console mark |
| PPS | empty, as it must be without a fix | empty |
| Screen | status screen shown, checked by eye | turned itself off after 60 s, as the release build should |

Both runs showed up to 3.7 g in the IMU's first half-second after enabling, while its low-pass
filters settle. **v0.1.0 skips the first second** (`83f494f`); that change was merged after the
last verified run and has not itself been checked on the board.

**Not yet proven:** a GNSS fix (and therefore PPS, RTC correction from GNSS and the elevation
anchoring), the display and BOOT button by eye, battery operation and charging, and long runs.

## What bring-up found

Each of these cost a debugging round on the real board; they are now handled in the code.

- **The QMI8658's `CTRL1` must be read-modify-written.** It powers up as `0x20`. Overwriting it
  with `0x40` (address auto-increment only) locked the IMU's SPI interface — reads returned
  `0x3E`/`0xDC` — until the board was powered off; the IMU is on the always-on 3V3 rail, so no
  ESP32 reset clears it. Setting only bit 6 (`0x60`), as SensorLib does, works.
- **Probing I²C addresses 0x78–0x7F wedges the QMC6309** at `0x7C` until the next power-off. Those
  addresses are reserved for 10-bit addressing; the scan stops at `0x77` and the magnetometer is
  identified by chip ID.
- **The SD card first, then the IMU.** Until a card has seen CMD0 with its CS low it ignores CS
  and listens to every clock on the shared bus. One byte clocked with every CS high before each IMU
  access releases MISO.
- **The PMU powers up with the LoRa rail on.** Its defaults are `dcdc=0x19 ldo=0x17`: ALDO3 on,
  ALDO4 (GNSS) off, ALDO1 and BLDO1 at 1.8 V. The firmware turns ALDO3 off before anything else.
- **The M10 keeps the last firmware's settings** while powered (and in battery-backed RAM): this
  unit answered at 115 200 baud after a reset. The firmware probes the baud rate, resets the
  configuration to defaults, and writes its own to the RAM layer every boot.
- **The RTC was set by Meshtastic and was 25 days wrong**, with its voltage-low flag clear — a
  plausible-looking lie. It is only corrected from a resolved GNSS time.
