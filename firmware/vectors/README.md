# format/v3 vectors checked on the board

Three of the `format/v3` conformance vectors, copied unchanged from
[`contracts-v0.9.0`](https://github.com/ParkWardRR/cairn-driving-log-selfhosted/tree/contracts-v0.9.0/contracts/format/v3/vectors)
(`70af4b4`), built into the firmware so the console's `vectors` command can
check `cairn-format` on the ESP32-S3 itself:

| Vector | What the board checks |
|---|---|
| `gnss-gap` | Structural scan (stop reason, frames, record counts), then keyed: segment key by HKDF from the test root, XChaCha20-Poly1305 open of every frame, plaintext digest |
| `auth-tag-tampered` | A flipped ciphertext bit: the structural scan cannot see it, the keyed scan must stop at `AUTH_FAILED` with three frames |
| `manifest-members-match` | CBOR decode, Ed25519 signature, content root, byte-identical re-encode |

All 62 vectors are checked on the host by the emulator's conformance runner at
the `cairn-format` commit `firmware/Cargo.toml` pins (`scripts/ci.sh`).

**These are public test keys** (see the vectors' own README): they protect
nothing and must never be provisioned.

`SHA256SUMS` lists the copied files.
