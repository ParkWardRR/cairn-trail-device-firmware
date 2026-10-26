# Hardware: LILYGO T-Beam Supreme

What is known about the board, and how it is known. Three grades of source, kept apart:

- **Schematic** — LilyGO's published schematics: the T-Beam-S3-Core module (V3.0, dated
  2026-05-13) and the two carrier boards it plugs into, T-Beam-S3-Supreme V3.0 and V3.1.
- **Vendor** — LilyGO's hardware page and its MIT-licensed board-support code.
- **The unit** — read off the board on the owner's desk. Until bring-up
  ([bringup.md](bringup.md)) runs, only what its stock firmware reported is in this grade.

Where two sources disagree, both are written down and the schematic wins until the board
settles it. What the other firmware for this board taught is in [prior-art.md](prior-art.md).

## The board as it was received

Read on 2026-10-25 from the stock firmware over USB, before anything was flashed:

| Fact | Value | Grade |
|---|---|---|
| USB identity | `303a:1001` "LilyGo TBeam_S3_Core", Espressif, serial `48CA435BED60` | the unit |
| MAC | `48:ca:43:5b:ed:60` | the unit |
| Firmware | Meshtastic **2.7.10.94d4bdf** (2025-09-24), `tbeam-s3-core` build | the unit |
| LoRa | **SX1262**, almost certainly: that Meshtastic build compiles only SX1262/SX1268 drivers for this board and the radio started (non-zero airtime). LilyGO also sells an LR1121 version | inferred |
| Battery | Meshtastic reported level 101 and −0.001 V, which in Meshtastic means no cell detected: **probably no 18650 fitted** | inferred |
| GNSS | Enabled but powered off in Meshtastic's settings; no fix recorded | the unit |
| USB stack | Meshtastic's TinyUSB CDC, so DTR/RTS cannot reset the chip. To reach the ROM bootloader: **hold BOOT, tap RST** | the unit |

## The board is two boards

A **T-Beam-S3-Core** module — ESP32-S3, the AXP2101 PMU, the GNSS receiver and the LoRa module —
plugs into the **Supreme carrier** through an M.2 B-key connector. The carrier holds the display,
the I²C sensors, the IMU, the RTC, the SD slot, the buttons, USB-C and the battery holder.

## Parts

| Part | Chip | Where | Grade |
|---|---|---|---|
| MCU | ESP32-S3FN8, dual-core Xtensa LX7; 8 MB flash, 8 MB PSRAM | core | schematic, vendor |
| PMU | X-Powers AXP2101, I²C `0x34` | core | schematic |
| GNSS | u-blox MAX-M10S (the footprint also takes a Quectel L76K) | core | vendor; schematic shows the L76K footprint |
| LoRa | HPD16A module (SX1262), 915 MHz — **held off** | core | schematic |
| Display | SH1106, 1.3", 128×64, I²C `0x3C` or `0x3D` (set by a resistor) | carrier | schematic |
| IMU | QMI8658A, 6-axis, on SPI (an MPU-6886 footprint is unpopulated beside it) | carrier | schematic |
| Magnetometer | V3.0: QMC6310U at `0x1C`. V3.1: QMC6309 at `0x7C`. A QMC6310N at `0x3C` also exists | carrier | schematic, vendor |
| Environment | Bosch BME280 — pressure, temperature, humidity — I²C `0x77` (`0x76` with a resistor moved) | carrier | schematic, vendor |
| RTC | NXP PCF8563, I²C `0x51`, with its **own backup cell** through a diode, and a 32.768 kHz crystal | carrier | schematic |
| Storage | microSD, on the shared SPI bus | carrier | schematic |
| Battery | one 18650, with a DW06D + AO3400 protection circuit (on **both** carrier revisions' schematics, although the vendor lists it as new in V3.1) | carrier | schematic |
| Buttons | RST (CHIP_PU), BOOT (GPIO 0, 100 k pull-up), PWR (the PMU's power key) | both | schematic |
| Slow clock | 32.768 kHz crystal on GPIO 15/16, for the ESP32-S3's RTC domain | core | vendor |

## Power rails

From the schematics, cross-checked against LilyGO's, MeshCore's and Meshtastic's init code. **This
table is what the PMU driver encodes**, and it names rails by their load.

| AXP2101 rail | Feeds | Voltage | Cairn's use |
|---|---|---|---|
| DCDC1 | VDD3V3: the ESP32-S3, the **IMU**, the **RTC**, the PMU's I²C pull-ups | 3.3 V | Always on |
| DCDC2 | Nothing (not connected) | — | Off |
| DCDC3, DCDC4, DCDC5 | The M.2 socket and expansion headers only | — | **Off** — nothing of ours is there |
| ALDO1 | OLED, BME280, magnetometer, and the **pull-ups of I²C bus 0** | 3.3 V | On while any of them is in use. With ALDO1 off, bus 0 has no pull-ups and hangs |
| ALDO2 | The expansion header; vendor and Meshtastic say "sensors, and must stay on"; on V3.0 a 0 Ω link may join it to ALDO1 | 3.3 V | **On** until the board shows it can go off |
| **ALDO3** | **The LoRa module, and nothing else** | — | **Never enabled.** The radio is off |
| ALDO4 | GNSS VCC (powers on at 2.9 V by default) | 3.3 V | On while recording; the receiver's hard off |
| BLDO1 | microSD (and, on V3.0, the SD level shifters) | 3.3 V | On while the card is in use |
| BLDO2 | Expansion header | — | Off |
| VRTC | GNSS backup supply (V_BCKP) and the PMU IRQ pull-up; alive whenever the cell is | — | Not switchable; why hot start needs the 18650 |
| VBACKUP, DLDO1/2, CPUSLDO | Nothing | — | Off |

Disagreements to settle on the board:

- **IMU and RTC power.** Both carrier schematics put the QMI8658 and the PCF8563 on VDD3V3. LilyGO's
  doc and RNode say ALDO1 feeds the IMU; RNode and Meshtastic say ALDO2 feeds the RTC. If the
  schematics are right, the IMU is always powered — useful for wake-on-motion, a cost to manage
  with its own power-down mode — and ALDO2 can probably go off.
- **Power-off hold time.** LilyGO's code sets 4 s; its doc and SoftRF's wiki say 6 s. Read register
  `0x27`.

**Turning the LoRa radio off, exactly:** ALDO3 is never enabled, and the radio's pins — GPIO 1
(DIO1), 2 (RADIO_RETAIN), 4 (BUSY), 5 (RESET), 10 (CS), 11 (MOSI), 12 (SCK), 13 (MISO) — stay inputs,
so nothing feeds the unpowered chip through them. No SPI traffic and no sleep command.

## Pins

| Function | GPIO |
|---|---|
| I²C bus 0 — OLED, magnetometer, BME280 | SDA 17, SCL 18 |
| I²C bus 1 — PMU, RTC | SDA 42, SCL 41 |
| Shared SPI — SD card, IMU | MOSI 35, MISO 37, SCK 36 |
| IMU | CS 34, INT 33 (through diodes) |
| SD card | CS 47 |
| GNSS UART | ESP TX 8 → receiver RX; receiver TX → ESP RX 9 |
| GNSS PPS | 6 (through a diode; also lights a PPS LED that cannot be switched off) |
| GNSS wake | 7 — **only does anything on the L76K variant** |
| LoRa (unused) | DIO1 1, RETAIN 2, BUSY 4, RESET 5, CS 10, MOSI 11, SCK 12, MISO 13 |
| RTC interrupt | 14 |
| PMU interrupt | 40 (pulled up to VRTC) |
| BOOT button | 0 |
| 32.768 kHz crystal | 15, 16 |
| USB | 19, 20 (native USB to the USB-C port; no USB-serial bridge) |
| UART0 on the 4-pin connector | TX 43, RX 44 |

## Chip identities and first contact

Facts our drivers check, from the MIT-licensed XPowersLib and SensorLib register maps:

| Chip | Identity | Notes |
|---|---|---|
| AXP2101 | register `0x03` reads `0x4A` | Disable TS measurement (no thermistor fitted); clear IRQ status by writing `0xFF` to `0x48`–`0x4A` |
| QMI8658 | WHO_AM_I `0x00` reads `0x05` | Soft reset: write `0xB0` to `0x60`, poll `0x4D` for `0x80`. Over SPI, bit 7 of the address marks a read |
| QMC6310 | `0x00` reads `0x80` | Same address as the OLED on the N variant: read `0x00` at `0x3C` before deciding which is there |
| QMC6309 | `0x00` reads `0x90` | |
| BME280 | `0xD0` reads `0x60` | Probe `0x77`, then `0x76` |
| PCF8563 | no ID register | Check the voltage-low flag before trusting the time |
| MAX-M10S | UBX-MON-VER hardware version `000A0000` | Ships at 9600 baud; configure with UBX-CFG-VALSET only |

**Telling the carrier revision apart:** the magnetometer — `0x7C` is V3.1, `0x1C` is V3.0, and
`0x3C` answering `0x80` is a QMC6310N board.

## AXP2101 registers the driver needs

From XPowersLib (MIT), `src/REG/AXP2101Constants.h` and `src/XPowersAXP2101.tpp`:

| Register | Use |
|---|---|
| `0x00` | Status 1: bit 5 VBUS good, bit 3 battery present |
| `0x01` | Status 2: bits 7:5 charge direction (1 charging, 2 discharging, 0 standby); bits 2:0 charger state |
| `0x10` | bit 0 power off, bit 1 soft reset |
| `0x18` | bit 3 fuel gauge enable; bit 2 VBACKUP charge enable (not connected here) |
| `0x1A` | Low-battery warning % (bits 7:4, value + 5) and shutdown % (bits 3:0) |
| `0x22` | Long-press behaviour: bit 1 enable, bit 0 restart (1) or power off (0) |
| `0x24` | System shutdown voltage, bits 2:0 = (mV − 2600) / 100 |
| `0x27` | Power key: bits 5:4 long-press IRQ time (1–2.5 s), bits 3:2 power-off hold (4/6/8/10 s), bits 1:0 power-on press (128 ms–2 s) |
| `0x30` | ADC enables: bit 0 VBAT, 1 TS, 2 VBUS, 3 VSYS, 4 die temperature |
| `0x34`/`0x35` | VBAT in mV (14 bits); VBUS at `0x38`/`0x39`, VSYS at `0x3A`/`0x3B`; die temperature at `0x3C`/`0x3D` |
| `0x40`–`0x42` / `0x48`–`0x4A` | IRQ enable / status. In the second byte: bit 3 PWR short press, bit 2 long press, bits 4/5 battery removed/inserted, bits 6/7 VBUS removed/inserted. Third byte: bit 3 charge start, bit 4 charge done |
| `0x61`–`0x64` | Charging: pre-charge, constant current (`0x62`: 11 = 500 mA), termination, target voltage (`0x64`: 3 = 4.2 V) |
| `0x80` | DCDC1–5 enable, bits 0–4 |
| `0x82`–`0x86` | DCDC voltages |
| `0x90` | ALDO1–4, BLDO1, BLDO2, CPUSLDO, DLDO1 enable, bits 0–7 |
| `0x92`–`0x97` | ALDO1–4, BLDO1–2 voltages: (mV − 500) / 100, so 3.3 V is `0x1C` |
| `0xA4` | Battery percentage from the fuel gauge |

## Things that shape the firmware

- **The PMU powers everything.** Nothing answers until its rail is on, and a "sensor absent" result
  before the rails are known is meaningless. Bring-up starts at the PMU.
- **The SD card and the IMU share one SPI bus.** One owner, behind one mutex; drive SD CS high
  before talking to the IMU, and the IMU's CS high before initialising the card. On V3.1 the SD
  lines have no level shifters, so an unpowered card still sits on the bus.
- **I²C bus 0 depends on ALDO1 for its pull-ups.** Turning ALDO1 off to save power turns off the
  display and both I²C sensors together, and the bus must not be touched until it is back.
- **Deep sleep on this board measures about 1.45 mA** (LilyGO's figure) — about three times what
  LilyGO's smaller boards manage. Real savings come from cutting rails, not from sleeping the CPU.
- **Wake sources.** The S3's RTC-capable GPIOs are 0–21: BOOT (0) and the RTC interrupt (14) can
  wake it from deep sleep; the IMU interrupt (33) and the PMU interrupt (40) cannot. The PMU
  interrupt also misfires with no battery or a full one, so it is read while awake, never used to
  wake.
- **GNSS hot start needs the 18650**, through VRTC. Without a cell, every power-up is a cold start,
  and the receiver's battery-backed configuration is gone too.
- **The receiver may still hold another firmware's settings.** Cairn sends its configuration to the
  receiver's RAM layer at every boot and clears the battery-backed layer once.
- **This silicon supports Secure Boot V2 and flash encryption**, unlike the dongle's rev v1.0
  ESP32. Neither is burned on a unit that matters before a sacrificial unit has been through it.

## Read off the unit

Filled in by bring-up. Rows already known are from the stock firmware, above.

| Fact | Value | How it was read | Date |
|---|---|---|---|
| MAC, USB serial | `48:ca:43:5b:ed:60`, `48CA435BED60` | stock firmware, USB descriptor | 2026-10-25 |
| Stock firmware | Meshtastic 2.7.10.94d4bdf | `meshtastic --info` | 2026-10-25 |
| Full flash backup | — | `esptool read-flash 0 ALL` | — |
| Carrier revision | — | silkscreen, magnetometer address | — |
| Chip revision, flash and PSRAM | — | `esptool chip-id`, `flash-id`, boot log | — |
| GNSS module and firmware | — | UBX-MON-VER | — |
| LoRa module | — (SX1262 inferred) | module label | — |
| Magnetometer variant | — | I²C probe and chip ID | — |
| OLED and BME280 addresses | — | I²C probe | — |
| AXP2101 rail states as shipped | — | registers `0x80`, `0x90`–`0x97` | — |
| What feeds the IMU and the RTC | — | toggle ALDO1/ALDO2, probe | — |
| Power-off hold time | — | register `0x27` | — |
| RTC voltage-low flag on first power | — | PCF8563 | — |
| 18650 fitted, capacity | — (probably none) | PMU status `0x00` bit 3, label | — |
| Board current, radio held off | — | meter on the cell | — |
| Board current, deep sleep | — (vendor: ~1.45 mA) | meter on the cell | — |

Source: LilyGO's [LilyGo-LoRa-Series](https://github.com/Xinyuan-LilyGO/LilyGo-LoRa-Series)
(schematics under `schematic/T-Beam-S3-Supreme/`, hardware page under
`docs/en/t_beam_supreme/`), read at `1991b1e`.
