# Prior art: what other firmware for this board taught

On 2026-10-25 the owner collected an analysis bundle for the T-Beam Supreme: the stock firmware
images, the board's schematics, and the source of eight projects that run on this board or do
something close to what Cairn's trail device does. This page is what was learned from reading
them. **Nothing in the bundle was built or run**, and **no code was copied**: Cairn is Blue Oak,
most of these projects are GPL-3.0 or carry no licence at all, and what carries over is facts,
numbers, designs and mistakes to avoid — written here in our own words, with a pointer to where
each came from.

Facts about the board itself — rails, registers, pins, addresses — are in
[hardware.md](hardware.md). The bring-up procedure that follows from them is
[bringup.md](bringup.md).

## The sources, and what we may take from each

| Project | Upstream, revision read | Licence | What we may take |
|---|---|---|---|
| LilyGO board support | `Xinyuan-LilyGO/LilyGo-LoRa-Series` @ `1991b1e` | Examples: `@license MIT` per file. Docs and schematics: none stated | Facts from the docs and schematics. The examples could be ported with attribution |
| XPowersLib (AXP2101) | inside the above, `lib/XPowersLib` v0.2.6 | **MIT** | The AXP2101 register map — the source our PMU driver is written from, with attribution |
| SensorLib (QMI8658, QMC63xx, PCF8563) | inside the above, `lib/SensorLib` v0.4.1 | **MIT** | Register maps and init sequences for the IMU, magnetometer and RTC drivers, with attribution |
| RadioLib | inside the above, `lib/RadioLib` | MIT | Not needed: the LoRa radio stays off |
| MeshCore | `meshcore-dev/MeshCore` companion-v1.18.0 @ `81dbae5` | **MIT** | Its Supreme board init (`src/helpers/esp32/TBeamBoard.{h,cpp}`) could be ported with attribution |
| usedbytes/tbeam | `usedbytes/tbeam` dev @ `1aa9a08` (2020, original T-Beam) | Files under `main/`, `components/fit/fit.{c,h}` and `go/process.go`: **MIT** (SPDX headers). `components/fit/gen/*`: Garmin FIT SDK licence. `go/serv.go`: none | MIT parts with attribution; the FIT SDK parts not at all |
| Meshtastic | `meshtastic/firmware` v2.7.10.94d4bdf (the build on the unit) | GPL-3.0 | Facts and lessons only |
| SoftRF | `lyusupov/SoftRF` @ `1bfad2d` and its wiki @ `bdebf59` | GPL-3.0; its FlightRecorder library LGPL-2.1+; wiki: none stated | Facts and lessons only |
| LoRa APRS Tracker | `richonguzman/LoRa_APRS_Tracker` @ `5bb2adc` | GPL-3.0 | Facts and lessons only |
| RNode | `markqvist/RNode_Firmware` 1.86 @ `9b39b6c` | GPL-3.0 | Facts and lessons only |
| SoloTrail | `PhilippFrenzel2005/SoloTrail_ESP32_Firmware` @ `04bf1ec` | **None** — all rights reserved | Ideas only |
| Tiny GPS Logger | `liuyal/Tiny_GPS_Logger` @ `e7e0cf9` | **None** — all rights reserved | Ideas only |

In practice: the drivers for the AXP2101, QMI8658, QMC6310/6309 and PCF8563 are written in Rust
from the MIT register maps, with a header crediting XPowersLib and SensorLib. Nothing else is
ported.

## 1. Power, and the LoRa radio

- **The LoRa radio has its own rail.** The SX1262 module (an HPD16A) is powered by the AXP2101's
  **ALDO3** and nothing else (core-module schematic, sheet 3). Every project that powers the radio
  down does it by disabling ALDO3. So *off* means ALDO3 is never enabled — no SPI traffic, no sleep
  command. With the rail off, **the radio's GPIOs must not be driven high** (CS 10, SCK 12, MOSI
  11, RESET 5, and GPIO 2), or the chip can be fed power through its pins; leave them as inputs.
  LilyGO's factory sleep code sets every pin to open-drain for the same reason.
- **Measured deep sleep on this board is about 1.45 mA** — LilyGO's own figure, against
  320–450 µA for its other boards (`examples/Factory/Factory.ino`). It is the floor any sleep mode
  starts from, and three times what the smaller boards manage, so a multi-day mode on this board
  is a matter of cutting rails, not just sleeping the CPU.
- **Before sleeping:** clear and disable the PMU's interrupts (LilyGO: "otherwise the sleep current
  will increase"), turn off the PMU's ADCs and charge LED, cut ALDO4 (GNSS), ALDO1 (display and
  I²C sensors), BLDO1 (SD), BLDO2 and DCDC3–5, and set every pin to open-drain.
- **Do not wake on the PMU interrupt.** Meshtastic leaves it unwired because it fires repeatedly
  when there is no battery or the battery is full. Wake on BOOT (GPIO 0) and a timer; read the PWR
  key's short and long presses from the PMU while awake.
- **Disable the PMU's TS pin.** The board has no thermistor; with TS measurement on, charging
  misbehaves (Meshtastic, XPowersLib's init).
- **Charging:** 500 mA to 4.2 V is what LilyGO, Meshtastic and SoftRF use (the tracker uses 800 mA).
  Start at 500 mA.
- **Cold boot:** LilyGO power-cycles ALDO1, ALDO2 and BLDO1 for 250 ms so a wedged SD card or
  magnetometer lets go of its bus. Worth copying.
- **Rail reference counting** (usedbytes): the first user of a rail sets its voltage, the last
  release turns it off. A good shape for a Rust PMU service.
- **Battery percentage.** Meshtastic interpolates a Li-ion discharge curve — 4190, 4050, 3990, 3890,
  3800, 3720, 3630, 3530, 3420, 3300, 3100 mV for 100 % down to 0 % in 10 % steps. SoftRF and RNode
  use straight lines, which read badly mid-range. The AXP2101 has its own gauge (register `0xA4`);
  compare it against a curve before trusting either.
- **Low-battery cutoff.** SoftRF warns at 3.5 V and shuts down below 3.2 V after three readings
  5 s apart; Meshtastic shuts down below 3.1 V after ten readings 20 s apart, never on USB. Both
  debounce, and Cairn's must too — then **seal the open bundle before shutting down**.

## 2. The u-blox MAX-M10S

- **It speaks UBX-CFG-VALSET only.** The legacy configuration messages (CFG-PRT, CFG-MSG, CFG-RATE,
  CFG-NAV5) that older u-blox code uses — usedbytes, SoftRF — are not the way to configure an M10.
  Meshtastic uses VALSET throughout, which is the model to follow.
- **It ships at 9600 baud.** Meshtastic probes 9600, 115200, 38400, then 4800 and 57600; LilyGO's
  recovery tool scans up to 921600.
- **Configuration from earlier firmware survives a reflash.** The M10 has no flash; Meshtastic
  saves its settings to the battery-backed RAM (BBR), which is kept alive by the 18650 through the
  PMU's VRTC. This unit has run Meshtastic 2.7.10, so if a cell was ever fitted the receiver may
  still hold Meshtastic's choices — NMEA GGA and RMC only, cyclic power save, SBAS off, jamming
  detection on. **Cairn sends its whole configuration to the RAM layer at every boot**, and clears
  the BBR layer once, at first boot, rather than inherit anything.
- **Read UBX-NAV-PVT, not NMEA.** Meshtastic and SoftRF parse NMEA and so lose the receiver's own
  horizontal and vertical accuracy estimates; `format/v3` requires them. NAV-PVT carries them, and
  its `gnssFixOK` flag is the honest fix bit (usedbytes keys its state machine on it).
- **Pedestrian dynamic model** (`CFG-NAVSPG-DYNMODEL` = 3) on foot, automotive in a car. SoftRF runs
  the airborne model; it is the wrong filter for a hiker.
- **Power-save limits** (Meshtastic's notes in `src/gps/ubx.h`): M10 power save does not work with
  SBAS or with BeiDou B1C; B1I cannot be combined with B1C or GLONASS. The workable set for power
  save is the default — GPS, Galileo, BeiDou B1I, QZSS — with SBAS off. After a short sleep the
  receiver does not pick up new satellites, so the satellite count stays near its first-fix value.
- **Sleeping the receiver.** Software backup: UBX-RXM-PMREQ with a duration, woken by a byte on its
  UART (send `0xFF`, flush what comes back). Hard off: cut ALDO4. Meshtastic's fitted rule is that
  cutting power pays only when the interval exceeds about 2750 × (lock time in s)^1.22 ms, and it
  never sleeps the receiver for intervals of 10 s or less. Keep it powered for **20 s after a new
  lock** so ephemeris finishes downloading.
- **Keep the rate at or under 5 Hz** — Meshtastic records satellites dropping out above it.
- **Hot start needs the 18650.** The receiver's backup supply is VRTC, fed from the cell; with no
  cell every power-up is a cold start (LilyGO's hardware doc, SoftRF's wiki).
- **The wake pin (GPIO 7) only does anything on the L76K variant.** MeshCore treats it as a GPS
  enable on all boards, which on the u-blox version means "stop GPS" does nothing.
- **Telling the M10 from the L76K:** UBX-MON-VER, where hardware version `000A0000` is an M10
  (Meshtastic), or the L76K's reply to `$PCAS06,0*1B` (LilyGO).
- **The receiver resets itself sometimes.** Meshtastic counts it by spotting the boot banner, and
  flushes the UART when its buffer fills. Both belong in a robust reader.
- **Current:** SoftRF's wiki quotes about **105 mA for the whole board with the M10 tracking**, and
  30–40 µA powered off. At 105 mA a 3000 mAh cell lasts under 29 hours before the display, Wi-Fi or
  BLE draw anything — so continuous full-power tracking alone barely meets a 24-hour target, and
  receiver power save is not optional.

## 3. Time

- **Accept a GNSS time only if it is plausible.** SoftRF refuses a year before the firmware's build
  year or more than 15 years after it; Meshtastic refuses anything before the build date. That is
  [invariant 6](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md#guiding-invariants)
  in other words, and the floor Cairn applies at the point an observation becomes a basis.
- **Set the RTC from GNSS, and only from a plausible, fresh time** (SoftRF writes the PCF8563 once,
  inside the same window). The PCF8563 has a voltage-low flag; a clock that lost power is not a time
  source.
- **Time a track by GNSS time, not by milliseconds since boot** — SoftRF's recorder and SoloTrail
  both stamp records with the CPU clock, which is the 1970-dates problem Cairn spent a week on.

## 4. Recording and storage

- **SoftRF's flight recorder rewrites the file on every point** — open, seek back over the signature
  lines, write the new record, recompute and rewrite the signature, close. The file is always complete
  after a power cut, and its wiki warns that the card then wears out: replace it every 100–200 logged
  hours. Cairn's append-only segments with a recovery scan get the same guarantee without rewriting.
- **usedbytes writes a FIT file whose header and CRC are patched at the end**, so a power cut leaves
  an invalid file. Export formats belong on the host, generated from the sealed record — never the
  record itself.
- **Leave a missing value empty, never zero** (SoloTrail's fixed-column CSV). `format/v3` already
  has sentinels for exactly this.
- **Don't flush on every row** (SoloTrail does) — and don't buffer unboundedly either. Flush on a
  schedule and on a low-battery warning.
- **Resume after a reboot** (Tiny GPS Logger persists its logging flag): a battery swap mid-hike must
  continue the activity, which Cairn's trip-as-outing model already does across boots.
- **Design transfer in chunks from the start**: Tiny GPS Logger's file listing and file read are
  commented out because they never worked over BLE. Cairn's `ble/v1` offload is chunked by design.
- **No SD hot-plug; format cards with the SD Association's formatter** (SoftRF's wiki).

## 5. Elevation

**No project fuses barometric and GNSS altitude, and none computes elevation gain.** SoloTrail logs
both side by side with a fixed 1013.25 hPa reference and its filters off; MeshCore samples once with
no filter. So this is Cairn's own design, informed by what they lacked:

- BME280 in normal mode, pressure oversampling ×16, IIR filter ×16 (Bosch's "indoor navigation"
  preset, in LilyGO's BSD-licensed example).
- **Record raw pressure and temperature**; fuse on the server, where it can be recomputed.
- Anchor the barometric altitude to GNSS mean-sea-level altitude only when the receiver's vertical
  accuracy is good, averaged over several fixes, and correct towards GNSS slowly after that.
- Count climb only once the filtered altitude leaves a hysteresis band of a few metres (3–5 m is
  the starting point), so a pressure wobble does not become a hill.

## 6. Sampling and track points

- **Adaptive rate** (the tracker's SmartBeaconing): slow below walking pace, faster with speed, and
  a point on a turn when the heading change exceeds roughly 12° + 60 ÷ speed. Heading wraps (355° to
  5° is 10°).
- **Ignore heading below about 0.5 km/h**, and use the magnetometer for heading at rest.
- **Hold a quality floor for a fix**: SoftRF wants every field younger than 3.5 s; Meshtastic drops
  fixes older than 5 s, HDOP of 0, courses of 360° or more.
- **Learned time-to-fix**: Meshtastic averages lock times (ignoring the first) and wakes the receiver
  early by that much. Useful in endurance mode.

## 7. Buttons, screen and USB

- **SoftRF on this board:** PWR short press = next page, long press = power off; BOOT click = back,
  with a 600 ms click window. Holding PWR for about 6 s is a hardware power-off by the PMU — LilyGO's
  code sets 4 s, its docs say 6 s; read the register.
- **Meshtastic:** power-off on *release* of a 3.9 s hold, with a rising tone from 2.2 s, and ignored in
  the first 30 s after boot — so the release cannot immediately wake the board.
- **Screen:** Meshtastic redraws at 1 fps when idle; the tracker blanks after a few seconds, never in
  the first 10 s after boot. No project protects the OLED from burn-in; Cairn's screen is off by
  default, which does.
- **USB:** the S3's native USB goes straight to the USB-C port. Meshtastic uses TinyUSB, so on the
  unit as it stands the serial port's DTR/RTS cannot reset the chip — the first Cairn flash needs
  BOOT held while RST is tapped. Firmware on `esp-hal` uses the USB-Serial-JTAG peripheral, after
  which `espflash` resets the chip itself.

## 8. Bring-up method

SoloTrail stages its bring-up — boot, GPS, barometer, SD, Wi-Fi, then the combined application — each
with its own build, a fill-in test template and a dated evidence file holding the real serial output.
LilyGO's factory test prints a bus scan, a rail table and a one-screen pass/fail summary. Cairn's
[bring-up](bringup.md) does both, as one `examples/` binary per stage with `defmt` logs.

## 9. Bugs found in passing

Not ours to fix, but each is a test we should have:

| Where | Bug | Our test |
|---|---|---|
| usedbytes `main/ubx.c` | The UBX parser never advances its index, so a 92-byte NAV-PVT split across two UART reads is overwritten; a bad second checksum byte does not reset the state machine | Feed the parser every split point of a NAV-PVT frame, and corrupted checksums |
| SoftRF FlightRecorder | `(int) .5 + x` casts only the 0.5, truncating coordinates (about 1.85 m bias) | Round-trip coordinates through every encoder |
| Meshtastic `GPS.cpp` | Latitude and longitude range checks are one-sided, so large negative values pass | Reject out-of-range in both directions |
| LoRa APRS Tracker | An HDOP of exactly 5 falls between two bands; `OLED_RST` is defined as GPIO 16, the 32.768 kHz crystal pin | Boundary values in every banded display |
| MeshCore | The LoRa TX LED is defined as GPIO 6, the GNSS PPS pin; the OLED and BME280 addresses differ between its ini and header | Probe addresses at runtime, never trust a constant |
| RNode | Its peripheral power-down uses the original T-Beam's rail map on the Supreme — cutting the sensors and leaving GNSS on | Name rails by load, not by number, in the code |
| Tiny GPS Logger | "GPS off" stops reading the UART but leaves the module powered | Measure current, don't infer it |

## 10. The owner's other firmware for this board

The trail device shares its T-Beam Supreme (`48:ca:43:5b:ed:60`) with two of the owner's own
projects, so these are not prior art read from a distance — they are measurements from **this
physical unit**, and the owner's work, with no licence question. Source:
`tbeam-supreme-firmware-handoff.md` and `tbeam-imu-report.md`, both revised 2026-11-11.

| Firmware | What it is | Where |
|---|---|---|
| `tbeam-fw` | A Meshtastic-compatible node feeding a Hackaday Supercon badge over BLE. PlatformIO/Arduino | `~/antigravity/supercon/tbeam-fw` |
| CarNet Beam | A drive logger built on this board for the BMW CarPlay work, flashed 2026-11-11 | `~/antigravity/bmw-f32-carplay-wan/tbeam` |

**Whichever firmware is on the board, the other two must be restorable.** `tbeam-fw` holds the
owner's mesh node identity and its BLE bonds in NVS, so a full-image backup comes off the board
before Cairn is flashed, and only an image taken off *this* board is ever written back.

### The QMI8658 latch, from both sides

`tbeam-fw` read `WHO_AM_I = 0x3E` with `0x0A`/`0x0B` = `0xDC` and `0x12`/`0x13` = `0x70`, and spent
about a week on the SPI side before a **cold power cycle** cleared it: both SPI modes, 100 kHz to
4 MHz, dummy bytes, both SPI peripherals, a hand-rolled driver and SensorLib, LilyGO's own example,
a blind `CTRL1 = 0x20`, and an ALDO2 power cycle. None of it helped, and the register dump did not
move one bit across a 40× change in clock rate.

**Cairn's bring-up had already produced that exact signature on purpose on 2026-10-25, and knows
what makes it** ([firmware.md](firmware.md#what-bring-up-found)): writing `CTRL1` whole with bit 5
clear. The handoff document infers "a stray write while `IMU_CS` was low during SD traffic" and
labels it inference; this repository has the controlled experiment. Both agree on the only remedy —
USB out *and* the 18650 out — and on why: the part is on VDD3V3, which no software here can drop
and which stays up from whichever supply is still connected.

What Cairn took from it, in code:

- `imu::WHO_AM_I_LATCHED` names the byte, and `whoami_hint` carries the remedy into the log, so a
  failure reads *cold power cycle*, not *unexpected WHO_AM_I*.
- The status line repeats the verdict. `tbeam-fw` learned the same lesson the same week: on native
  USB a reset drops the port, so a report printed once from `setup()` produced an **empty capture**.
- **INT 33 is the alive test**, and `tbeam-fw` never ran it usefully. Its own notes had the pin
  source-checked from LilyGO's `utilities.h` the whole time; the one attempt that was made used an
  unconfirmed pin, saw nothing, and was discounted. Cairn runs it on the failure path — and on a
  healthy part in verbose builds, so the test has been watched passing before anything rests on its
  silence.
- `|a| ≈ 1 g` is what made the recovery believable: 1.06 g at rest, seven distinct triples across
  29 samples, 2972 reads with no failures. The firmware now reports |a| in its status line, and
  `trail verify --stationary` already fails a session above 1.5 g at rest.

### The rest, worth having

- **The vendor's rail table cost the most time of anything in that investigation.** It labels ALDO2
  "Sensor" and never names the IMU, so a power-cycle test was run on a rail the IMU is not
  connected to and counted as evidence for days. [hardware.md](hardware.md#power-rails) names every
  rail by its load for this reason; so does `pmu::ldo`.
- **A GNSS fix has been had on this board**: outdoors in a car, 12 satellites, tracked to 41 mph,
  and a second fix with 10 satellites the same evening. Indoors it reports no fix with sentences
  flowing normally. The receiver and the antenna are therefore proven; Cairn's own fix path is
  still [not proven](firmware.md#how-v010-was-verified) on hardware.
- **`--flash-size keep`, never a literal size.** esptool patches whatever it is given into the
  bootloader's flash-size header: `16MB` on an 8 MB part made LittleFS fail to mount and silently
  reformatted a filesystem. Cairn flashes with `espflash`, and its images are saved for an 8 MB
  part by `scripts/ci.sh`; the trap belongs to anyone reaching for esptool on this board.
- **Erase and write in one esptool invocation.** A separate `erase-flash` resets the chip out of
  download mode, and the write that follows fails with "Invalid head of packet" — leaving the flash
  erased and unwritten.
- **The SD card must be FAT32 with an MBR**, which is not what macOS Disk Utility produces by
  default. Cairn's unit has been running a GUID-partitioned card ([hardware.md](hardware.md#read-off-the-unit))
  because `PartView` reads the partition table itself; Arduino's driver cannot, so a card shared
  between the two firmwares has to be MBR.
- **`tbeam-fw` writes ten log streams into `/tbeam/`** on the card. Cairn's sessions live in their
  own directories, which is what keeps the two from fighting over one card.
- **The bezel overhangs the glass** by 5 px left and right, 4 px top and bottom. Cairn's screen is
  off by default, but any layout that reaches the edge of the 128×64 panel is partly behind plastic.
