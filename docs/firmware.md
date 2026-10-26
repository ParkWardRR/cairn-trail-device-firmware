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

Over the board's own USB port (USB-Serial-JTAG; no driver needed). The host tool `trail`
(`tools/trail`, Rust) wraps it.

| Command | Does |
|---|---|
| `stat` | Every subsystem's latest state in one screen |
| `ls` | Sessions and files with sizes |
| `cat <S0007/GNSS.CSV>` or `cat GNSS.CSV` (open session) | Stream a file between `BEGIN` and `END` markers, with its CRC-32 |
| `flush` | Write and flush every buffer now |
| `stop` / `start` | Close the session (card safe to remove) / open a new one |
| `mark` | A mark, as the BOOT button |
| `gnssid` | The receiver's identity frames, for AssistNow registration |
| `gnss cold` / `warm` / `hot` | Restart the receiver forgetting time+orbits+position / orbits / nothing (UBX-CFG-RST), to measure time to first fix |
| `prime` / `prime time <unix_ms>` / `prime pos <lat> <lon> [alt_m] [acc_m]` | Show the priming record and plan / give the receiver a time or a position now (see [Priming](#priming)) |
| `assist begin` / `data <hex>` / `commit <crc32>` | Write `ASSIST.UBX` over USB and feed it to the receiver at once |
| `prov show` / `begin` / `data` / `commit` / `erase` | Identity and network settings ([uplink.md](uplink.md)) |
| `net` | Wi-Fi and check-in state |
| `reboot` | Close the session and restart |

**A `cat` pauses logging** while it streams (about 20 KB/s; a 800 KB IMU file takes 40 s): the
storage task owns the bus and the thread-mode tasks wait for it. The gap is visible in the files'
`t_ms`. GNSS keeps running on its interrupt-priority executor and is buffered meanwhile.

Payloads (`assist`, `prov`) go as `data` lines of 24 bytes, each answered with the running byte
count before the next is sent, then a CRC-32 over the whole: a lost byte is caught at once.

```sh
cd tools/trail && cargo build --release && cd -      # once; or scripts/ci.sh
T=tools/trail/target/release/trail
$T console --cmd stat --cmd ls
$T console --session S0007 --out pulled/S0007        # every file of a session, CRC-checked
$T console --pull S0007/GNSS.CSV --out pulled/S0007  # or one at a time
$T console --reset --seconds 30 --log boot.log       # a boot, captured
$T verify pulled/S0007 --stationary
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
- **Registration** (`trail assistnow register`, once per board) posts them to Thingstream's
  zero-touch-provisioning endpoint with the device profile's ZTP token:
  `{"token": …, "messages": {"UBX-SEC-UNIQID": "<hex>", "UBX-MON-VER": "<hex>"}}`. The response
  is kept in `~/.config/cairn/assistnow.json` (0600): the chipcode bound to this receiver, the
  service URL (`https://assistnow.services.u-blox.com/GetAssistNowData.ashx`) and the data the
  plan allows — `ulorb_l1` (live orbits), `uporb_1/3/7/14` (predicted orbits for 1–14 days),
  `ualm`, `utime`, `ukion`, `usvht`.
- **Download** (`trail assistnow fetch --param data=… --param gnss=gps,gal,bds,glo`) keeps the
  reply only if it is well-formed UBX, and lists its frames by type.
- **Onto the card**: `trail assistnow upload` over USB (the board writes `ASSIST.UBX` and feeds it
  at once), or `install` to a card in a reader.
- **On the board**, the storage task reads `ASSIST.UBX` a little per tick and hands it frame by
  frame to the GNSS task, which writes it to the receiver after configuring it (UBX input on,
  `CFG-NAVSPG-ACKAIDING` so each message is answered with UBX-MGA-ACK) — **one frame per pass of
  its loop**, reading the UART in between. `stat` shows frames queued, sent, accepted and not used;
  `EVENTS.CSV` records `assist_file` and `assist_sent`. The file stays on the card; the receiver
  ignores data that has expired. MGA-INI frames in the file — the time an Online reply was made,
  wrong by the time the card is used — are dropped; priming sends the time as it is now.

Which data: **live orbits** (`ulorb_l1`, ~8 KB for four constellations) are good for about four
hours — fetch just before heading out. **Predicted orbits** (`uporb_7`, `uporb_14`) last up to two
weeks and suit a card that goes on a trip.

Credentials live in `~/.config/cairn/thingstream.env` (mode 0600; `trail` refuses a file others can
read) and never in this repository.

**Verified on the board, 2026-10-26:** identity read, registration accepted, 8 396 bytes of live
orbits for GPS, Galileo, BeiDou and GLONASS (110 frames) fetched, uploaded over USB and fed: **all
109 MGA frames acknowledged as accepted** (the 110th, an MGA-INI time, dropped as above). After a
cold start and reboot the order held — priming time first, then the 109 — 110 of 110 accepted.
The time to a first fix outdoors with and without it is still to be measured: indoors there was no
fix either way.

## Priming

A receiver that wakes knowing neither when nor where it is searches the whole sky. Told roughly,
it looks only for the satellites that should be up — u-blox documents a faster strategy once the
position is known to ~100 km. Nothing needs the internet:

- **`PRIME.TXT`** in the card's root holds the last good fix (accuracy ≤ 50 m, refreshed every
  5 minutes while the fix holds): `lat`, `lon`, `alt_m` (height above the WGS84 ellipsoid),
  `acc_m`, `fix_unix` (when it was true), `rtc_unix` (when the RTC was last checked against GNSS or
  the computer) and `source`. Plain text: edit it on any computer, or write a position with
  `source=manual` and `fix_unix=0`.
- **At boot** the storage task reads it and plans (`trail_core::prime::plan`, host-tested):
  - *time*, from the RTC, only if `rtc_unix` says the RTC was checked, the RTC has not gone back
    since, and the check is under 180 days old; accuracy 2 s + 4 s per day since. A plausible RTC
    alone is not believed: this board's was set at a factory and read 25 days wrong.
  - *position*, with its accuracy grown at highway speed (25 m/s) for the time since it was true;
    a hand-written position with no time is taken as current; any other position of unknown age is
    given 300 km; nothing past 1000 km.
- **On the first NAV-PVT** the GNSS task decides: a receiver that woke with date and time kept
  its state on its backup supply (the 18650 feeds V_BCKP, a hot start) and is left alone; one that
  woke without gets `UBX-MGA-INI-TIME_UTC` (read off the RTC as it is sent) and
  `UBX-MGA-INI-POS_LLH`, before any AssistNow data. `EVENTS.CSV`: `prime_plan`, `prime`, and
  `fix_acquired … uptime_s=` — the time to first fix.
- **From the computer**: `trail prime` sends this Mac's clock (±1 s), setting the RTC and
  `rtc_unix`; `trail prime --lat … --lon … [--alt …] [--acc …]` also a position.
- The RTC is checked against GNSS **only with a fix**: a primed receiver reports the RTC's own time
  back as resolved, and checking the RTC against that echo would make it look trustworthy.

### Wi-Fi fingerprints: a position from the access points around

When `PRIME.TXT` has no position, or one too old to be within 1 km, the logger asks the Wi-Fi.
It is the fingerprinting-and-KNN method of
[wireless_localization2](https://github.com/dmachinewhisperer/wireless_localization2) (an ESP32
indoor-localization thesis project), turned around: there a building is surveyed by hand and a
model runs on a server; here **the GNSS does the survey wherever the logger goes, and the chip
does the matching, offline** (`trail_core::wififp`, host-tested).

- **Learning.** On a fix good to 30 m, at a place at least 100 m from every fingerprint so far (at
  most one scan a minute), the logger scans and appends a line to `WIFIFP.CSV` in the card's root:
  `unix,lat,lon,acc_m,bssid:rssi …` — the 16 strongest access points heard. BSSIDs with the
  locally-administered bit (phone hotspots, randomised MACs) are left out: they travel, often with
  the logger itself. `wifi learn <lat> <lon> [acc_m]` records one by hand, for a place the GNSS
  cannot see, such as indoors at home.
- **Locating, at boot.** A scan (~1 s) is matched against every fingerprint, streamed from the
  card: signal distance is the RMS of RSSI differences over every access point heard in either
  (one not heard counts as -100 dBm); a fingerprint counts only if it shares two access points with
  the scan. The three nearest are averaged, weighted by closeness; accuracy is 100 m plus their
  spread, and nothing is given if they disagree by more than 5 km (an access point that moved).
  Height comes from the old record, or else the barometer, with the accuracy widened to 300 m.
- **Power.** Not provisioned for a network, the radio is switched on only for each scan and off
  after it. Provisioned, the station scans while joined.
- **Console:** `wifi scan` (what is heard), `wifi locate` (the match now), `wifi learn`, `wifi
  forget` (empties the file). `EVENTS.CSV`: `wififp`, `wififp_learned`, `wifi_locate`.

**Verified on the board, 2026-10-26** (indoors, so with a hand-given position — a deliberately
false one, since removed): a scan while joined to the network heard 29 access points and kept 14
(15 mobile ones dropped); `wifi learn` wrote a fingerprint of 10; `wifi locate` matched it to
±100 m; after a reboot the logger read the fingerprint, scanned, matched and planned a Wi-Fi
position (±300 m, height from the barometer), and — the receiver having kept its state — sent
nothing. Learning from real fixes needs open sky.

**Verified on the board, 2026-10-26:** with no `PRIME.TXT` and the RTC 25 days wrong, nothing was
sent. After `trail prime`, the RTC read true, and a `gnss cold` + reboot gave a receiver reporting
2021-03-07: priming sent the time, the receiver acknowledged it, and within 7 s reported
2026-10-26 with date, time and full resolution valid. After a plain reboot the receiver kept its
time and priming correctly sent nothing. Position priming and the effect on time to first fix
need open sky.

## Cairn's bundle format on the board (T2, in progress)

The logger is gaining Cairn's sealed `format/v3` bundles beside its CSV files. The format is not
reimplemented here: the firmware builds **`cairn-format`**, the Rust implementation in the dongle
repository's emulator workspace (`emulator/cairn-format`, `no_std` + `alloc`), pinned by commit in
`firmware/Cargo.toml` — one Rust implementation for the emulator and this device.

- **On the host**, `scripts/ci.sh` checks out the dongle repository at that commit and runs its
  conformance runner: every `format/v3` vector (62 at `contracts-v0.9.0`).
- **On the board**, `vectors` on the console runs three of them on the ESP32-S3
  (`firmware/vectors/`, copied unchanged): a structural and keyed segment scan (`gnss-gap`: HKDF
  segment key, XChaCha20-Poly1305 open of every frame, plaintext digest), a tampered tag that only
  the keyed scan may catch (`auth-tag-tampered`), and a manifest (`manifest-members-match`: CBOR,
  Ed25519 signature, content root, byte-identical re-encode, and a flipped byte refused).

**Verified on the board, 2026-10-26:** `vectors: 3/3 pass on the ESP32-S3 in 122 ms`. With two
expectations deliberately broken the board reported both failures (`1/3`), so the check can fail.
The firmware grew from 887 KB to 1.07 MB with the format's crypto linked in. Sealing sessions into
bundles is next.

## SD cards

FAT16 or FAT32. A card formatted by macOS Disk Utility with its default scheme (GUID partition
map: an EFI partition, then the data partition) works: the firmware reads the GPT and presents the
first basic-data partition to the FAT library as if it were an MBR partition, never writing block 0
(`trail_core::gpt`). exFAT, which large cards ship with, does not; reformat as *MS-DOS (FAT)*.

## How v0.1.0 was verified

On 2026-10-25, on the owner's unit (V3.1 carrier, ESP32-S3 rev 0.2, MAX-M10S SPG 5.10, 62.6 GB
SDXC formatted FAT32 under a GUID map), indoors, on USB power with no cell fitted. Each build was
flashed from a clean commit, left to record, and every file of its session pulled back over USB
with matching CRC-32s; then the session check with `--stationary` (a Python script then, `trail verify` now). The output is in
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
filters settle. **v0.1.0 skips the first second** (`83f494f`). Checked on 2026-10-26 (session
S0026, at rest): |a| between 874 and 1080 mg from the first row; `trail verify --stationary` now
fails any session with a sample above 1.5 g at rest.

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
  plausible-looking lie. It is only corrected from a GNSS time with a fix, or the computer's clock.
- **esp-println's `auto` printer ate console input.** On the ESP32-S3 its USB-host check reads
  `0x60038000` — the USB-Serial-JTAG FIFO register, not the interrupt status at `+0x08` — so every
  line printed while a command was arriving swallowed one received byte: ~1 command in 9 with the
  verbose log on, and 6 bytes of the first provisioning payload. The firmware uses the
  `jtag-serial` printer; 24 of 24 commands then arrived.
- **Sixteen AssistNow frames in a row lost every acknowledgement.** At 9600 baud that is 1.3 s
  without reading the UART, and its 128-byte receive FIFO overflowed: 109 frames sent, 0
  acknowledged, NAV checksums failing. One frame per pass: 109 of 109 acknowledged.
