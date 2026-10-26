# Hardware: LILYGO T-Beam Supreme

**Every row here is from the vendor's documentation and has not been checked on a real unit.**
Bring-up ([roadmap T1](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/blob/main/ROADMAP.md#t1t8--the-trail-device--designed))
replaces each "vendor" with what the board itself reported, and fills in the *Read off the unit*
table at the end. LILYGO ships revisions with different parts — the magnetometer alone has three
variants — so the firmware probes rather than assumes.

Source: [LilyGo-LoRa-Series, T-Beam Supreme hardware](https://github.com/Xinyuan-LilyGO/LilyGo-LoRa-Series/blob/master/docs/en/t_beam_supreme/t_beam_supreme_hw.md).

## Parts

| Part | Chip | Source |
|---|---|---|
| MCU | ESP32-S3FN8 (dual-core Xtensa LX7), 8 MB quad-SPI flash, 8 MB quad-SPI PSRAM | vendor |
| GNSS | u-blox MAX-M10S | vendor (order option; the other is a Quectel L76K) |
| LoRa | Semtech SX1262, 915 MHz | vendor (order option; the other is an LR1121) |
| Display | SH1106, 1.3", 128×64 | vendor |
| IMU | QMI8658, 6-axis, on SPI | vendor |
| Magnetometer | QMC6310U, QMC6310N or QMC6309 (V3.1 boards use QMC6309) | vendor |
| Environment | Bosch BME280 — pressure, temperature, humidity | vendor |
| RTC | NXP PCF8563 | vendor |
| PMU | X-Powers AXP2101 | vendor |
| Storage | microSD, on the shared SPI bus | vendor |
| Power | USB-C, 3.9–6 V in; charge current programmable 0–1024 mA; one 18650 cell, which also backs the GNSS and RTC. V3.1 adds an onboard BMS | vendor |
| Buttons | PWR (the PMU's key), BOOT (GPIO 0), RST | vendor |

## Pins

| Function | Signal | GPIO |
|---|---|---|
| I²C bus 0 — OLED, magnetometer, BME280 | SDA / SCL | 17 / 18 |
| I²C bus 1 — PMU, RTC | SDA / SCL | 42 / 41 |
| Shared SPI — SD card, IMU | MOSI / MISO / SCK | 35 / 37 / 36 |
| IMU | CS / INT | 34 / 33 |
| SD card | CS | 47 |
| LoRa SPI (dedicated) | SCK / MISO / MOSI / CS | 12 / 13 / 11 / 10 |
| LoRa | RESET / BUSY / DIO1 | 5 / 4 / 1 |
| GNSS UART | TX / RX | 8 / 9 |
| GNSS | PPS | 6 |
| RTC | INT | 14 |
| PMU | IRQ | 40 |
| BOOT button | — | 0 |
| Free UART1 (QWIIC) | TX / RX | 43 / 44 |

The bus each I²C device sits on is inferred from the vendor's address table; it is the first
thing T1 confirms.

## I²C addresses

| Bus | Device | Address |
|---|---|---|
| 0 | SH1106 OLED | `0x3C` (or `0x3D`) |
| 0 | QMC6310U / QMC6310N | `0x1C` / `0x3C` — the N variant collides with the OLED's default |
| 0 | QMC6309 | `0x7C` |
| 0 | BME280 | `0x77` (`0x76` with the resistor moved) |
| 1 | AXP2101 | `0x34` |
| 1 | PCF8563 | `0x51` |

## Things that shape the firmware

- **The PMU powers everything.** GNSS, LoRa, SD and the sensors sit on AXP2101 rails, and none of
  them answers until the PMU has enabled its rail. Bring-up starts with the PMU, and a "sensor
  absent" result before the rails are known to be on means nothing.
- **The SD card and the IMU share one SPI bus.** One owner task or one mutex, proven under load.
  The dongle lost a week of drives to two tasks sharing a serial link unlocked
  ([dongle PR #54](https://github.com/ParkWardRR/cairn-esp32-device-firmware/pull/54)).
- **Wake from deep sleep.** The S3's RTC-capable GPIOs are 0–21. The IMU interrupt (33) and the
  PMU IRQ (40) cannot wake the chip from deep sleep; the RTC interrupt (14) and BOOT (0) can. To
  be confirmed on the board.
- **The QMC6310N's address may collide with the OLED's.** If the unit has that part, one of them
  moves or the magnetometer is read on its alternate address.
- **This silicon supports Secure Boot V2 and flash encryption**, unlike the dongle's rev v1.0
  ESP32. Neither is burned on a unit that matters before a sacrificial unit has been through the
  whole procedure.

## Read off the unit

Filled in by T1. Until then, nothing here is known.

| Fact | Value | How it was read | Date |
|---|---|---|---|
| Board revision | — | silkscreen | — |
| Chip revision, MAC | — | `esptool.py chip_id` | — |
| Flash / PSRAM size | — | `esptool.py flash_id`, boot log | — |
| GNSS module and firmware | — | `UBX-MON-VER` | — |
| Magnetometer variant | — | I²C probe and chip-id register | — |
| BME280 address | — | I²C probe | — |
| PMU rails powering each peripheral | — | AXP2101 registers | — |
| RTC voltage-low flag on first power | — | PCF8563 `VL` bit | — |
| 18650 fitted, capacity | — | label | — |
