# Chainloader wire protocol

Version 1. Endian: **little-endian** for every multi-byte field (matching the
AArch64 default, so the loader never byte-swaps). Transport: a raw UART byte
stream, 115200 8N1 by default. No allocation is required on either side.

This document is the normative reference; an independent implementation should
be able to interoperate from it alone. The Rust encoder/decoder lives in
[`chainloader-protocol`](../chainloader-protocol) and must match this text.

## Framing

Every message is one frame:

```text
offset  size  field     notes
  0      2    magic     = 0x50AE, little-endian (bytes 0xAE 0x50). Resync marker only.
  2      1    version   = 1 (PROTOCOL_VERSION)
  3      1    type      = FrameType (below)
  4      2    length    = payload byte count, 0..=1024 (MAX_PAYLOAD)
  6      N    payload    N = length
 6+N     4    crc32     CRC-32/ISO-HDLC over bytes [2 .. 6+N] (version..payload)
```

- The **magic** only lets a receiver relock after garbage; it is *not* covered
  by the CRC. A receiver that locks onto a coincidental magic still rejects the
  frame when the CRC fails, and resyncs by scanning for the next magic.
- The **CRC-32** is the ISO-HDLC / zlib CRC: reflected polynomial `0xEDB88320`,
  init `0xFFFF_FFFF`, final XOR `0xFFFF_FFFF`. Check value for `"123456789"` is
  `0xCBF43926`.
- `length` is bounded by `MAX_PAYLOAD = 1024`, so a receiver needs a fixed
  `MAX_FRAME = 6 + 1024 + 4 = 1034`-byte buffer and never allocates.

### Frame types

| Value | Name     | Direction | Payload                         |
|-------|----------|-----------|---------------------------------|
| 0x01  | `HELLO`  | host → Pi | `host_version: u32`             |
| 0x02  | `READY`  | Pi → host | loader capabilities (below)     |
| 0x03  | `HEADER` | host → Pi | image descriptor (below)        |
| 0x04  | `DATA`   | host → Pi | `offset: u32`, then chunk bytes |
| 0x05  | `ACK`    | Pi → host | `next_offset: u32`              |
| 0x06  | `ERROR`  | Pi → host | `code: u16`, `detail: u32`      |
| 0x07  | `BOOT`   | host → Pi | *(empty)*                       |

## Payloads

All fields little-endian. Offsets are within the payload.

### READY (Pi → host)  — 32 bytes

| off | size | field            | meaning                                       |
|-----|------|------------------|-----------------------------------------------|
| 0   | 4    | `loader_version` | loader build version                          |
| 4   | 4    | `max_image_len`  | largest image the loader will accept          |
| 8   | 8    | `load_addr_min`  | inclusive low bound of the writable window    |
| 16  | 8    | `load_addr_max`  | exclusive high bound of the writable window   |
| 24  | 4    | `alignment`      | required `load_addr` alignment (power of two) |
| 28  | 2    | `max_chunk`      | largest `DATA` chunk the loader accepts       |
| 30  | 2    | `reserved`       | 0                                             |

### HEADER (host → Pi) — 28 bytes

| off | size | field         | meaning                                                 |
|-----|------|---------------|---------------------------------------------------------|
| 0   | 8    | `load_addr`   | physical address to load the image at                   |
| 8   | 4    | `image_len`   | transferred image length in bytes (file-backed content) |
| 12  | 4    | `mem_len`     | total memory footprint incl. the zero-filled BSS tail   |
| 16  | 4    | `image_crc32` | CRC-32 of the transferred image (end-to-end integrity)  |
| 20  | 4    | `entry_off`   | byte offset added to `load_addr` for the entry PC       |
| 24  | 4    | `flags`       | 0 (reserved)                                            |

`mem_len >= image_len`. The loader writes the `image_len` transferred bytes at
`load_addr`, then zero-fills `[load_addr + image_len, load_addr + mem_len)` — the
image's BSS — before boot. It validates the full `mem_len` footprint against the
window and its own extent, not just the transferred bytes.

### DATA (host → Pi) — 4 + chunk bytes

`offset: u32` (byte offset of this chunk within the image) followed by up to
`max_chunk` chunk bytes. `4 + chunk_len` must not exceed `MAX_PAYLOAD`.

### ERROR (Pi → host) — 6 bytes

`code: u16` then `detail: u32` (context, e.g. the offset that failed). Codes:

| Code | Name             | Meaning                                     |
|------|------------------|---------------------------------------------|
| 1    | `BadVersion`     | frame `version` not supported               |
| 2    | `BadCrc`         | frame CRC mismatch                          |
| 3    | `BadLength`      | `length` out of range for the frame type    |
| 4    | `UnknownType`    | unrecognized `type` byte                    |
| 5    | `Unexpected`     | frame not valid in the current state        |
| 6    | `AddrOutOfRange` | image window outside `[load_addr_min, max)` |
| 7    | `AddrMisaligned` | `load_addr` violates `alignment`            |
| 8    | `AddrOverlap`    | image window overlaps the running loader    |
| 9    | `ImageTooLarge`  | `image_len` exceeds `max_image_len`         |
| 10   | `OffsetMismatch` | `DATA` `offset` ≠ expected next offset      |
| 11   | `ImageCrc`       | assembled image CRC ≠ `image_crc32`         |
| 12   | `NoImage`        | `BOOT` before a complete, verified image    |

## Conversation

```text
HOST                          PI
 |──── HELLO ─────────────────>|
 |<─── READY ──────────────────|   loader advertises window/limits
 |──── HEADER(addr,len,crc) ───>|
 |<─── ACK(next=0) / ERROR ────|   header validated (bounds, align, size)
 |──── DATA(off=0, chunk) ─────>|
 |<─── ACK(next=len0) ─────────|   lockstep: one ACK per DATA
 |──── DATA(off=len0, chunk) ──>|
 |<─── ACK(next=..) ───────────|
 |             ...              |
 |──── DATA(last) ─────────────>|
 |<─── ACK(next=image_len) ────|   full image received; image CRC verified
 |──── BOOT ───────────────────>|
 |<─── ACK ────────────────────|   optional "booting" ack, then jump
 |                             |── jump (see ENTRY_CONTRACT.md)
```

- **Lockstep** ACKs (one per `DATA`) keep the loader allocation-free and make
  retries trivial: on `ERROR` or timeout the host resends the same `DATA`.
- `ACK.next_offset` is the total bytes accepted so far, i.e. the offset the
  loader expects in the next `DATA`. It lets the host resync after a retry.
- The loader validates `HEADER` bounds/alignment/overlap (over the full
  `mem_len` footprint) *before* accepting any `DATA`, and verifies `image_crc32`
  over the assembled image *before* honoring `BOOT`. On `BOOT` it zero-fills the
  declared BSS tail `[image_len, mem_len)` before branching. `BOOT` with no
  verified image returns `ERROR(NoImage)`.

## Versioning

`version` is checked on every frame. An incompatible change bumps
`PROTOCOL_VERSION`; a loader that receives a higher version replies
`ERROR(BadVersion)` with `detail = supported_version` so the host can report a
clear mismatch rather than hanging.
