#!/usr/bin/env python3
"""Talk to the trail device over its USB serial console.

Captures everything to a log, can reset the chip first, sends console
commands, and pulls files off the SD card with `cat`, checking each one
against the CRC-32 the firmware prints after it.

    scripts/console.py --reset --seconds 60 --log evidence/boot.log
    scripts/console.py --cmd stat --cmd ls --seconds 5
    scripts/console.py --pull GNSS.CSV --pull BARO.CSV --out pulled/
    scripts/console.py --session S0009 --out pulled/S0009

Needs pyserial. Homebrew's esptool ships one:
    $(ls /opt/homebrew/Cellar/esptool/*/libexec/bin/python | head -1) scripts/console.py ...
"""
import argparse
import glob
import os
import re
import sys
import time
import zlib

import serial


SESSION_FILES = ["INFO.TXT", "EVENTS.CSV", "GNSS.CSV", "SAT.CSV", "BARO.CSV",
                 "MAG.CSV", "POWER.CSV", "PPS.CSV", "IMU.CSV"]


def find_port(explicit):
    if explicit:
        return explicit
    ports = sorted(glob.glob("/dev/cu.usbmodem*"))
    if not ports:
        sys.exit("no /dev/cu.usbmodem* port")
    return ports[0]


def open_port(name, wait=10.0):
    deadline = time.time() + wait
    while True:
        try:
            port = find_port(name)
            s = serial.Serial(port, 115200, timeout=0.1)
            return s
        except (serial.SerialException, SystemExit, OSError):
            if time.time() > deadline:
                raise
            time.sleep(0.2)


def usb_jtag_reset(s):
    """Reset into the application through the USB-Serial-JTAG's DTR/RTS."""
    s.dtr = False
    s.rts = True
    time.sleep(0.1)
    s.rts = False
    time.sleep(0.05)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port")
    ap.add_argument("--reset", action="store_true", help="reset the chip before capturing")
    ap.add_argument("--seconds", type=float, default=10.0, help="capture time after the last command")
    ap.add_argument("--wait", type=float, default=0.0, help="seconds before sending commands")
    ap.add_argument("--cmd", action="append", default=[], help="console command (repeatable)")
    ap.add_argument("--pull", action="append", default=[], help="file to `cat` and save (repeatable)")
    ap.add_argument("--session", help="pull every file of this session, e.g. S0009")
    ap.add_argument("--out", default=".", help="directory for pulled files")
    ap.add_argument("--log", help="append everything received to this file")
    ap.add_argument("--quiet", action="store_true")
    a = ap.parse_args()

    if a.session:
        a.pull += [f"{a.session}/{f}" for f in SESSION_FILES]

    s = open_port(a.port)
    if a.reset:
        usb_jtag_reset(s)
        s.close()
        time.sleep(0.3)
        s = open_port(a.port)

    log = open(a.log, "ab") if a.log else None
    buf = bytearray()
    pending = list(a.cmd) + [f"cat {p}" for p in a.pull]
    start = time.time()
    last_cmd_time = start
    next_cmd_at = start + a.wait
    end_at = None
    waiting_end = None
    waiting_deadline = 0.0

    while True:
        now = time.time()
        if waiting_end is not None:
            # A `cat` is streaming: send nothing until its END marker arrives.
            if buf.count(b"----- END ") > waiting_end or now > waiting_deadline:
                waiting_end = None
                next_cmd_at = now + 0.3
        if pending and waiting_end is None and now >= next_cmd_at:
            cmd = pending.pop(0)
            s.write((cmd + "\n").encode())
            last_cmd_time = now
            if cmd.startswith("cat"):
                waiting_end = buf.count(b"----- END ")
                waiting_deadline = now + 120
            else:
                next_cmd_at = now + 0.7
        if not pending and waiting_end is None and end_at is None and now >= next_cmd_at:
            end_at = max(last_cmd_time, start) + a.seconds
        if end_at is not None and now >= end_at:
            break
        try:
            chunk = s.read(4096)
        except (serial.SerialException, OSError):
            s.close()
            s = open_port(a.port)
            continue
        if chunk:
            buf.extend(chunk)
            if log:
                log.write(chunk)
                log.flush()
            if not a.quiet:
                sys.stdout.write(chunk.decode("utf-8", "replace"))
                sys.stdout.flush()
            # Keep waiting while a `cat` is still streaming.
            if end_at is not None and b"----- BEGIN" in buf[-200000:] and buf.rfind(b"----- END") < buf.rfind(b"----- BEGIN"):
                end_at = time.time() + 2.0

    failures = 0
    text = bytes(buf)
    for m in re.finditer(rb"----- BEGIN (\S+) (\d+) bytes -----\r?\n(.*?)\r?\n----- END \S+ (\d+) bytes crc32=([0-9a-f]{8}) -----", text, re.S):
        name, declared, body, total, crc = m.group(1).decode(), int(m.group(2)), m.group(3), int(m.group(4)), m.group(5).decode()
        got = f"{zlib.crc32(body) & 0xFFFFFFFF:08x}"
        ok = got == crc and len(body) == total
        failures += 0 if ok else 1
        os.makedirs(a.out, exist_ok=True)
        dest = os.path.join(a.out, name.replace("/", "_"))
        with open(dest, "wb") as f:
            f.write(body)
        print(f"\n[pull] {name}: {len(body)} bytes (declared {declared}, streamed {total}) crc32 {got} vs {crc} -> {'OK' if ok else 'MISMATCH'} -> {dest}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
