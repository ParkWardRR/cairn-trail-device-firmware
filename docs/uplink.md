# Wi-Fi uplink to a Cairn server

The logger works on its own. This is the optional layer on top: over home Wi-Fi it checks in with
a Cairn server, and — as the stages below land — uploads its sessions as Cairn's sealed
`format/v3` bundles and takes firmware updates.

**The owner's choices (2026-10-25):** the full Cairn path (`format/v3` bundles, enrolment, the
server's receipt), TLS from the start, sessions **kept** on the card after the server signs for
them, and firmware updates over Wi-Fi.

## What the server actually offers

From reading `cairn-vehicle-server` and the dongle firmware, not the specs alone:

- **The device listener** (`:8443`, on the LAN at `172.16.6.80`) requires **mutual TLS**: a P-256
  client certificate from the Cairn private CA whose CommonName is the device id — 32 lowercase
  hex, the first 16 bytes of SHA-256 of the Wi-Fi MAC. The same CA signs the server's certificate
  (SANs: `cairn.alpina.casa`, the tailnet name, both IPs).
- **Upload** is three calls: `POST /api/v2/bundles/offer` (body: the bundle's `manifest.cbor`,
  header `X-Cairn-Signature`: the device's Ed25519 signature) → `PUT
  /api/v2/bundles/{bundle_id}/chunks/{sha256}` for each missing chunk → `POST
  /api/v2/bundles/{bundle_id}/commit`, which answers with the signed receipt (CBOR). The receipt is
  checked against the server's Ed25519 receipt key (`GET /api/v2/server/receipt-key`).
- **Before a bundle is accepted** the device must be enrolled (its signing key in `devices.json`),
  its storage root escrowed (`keystore.json`), and assigned to a vehicle — for the trail logger, a
  *person* carrier — with the assignment id written into every bundle.
- **There is no check-in endpoint.** `GET /api/v2/health` over mutual TLS is the strongest check-in
  the server has; a signed check-in (the `ble/v1` draft's instruction frames) and `uplink/v1`
  exist only on paper. So **the trail logger uses the same `:8443` listener the dongle does**, and
  moves to `uplink/v1` when the server and the dongle do.
- **Firmware updates**: the server can serve a signed update descriptor and image
  (`GET /api/v2/firmware/latest`, `…/{sha256}/image`, when started with `-firmware-dir`); no device
  has a downloader yet.

## Stages

| # | Stage | State |
|---|---|---|
| 1 | **Wi-Fi + mutual TLS + check-in.** Join the provisioned network, DHCP, TLS 1.3 to the device listener with this device's certificate, the server checked against the pinned Cairn CA and its name, then `GET /api/v2/health` every 10 minutes. Identity and network settings in the `cairn` flash partition, written over USB | **Verified on the board, 2026-10-26:** joined `Grizzle` (pinned BSSID, channel 11, −63 dBm), DHCP `172.16.6.135`, TLS 1.3 with the device certificate, `checkin HTTP 200 {"status":"ok",…}`. Provisioning over USB: 1 123 bytes in 3.5 s, first try |
| 2 | **Enrolment.** Generate the device's Ed25519 signing key and storage root on the board, produce the sealed `enrolment/v1` blob, enrol it with `cairn-admin device enroll`, add a person carrier and an assignment | Designed |
| 3 | **Sealed bundles.** Port the dongle emulator's Rust `format/v3` writer to `no_std` and seal each session alongside its CSV files: GNSS samples, IMU summaries, health, time observations, marks, and the barometer/power/activity records proposed in the Cairn plan | Designed |
| 4 | **Upload and receipt.** offer → chunks (8 KiB) → commit; verify the receipt's signature and content root; mark the session uploaded and keep it | Designed |
| 5 | **Firmware updates.** A/B app slots (already in `partitions.csv`), fetch the signed descriptor at check-in, verify with the offline update key before downloading, hash the image back out of flash, then switch slots | Partition table in place |

## Provisioning (stage 1)

```sh
trail provision cert  --mac 48:ca:43:5b:ed:60   # issue CN=<device id> from the Cairn CA
trail provision test  --mac 48:ca:43:5b:ed:60   # try it against the server from this Mac
trail provision write --mac 48:ca:43:5b:ed:60 --pin-bssid   # write it all to the board
```

Nothing secret enters the repository: the CA key stays in `~/cairn-backups/pki-20261007/`, the
device key is written to `~/cairn-backups/trail-<device id>/` (0600) and to the board's flash, and
Wi-Fi credentials come from `~/.config/cairn/wifi.env` (0600). `--pin-bssid` joins only the access
point named there.

On the board, `prov show` and `net` report what is provisioned (never the secrets themselves), and
each check-in is an `EVENTS.CSV` row: `wifi` (joined, address), `checkin` (`HTTP 200 {"status":"ok",…}`
or why not).

**TLS without a clock.** Certificate dates are checked against the GNSS time when there is one and
otherwise against the firmware's commit time — never the unanchored RTC, which this board's was 25
days wrong.

## Flash layout

`firmware/partitions.csv`: `nvs`, `otadata`, `phy_init`, `cairn` (0x12000, 24 KiB: identity and
network settings, `src/keys.rs`), `ota_0` and `ota_1` (3.9 MiB each). The first flash with this
table needs `otadata` and `cairn` erased once, since the old single-app layout left bytes there.
