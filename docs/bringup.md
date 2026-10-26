# Bring-up

How the first Cairn firmware meets the board: in stages, each a small `examples/` binary with
`defmt` logs, each leaving a dated evidence file with the real output. Nothing here has run yet.
This is a procedure, not a schedule — the schedule is
[T1 in the Cairn roadmap](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md#t1t8--the-trail-device--in-progress).

The staging and evidence habit come from SoloTrail and LilyGO's factory test
([prior-art.md §8](prior-art.md#8-bring-up-method)). The facts each stage checks are in
[hardware.md](hardware.md).

## Rules for every stage

- **ALDO3 is never enabled**, and the LoRa pins stay inputs. The radio is off
  ([hardware.md](hardware.md#power-rails)).
- **Read-only first.** A stage reads and prints before it writes anything to a chip.
- **No eFuse is burned** at any stage. Secure Boot and flash encryption wait for T8 and a second
  board.
- **The antenna stays fitted**, radio off or not.
- **Evidence or it did not happen.** Each stage writes `evidence/<date>-<stage>.md`: the commit, the
  command, and the unedited output.

## Stage 0 — keep a way back

Before anything is flashed, the stock Meshtastic image is backed up, so the board can always be
returned to exactly what arrived.

1. Hold **BOOT**, tap **RST**, release BOOT. The stock firmware uses TinyUSB, so the serial port
   cannot reset the chip itself.
2. `esptool --port /dev/cu.usbmodem… read-flash 0 ALL ~/cairn-backups/tbeam-<mac>-<date>.bin`
   (mode 0600), then record its SHA-256 in the evidence file.
3. `esptool … chip-id`, `flash-id` and an eFuse **summary** (read only) into the evidence file.
4. Know the restore: `write-flash 0x0 <backup>`. The owner's analysis bundle also holds LilyGO's
   factory test image and Meshtastic 2.7.10, 2.7.26 and 2.8.1, with checksums.

After the first `esp-hal` image is on the board, it speaks USB-Serial-JTAG and `espflash` resets
the chip without the buttons.

## Stage 1 — the PMU

Nothing else answers until this works.

- Talk to the AXP2101 on I²C bus 1 (SDA 42, SCL 41) at `0x34`; check `0x03` reads `0x4A`.
- **Print every rail's state and voltage as shipped** (`0x80`, `0x82`–`0x86`, `0x90`, `0x92`–`0x97`)
  before changing any.
- Disable TS measurement; clear and disable all IRQs; set charging to 500 mA / 4.2 V.
- Enable only what the next stages need: ALDO1, ALDO2, ALDO4, BLDO1. **Not ALDO3.** Turn off
  DCDC2–5, BLDO2, DLDO1/2.
- Read VBAT, VBUS, VSYS, die temperature, battery-present, charge state, the fuel gauge; read the
  power-key timing register `0x27`.
- Pass: the rail table printed, and a second read after the changes showing exactly the rails
  intended.

## Stage 2 — the I²C buses

- Bus 1: AXP2101 `0x34`, PCF8563 `0x51`.
- Bus 0 (with ALDO1 on): scan, then identify — the magnetometer by chip ID (`0x7C` QMC6309 → V3.1
  carrier; `0x1C` QMC6310U → V3.0; `0x3C` answering `0x80` → QMC6310N), the OLED at `0x3C` or
  `0x3D`, the BME280 at `0x77` or `0x76` (`0xD0` reads `0x60`).
- PCF8563: read the time and **the voltage-low flag** before writing anything.
- Draw one frame on the SH1106.
- Pass: every expected device found by identity, not just by an ACK.

## Stage 3 — the shared SPI bus

- Drive SD CS (47) and IMU CS (34) high before either is initialised.
- QMI8658: WHO_AM_I `0x05`, soft reset, accelerometer and gyroscope readings; then the pedometer
  and wake-on-motion configuration, with the interrupt on GPIO 33 seen toggling.
- SD (BLDO1 on): mount, write a file, read it back — alternating with IMU reads in the same loop,
  so the bus sharing is proven under load, not assumed.
- Pass: thousands of interleaved IMU reads and card writes with no error and no corruption.

## Stage 4 — the GNSS receiver

- ALDO4 on; open the UART at 9600; wait up to 5 s for the first bytes rather than a fixed delay.
- UBX-MON-VER: confirm the MAX-M10S (`000A0000`) and record its firmware version.
- **Read the receiver's battery-backed configuration before changing it** (UBX-CFG-VALGET), so the
  evidence shows what Meshtastic left — then clear that layer once.
- Configure the RAM layer with UBX-CFG-VALSET: UBX-NAV-PVT on UART1, NMEA off, pedestrian dynamic
  model, the default constellations, 1 Hz; raise the baud rate.
- Fuzz the NAV-PVT parser on the host first: every split point of a frame, bad checksums, a
  receiver reset banner mid-stream.
- Pass: a fix with the receiver's own horizontal and vertical accuracy, and PPS seen on GPIO 6.

## Stage 5 — the radio held off, measured

- Confirm from register `0x90` that ALDO3 is off, and stays off across every stage and a reboot.
  ALDO3 is not enabled even to measure what keeping it off saves.
- With a meter in series with the cell: the board's current with every stage's rails on; with the
  display off; with GNSS off.
- Deep sleep: rails cut, pins open-drain, PMU IRQs cleared, wake on BOOT. Compare against LilyGO's
  ~1.45 mA.
- Pass: the numbers written into [hardware.md](hardware.md#read-off-the-unit).

## Stage 6 — everything at once

All of the above in one binary, with a bus scan, a rail table and a pass/fail summary on the
display — LilyGO's factory-test shape. This is the `bringup` build T1 closes on.
