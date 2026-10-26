#!/usr/bin/env python3
"""Provision a trail logger for the Cairn server, over USB.

    scripts/provision.py cert   --mac 48:ca:43:5b:ed:60    # issue the device's TLS client cert
    scripts/provision.py test   --mac 48:ca:43:5b:ed:60    # try that cert against the server, from this Mac
    scripts/provision.py write  --mac 48:ca:43:5b:ed:60    # write identity + Wi-Fi + server into the board's flash

The device id is the dongle's rule: the first 16 bytes of SHA-256 of the
Wi-Fi MAC, as 32 lowercase hex — and the server refuses a bundle whose
manifest device id differs from the client certificate's CommonName, so the
certificate is issued for exactly that name.

Inputs, none of them in this repository:
  ~/cairn-backups/pki-20261007/ca.pem, ca-key.pem   the Cairn private CA (key only on this Mac)
  ~/.config/cairn/wifi.env                          WIFI_SSID, WIFI_PASSWORD, optional WIFI_BSSID / WIFI_CHANNEL
Outputs:
  ~/cairn-backups/trail-<device_id>/                device key (0600), CSR, certificate
"""
import argparse
import glob
import hashlib
import os
import stat
import struct
import subprocess
import sys
import tempfile
import time
import zlib

PKI = os.path.expanduser("~/cairn-backups/pki-20261007")
WIFI = os.path.expanduser("~/.config/cairn/wifi.env")
OPENSSL = next((p for p in ("/opt/homebrew/opt/openssl@3/bin/openssl", "/opt/homebrew/opt/openssl@3.5/bin/openssl") if os.path.exists(p)), None)

SERVER_HOST = "cairn.alpina.casa"
SERVER_IP = "172.16.6.80"
SERVER_PORT = 8443

TAG = dict(DEVICE_ID=1, CLIENT_CERT=2, CLIENT_KEY=3, CA_CERT=4, WIFI_SSID=5, WIFI_PASSWORD=6,
           WIFI_BSSID=7, WIFI_CHANNEL=8, SERVER_HOST=9, SERVER_IP=10, SERVER_PORT=11, RECEIPT_KEY=12)


def run(*args, inp=None):
    r = subprocess.run(args, input=inp, capture_output=True)
    if r.returncode != 0:
        sys.exit(f"{' '.join(args[:3])}… failed: {r.stderr.decode(errors='replace')[:400]}")
    return r.stdout


def device_id(mac):
    raw = bytes.fromhex(mac.replace(":", "").replace("-", ""))
    if len(raw) != 6:
        sys.exit("MAC must be 6 bytes")
    return hashlib.sha256(raw).hexdigest()[:32]


def outdir(did):
    d = os.path.expanduser(f"~/cairn-backups/trail-{did}")
    os.makedirs(d, mode=0o700, exist_ok=True)
    return d


def cmd_cert(a):
    if not OPENSSL:
        sys.exit("needs Homebrew OpenSSL 3 (macOS LibreSSL signs with SHA-1, which Go rejects)")
    did = device_id(a.mac)
    d = outdir(did)
    key, csr, crt = (os.path.join(d, n) for n in ("client-key.pem", "client.csr", "client.pem"))
    if os.path.exists(crt) and not a.force:
        print(f"{crt} exists (use --force to reissue)")
        return
    old = os.umask(0o077)
    run(OPENSSL, "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", key)
    os.umask(old)
    run(OPENSSL, "req", "-new", "-key", key, "-subj", f"/CN={did}", "-out", csr, "-sha256")
    with tempfile.NamedTemporaryFile("w", suffix=".cnf", delete=False) as ext:
        ext.write("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n")
    run(OPENSSL, "x509", "-req", "-in", csr, "-CA", f"{PKI}/ca.pem", "-CAkey", f"{PKI}/ca-key.pem",
        "-CAserial", f"{PKI}/ca.srl", "-out", crt, "-days", "3650", "-sha256", "-extfile", ext.name)
    os.unlink(ext.name)
    print(f"issued CN={did}\n  {crt}\n  {key} (0600)")
    print(run(OPENSSL, "x509", "-in", crt, "-noout", "-subject", "-issuer", "-dates").decode())


def cmd_test(a):
    did = device_id(a.mac)
    d = outdir(did)
    out = subprocess.run(
        ["curl", "-sS", "--max-time", "15", "--cacert", f"{PKI}/ca.pem", "--cert", f"{d}/client.pem",
         "--key", f"{d}/client-key.pem", "--resolve", f"{SERVER_HOST}:{SERVER_PORT}:{SERVER_IP}",
         f"https://{SERVER_HOST}:{SERVER_PORT}/api/v2/health", "-w", "\nHTTP %{http_code}\n"],
        capture_output=True, text=True)
    print(out.stdout or out.stderr)


def receipt_key(did):
    d = outdir(did)
    out = subprocess.run(
        ["curl", "-sS", "--max-time", "15", "--cacert", f"{PKI}/ca.pem", "--cert", f"{d}/client.pem",
         "--key", f"{d}/client-key.pem", "--resolve", f"{SERVER_HOST}:{SERVER_PORT}:{SERVER_IP}",
         f"https://{SERVER_HOST}:{SERVER_PORT}/api/v2/server/receipt-key"], capture_output=True, text=True)
    import json
    try:
        j = json.loads(out.stdout)
        return bytes.fromhex(j["public_key"]), j.get("algorithm")
    except Exception:
        return None, None


def load_wifi():
    if not os.path.exists(WIFI):
        sys.exit(f"missing {WIFI}")
    if stat.S_IMODE(os.stat(WIFI).st_mode) & 0o077:
        sys.exit(f"{WIFI} is readable by others; chmod 600 it")
    env = {}
    for line in open(WIFI):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip()
    return env


def entry(tag, value):
    return struct.pack("<BH", TAG[tag], len(value)) + value


def payload(a):
    did = device_id(a.mac)
    d = outdir(did)
    if not os.path.exists(f"{d}/client.pem"):
        sys.exit("run `cert` first")
    cert = run(OPENSSL, "x509", "-in", f"{d}/client.pem", "-outform", "DER")
    key = run(OPENSSL, "ec", "-in", f"{d}/client-key.pem", "-outform", "DER")
    ca = run(OPENSSL, "x509", "-in", f"{PKI}/ca.pem", "-outform", "DER")
    w = load_wifi()
    p = b"".join([
        entry("DEVICE_ID", bytes.fromhex(did)),
        entry("CLIENT_CERT", cert),
        entry("CLIENT_KEY", key),
        entry("CA_CERT", ca),
        entry("WIFI_SSID", w["WIFI_SSID"].encode()),
        entry("WIFI_PASSWORD", w.get("WIFI_PASSWORD", "").encode()),
        entry("SERVER_HOST", SERVER_HOST.encode()),
        entry("SERVER_IP", bytes(int(x) for x in SERVER_IP.split("."))),
        entry("SERVER_PORT", struct.pack("<H", SERVER_PORT)),
    ])
    if a.pin_bssid and w.get("WIFI_BSSID"):
        p += entry("WIFI_BSSID", bytes.fromhex(w["WIFI_BSSID"].replace(":", "")))
        if w.get("WIFI_CHANNEL"):
            p += entry("WIFI_CHANNEL", bytes([int(w["WIFI_CHANNEL"])]))
    rk, alg = receipt_key(did)
    if rk:
        p += entry("RECEIPT_KEY", rk)
        print(f"server receipt key ({alg}): {rk.hex()[:16]}…")
    else:
        print("warning: could not fetch the server's receipt key; upload receipts will not verify until it is provisioned")
    return p


def cmd_write(a):
    import serial

    p = payload(a)
    crc = zlib.crc32(p) & 0xFFFFFFFF
    port = a.port or (sorted(glob.glob("/dev/cu.usbmodem*")) or [None])[0]
    if not port:
        sys.exit("no /dev/cu.usbmodem*: is the board plugged in?")
    s = serial.Serial(port, 115200, timeout=0.2)

    def say(line, expect, wait=3.0):
        s.reset_input_buffer()
        s.write((line + "\n").encode())
        buf = b""
        end = time.time() + wait
        while time.time() < end:
            buf += s.read(4096)
            if expect.encode() in buf:
                return buf.decode(errors="replace")
        sys.exit(f"no '{expect}' after `{line[:40]}`: {buf[-300:]!r}")

    say("prov begin", "prov: ready")
    h = p.hex()
    for i in range(0, len(h), 512):
        say(f"prov data {h[i:i + 512]}", "prov: ok")
    out = say(f"prov commit {crc:08x}", "prov: written", wait=8)
    print(out.strip().splitlines()[-1])
    print(say("prov show", "prov:").strip().splitlines()[-1])
    print("reboot the board (console: reboot) to start Wi-Fi")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("cert", "test", "write"):
        p = sub.add_parser(name)
        p.add_argument("--mac", required=True, help="the board's Wi-Fi MAC, e.g. 48:ca:43:5b:ed:60")
        if name == "cert":
            p.add_argument("--force", action="store_true")
        if name == "write":
            p.add_argument("--port")
            p.add_argument("--pin-bssid", action="store_true", help="join only the access point in wifi.env")
    a = ap.parse_args()
    {"cert": cmd_cert, "test": cmd_test, "write": cmd_write}[a.cmd](a)


if __name__ == "__main__":
    main()
