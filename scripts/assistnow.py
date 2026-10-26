#!/usr/bin/env python3
"""u-blox AssistNow for the trail logger: register the board's GNSS receiver
with Thingstream, download assistance data, and put it on the SD card, so the
receiver gets a fix in seconds instead of minutes — even on a trail with no
signal, since the data is loaded from the card at boot.

    scripts/assistnow.py identity                  # read the receiver's identity over USB
    scripts/assistnow.py register                  # Thingstream ZTP -> chipcode (once per board)
    scripts/assistnow.py fetch                     # download assistance data to assist/ASSIST.UBX
    scripts/assistnow.py install /Volumes/GPS      # copy it to the card's root

Credentials are read from ~/.config/cairn/thingstream.env (mode 0600, never in
this repository):

    ASSISTNOW_ZTP_URL=https://api.thingstream.io/ztp/assistnow/credentials
    ASSISTNOW_ZTP_TOKEN=<the ZTP token from your Thingstream device profile>

What registration returns (the chipcode, service URLs) is kept in
~/.config/cairn/assistnow.json, also 0600. The ZTP request format —
{"token": ..., "messages": {"UBX-SEC-UNIQID": hex, "UBX-MON-VER": hex}} — was
confirmed against the API's own validation errors; the download step follows
whatever the registration response names, so check its output on first use.
"""
import argparse
import glob
import json
import os
import shutil
import stat
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

CONF = os.path.expanduser("~/.config/cairn")
ENV = os.path.join(CONF, "thingstream.env")
IDENTITY = os.path.join(CONF, "gnss-identity.json")
CREDS = os.path.join(CONF, "assistnow.json")
SECRET_HINTS = ("chipcode", "token", "secret", "password", "key", "credential")


def load_env():
    if not os.path.exists(ENV):
        sys.exit(f"missing {ENV}")
    mode = stat.S_IMODE(os.stat(ENV).st_mode)
    if mode & 0o077:
        sys.exit(f"{ENV} is readable by others (mode {oct(mode)}); chmod 600 it")
    env = {}
    for line in open(ENV):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip()
    return env


def save_private(path, obj):
    os.makedirs(CONF, mode=0o700, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, indent=2)


def redact(obj):
    """Show a response's shape without its secrets."""
    if isinstance(obj, dict):
        return {k: ("<redacted>" if any(h in k.lower() for h in SECRET_HINTS) and isinstance(v, str) else redact(v))
                for k, v in obj.items()}
    if isinstance(obj, list):
        return [redact(v) for v in obj]
    return obj


def cmd_identity(a):
    import serial  # pyserial

    port = a.port or (sorted(glob.glob("/dev/cu.usbmodem*")) or [None])[0]
    if not port:
        sys.exit("no /dev/cu.usbmodem* port: is the board plugged in?")
    s = serial.Serial(port, 115200, timeout=0.2)
    s.reset_input_buffer()
    s.write(b"gnssid\n")
    buf = b""
    deadline = time.time() + 4
    while time.time() < deadline:
        buf += s.read(4096)
        if b"UBX-SEC-UNIQID" in buf and b"\n" in buf.split(b"UBX-SEC-UNIQID", 1)[1]:
            break
    ident = {}
    for line in buf.decode("ascii", "replace").splitlines():
        for name in ("UBX-MON-VER", "UBX-SEC-UNIQID"):
            if line.startswith(name + " "):
                ident[name] = line.split(" ", 1)[1].strip()
    if not ident.get("UBX-MON-VER") or not ident.get("UBX-SEC-UNIQID"):
        sys.exit(f"the board did not report both frames (firmware too old, or the receiver did not answer):\n{buf[-400:]!r}")
    for k, v in ident.items():
        if not (v.startswith("b562") and len(v) % 2 == 0):
            sys.exit(f"{k} is not a UBX frame: {v[:40]}")
    save_private(IDENTITY, ident)
    print(f"UBX-MON-VER    {len(ident['UBX-MON-VER']) // 2} bytes")
    print(f"UBX-SEC-UNIQID {ident['UBX-SEC-UNIQID']}")
    print(f"saved to {IDENTITY}")


def post_json(url, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def cmd_register(a):
    env = load_env()
    if not os.path.exists(IDENTITY):
        sys.exit("run `identity` first (the board must be plugged in)")
    ident = json.load(open(IDENTITY))
    if a.payload_only:
        # Strip sync, class, id, length and checksum: payload hex only.
        ident = {k: v[12:-4] for k, v in ident.items()}
    status, body = post_json(env["ASSISTNOW_ZTP_URL"], {"token": env["ASSISTNOW_ZTP_TOKEN"], "messages": ident})
    text = body.decode("utf-8", "replace")
    if status != 200:
        sys.exit(f"ZTP refused (HTTP {status}): {text[:500]}")
    resp = json.loads(text)
    save_private(CREDS, resp)
    print(f"registered; response saved to {CREDS} (0600). Its shape:")
    print(json.dumps(redact(resp), indent=2))


def find(obj, pred, path=""):
    """Every (path, value) in a JSON document whose key matches pred."""
    out = []
    if isinstance(obj, dict):
        for k, v in obj.items():
            p = f"{path}.{k}" if path else k
            if isinstance(v, (dict, list)):
                out += find(v, pred, p)
            elif pred(k.lower(), v):
                out.append((p, v))
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            out += find(v, pred, f"{path}[{i}]")
    return out


def cmd_fetch(a):
    if not os.path.exists(CREDS):
        sys.exit("run `register` first")
    creds = json.load(open(CREDS))
    chip = [v for _, v in find(creds, lambda k, v: "chipcode" in k and isinstance(v, str))]
    if not chip:
        sys.exit(f"no chipcode in {CREDS}; its shape is:\n{json.dumps(redact(creds), indent=2)}")
    url = a.url
    if not url:
        urls = [v for _, v in find(creds, lambda k, v: isinstance(v, str) and v.startswith("http"))]
        if not urls:
            sys.exit("the registration response names no service URL; pass --url (see the Thingstream device profile)")
        url = urls[0]
    params = {"chipcode": chip[0]}
    for kv in a.param:
        k, v = kv.split("=", 1)
        params[k] = v
    full = url + ("&" if "?" in url else "?") + urllib.parse.urlencode(params, safe=",;")
    shown = full.replace(chip[0], "<chipcode>")
    print(f"GET {shown}")
    try:
        with urllib.request.urlopen(full, timeout=60) as r:
            data = r.read()
    except urllib.error.HTTPError as e:
        sys.exit(f"HTTP {e.code}: {e.read()[:300]!r}")
    summary = summarize(data)
    if not summary:
        sys.exit(f"the reply is not UBX data ({len(data)} bytes): {data[:200]!r}")
    os.makedirs(os.path.dirname(a.out) or ".", exist_ok=True)
    open(a.out, "wb").write(data)
    print(f"{len(data)} bytes, {sum(summary.values())} UBX frames -> {a.out}")
    for (cls, mid), n in sorted(summary.items()):
        print(f"  class 0x{cls:02x} id 0x{mid:02x}: {n}")


def summarize(data):
    """Count well-formed UBX frames by (class, id); None if there are none."""
    counts, i = {}, 0
    while i + 8 <= len(data):
        if data[i] != 0xB5 or data[i + 1] != 0x62:
            i += 1
            continue
        ln = data[i + 4] | (data[i + 5] << 8)
        end = i + 6 + ln + 2
        if end > len(data):
            break
        a = b = 0
        for x in data[i + 2:i + 6 + ln]:
            a = (a + x) & 0xFF
            b = (b + a) & 0xFF
        if (a, b) == (data[end - 2], data[end - 1]):
            counts[(data[i + 2], data[i + 3])] = counts.get((data[i + 2], data[i + 3]), 0) + 1
            i = end
        else:
            i += 1
    return counts or None


def cmd_install(a):
    if not os.path.isdir(a.card):
        sys.exit(f"{a.card} is not a mounted card")
    dest = os.path.join(a.card, "ASSIST.UBX")
    shutil.copyfile(a.src, dest)
    print(f"copied {a.src} -> {dest}; eject the card, put it in the logger, power on")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("identity", help="read the receiver's identity frames over USB")
    p.add_argument("--port")
    p = sub.add_parser("register", help="register the receiver with Thingstream ZTP")
    p.add_argument("--payload-only", action="store_true", help="send payload hex instead of whole frames")
    p = sub.add_parser("fetch", help="download AssistNow data")
    p.add_argument("--url", help="service URL, if the registration response does not name one")
    p.add_argument("--param", action="append", default=[], help="extra query parameter key=value (repeatable)")
    p.add_argument("--out", default="assist/ASSIST.UBX")
    p = sub.add_parser("install", help="copy ASSIST.UBX to a mounted card")
    p.add_argument("card")
    p.add_argument("--src", default="assist/ASSIST.UBX")
    a = ap.parse_args()
    {"identity": cmd_identity, "register": cmd_register, "fetch": cmd_fetch, "install": cmd_install}[a.cmd](a)


if __name__ == "__main__":
    main()
