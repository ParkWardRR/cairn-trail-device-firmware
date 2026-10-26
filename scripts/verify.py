#!/usr/bin/env python3
"""Check a session pulled off the card: every file well formed, rows in time
order, each stream at its expected rate, values physically plausible.

    scripts/verify.py pulled/            # files named GNSS.CSV, IMU.CSV, ...

Exit status 1 if any check fails. Stationary-bench expectations (the IMU
reads ~1 g) are checked only with --stationary.
"""
import argparse
import csv
import math
import os
import sys

# (file, expected rows per second, tolerance as a fraction)
RATES = {
    "IMU.CSV": (50.0, 0.15),
    "BARO.CSV": (4.0, 0.15),
    "MAG.CSV": (10.0, 0.25),
    "GNSS.CSV": (1.0, 0.10),
    "POWER.CSV": (0.2, 0.25),
    "PPS.CSV": (1.0, 0.10),
}

failures = []


def check(cond, msg):
    print(("  ok    " if cond else "  FAIL  ") + msg)
    if not cond:
        failures.append(msg)


def load(path):
    with open(path, newline="") as f:
        rows = list(csv.reader(f))
    return rows[0], rows[1:]


def find(d, name):
    for f in os.listdir(d):
        if f.upper().endswith(name):
            return os.path.join(d, f)
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--stationary", action="store_true")
    a = ap.parse_args()

    for name in ["GNSS.CSV", "SAT.CSV", "IMU.CSV", "BARO.CSV", "MAG.CSV", "POWER.CSV", "EVENTS.CSV", "PPS.CSV"]:
        path = find(a.dir, name)
        print(f"\n{name}")
        if not path:
            check(False, f"{name} present")
            continue
        header, rows = load(path)
        check(header[0] == "t_ms", f"header starts with t_ms ({len(header)} columns)")
        if name == "PPS.CSV" and not rows:
            # The receiver emits a time pulse only once it has a fix.
            gnss = find(a.dir, "GNSS.CSV")
            had_fix = gnss and any(r[6] == "1" for r in load(gnss)[1])
            check(not had_fix, "no PPS rows, and no GNSS fix to cause any")
            continue
        check(len(rows) > 0, f"{len(rows)} rows")
        if not rows:
            continue
        bad = [i for i, r in enumerate(rows) if len(r) != len(header)]
        check(not bad, f"every row has {len(header)} columns" + (f" (first bad row {bad[0] + 2}: {rows[bad[0]]})" if bad else ""))
        t = [int(r[0]) for r in rows if r and r[0].isdigit()]
        back = sum(1 for x, y in zip(t, t[1:]) if y < x)
        check(back == 0, f"t_ms never goes backwards ({back} steps back)")
        span = (t[-1] - t[0]) / 1000.0 if len(t) > 1 else 0
        if name in RATES and span > 10:
            want, tol = RATES[name]
            got = (len(t) - 1) / span
            check(abs(got - want) <= want * tol, f"rate {got:.2f}/s over {span:.0f} s (expect {want}/s ±{tol:.0%})")
            gaps = [(y - x) for x, y in zip(t, t[1:])]
            print(f"        largest gap {max(gaps)} ms")

        col = {h: i for i, h in enumerate(header)}
        if name == "IMU.CSV":
            mags = [math.sqrt(sum(float(r[col[k]]) ** 2 for k in ("ax_mg", "ay_mg", "az_mg"))) for r in rows]
            mean = sum(mags) / len(mags)
            print(f"        |a| mean {mean:.0f} mg, min {min(mags):.0f}, max {max(mags):.0f}")
            if a.stationary:
                check(900 < mean < 1100, "stationary: |a| averages ~1 g")
            temps = [float(r[col["temp_c"]]) for r in rows]
            check(all(-20 < x < 70 for x in temps), "IMU temperature in -20..70 C")
            gyr = [abs(float(r[col[k]])) for r in rows for k in ("gx_dps", "gy_dps", "gz_dps")]
            if a.stationary:
                check(sorted(gyr)[len(gyr) // 2] < 5, "stationary: median |gyro| < 5 dps")
        if name == "BARO.CSV":
            p = [float(r[col["pressure_pa"]]) for r in rows]
            check(all(30000 < x < 110000 for x in p), f"pressure in 300..1100 hPa ({min(p) / 100:.1f}..{max(p) / 100:.1f})")
            e = [float(r[col["elevation_m"]]) for r in rows]
            print(f"        elevation {min(e):.1f}..{max(e):.1f} m, gain {rows[-1][col['gain_m']]} m, loss {rows[-1][col['loss_m']]} m")
            if a.stationary:
                check(max(e) - min(e) < 3, "stationary: elevation varies < 3 m")
            h = [float(r[col["humidity_pct"]]) for r in rows]
            check(all(0 <= x <= 100 for x in h), "humidity in 0..100 %")
        if name == "GNSS.CSV":
            fixes = [r for r in rows if r[col["fix_ok"]] == "1"]
            print(f"        {len(fixes)} of {len(rows)} epochs with a fix; max sats {max(int(r[col['num_sv']]) for r in rows)}")
            for r in fixes:
                lat, lon = float(r[col["lat"]]), float(r[col["lon"]])
                if not (-90 <= lat <= 90 and -180 <= lon <= 180):
                    check(False, f"fix with coordinates in range: {lat},{lon}")
                    break
        if name == "SAT.CSV":
            cno = [int(r[col["cno_dbhz"]]) for r in rows]
            print(f"        {len(rows)} satellite rows, best C/N0 {max(cno)} dB-Hz")
        if name == "MAG.CSV":
            check(any(int(r[col["mx"]]) != 0 for r in rows), "magnetometer reads non-zero")
        if name == "POWER.CSV":
            vbus = [int(r[col["vbus_mv"]]) for r in rows]
            print(f"        vbus {min(vbus)}..{max(vbus)} mV, battery={rows[-1][col['battery']]}")
            ldo = [int(r[col["ldo"]], 16) for r in rows]
            check(all(x & 0x04 == 0 for x in ldo), "ALDO3 (LoRa) off in every power row")
        if name == "EVENTS.CSV":
            names = {r[1] for r in rows if len(r) > 1}
            print(f"        events: {', '.join(sorted(names))}")
            for must in ("boot", "session", "pmu", "rails", "imu", "gnss_version"):
                check(must in names, f"event '{must}' recorded")

    print(f"\n{'FAILED: ' + str(len(failures)) if failures else 'all checks passed'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
