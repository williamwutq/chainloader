# Planned Features

This document outlines planned work for the `chainloader` project. It is a
design surface, not a backlog: an entry exists so the decision can be argued
with *before* anything is built, and it should carry enough reasoning that a
reader who disagrees knows exactly which claim to attack.

A shipped entry moves to `CHANGELOG.md` under `[Unreleased]` and is deleted
from here.

The scaffold in place today: `chainloader-protocol`'s framing, CRC, and
streaming `Decoder`; the loader boot/UART skeleton; and `cargo-pi` argument
dispatch. The wire format (`docs/PROTOCOL.md`) and the jump contract
(`docs/ENTRY_CONTRACT.md`) are committed designs these entries implement.

---

## Entry format

Each entry is a `##` heading, followed by a metadata block, followed by three
required subsections: `### Motivation`, `### Design`, `### Open questions`.
State `No` explicitly rather than omitting a metadata field.

---

## `loader-receive` — receive, validate, and jump (0.1.0)

**Crate:** `chainloader-loader`.
**Breaking change:** No — the loader has no public API.
**Depends on:** the shipped `Decoder` and message structs.

### Motivation

The loader currently echoes bytes. The whole point is to receive an image,
validate it, and jump. This is the core of the project and the phase-3/phase-4
work.

### Design

A state machine driven by `Decoder` over `Uart::get_byte`, replying with
`encode_frame` over `Uart::put_byte`:

- `HELLO` → reply `READY` advertising `max_image_len`, the writable window
  `[load_addr_min, load_addr_max)` derived from a conservative constant window
  minus `[__loader_start, __loader_end)`, `alignment`, and `max_chunk`.
- `HEADER` → validate: `image_len <= max_image_len`; `load_addr` aligned;
  `[load_addr, load_addr + image_len)` inside the window and non-overlapping the
  loader (read the linker symbols); reply `ACK(next=0)` or the matching `ERROR`.
- `DATA` → check `offset == expected`, bounds-check, copy into RAM with
  `write_volatile`, fold into a streaming `Crc32`, reply `ACK(next=received)`.
- final `DATA` → verify streamed CRC == `image_crc32`; `ACK` or `ERROR(ImageCrc)`.
- `BOOT` → run the `ENTRY_CONTRACT.md` cache sequence and branch. `BOOT` with no
  verified image → `ERROR(NoImage)`.

Bounds and alignment are checked against actual linker symbols, not just
constants, so the loader can never overwrite itself.

### Open questions

- **Timeouts.** The loader blocks on `get_byte`. A watchdog/timeout would let it
  re-announce `READY` after a host disconnect mid-transfer. Poll `FR` with a
  loop counter, or leave blocking and rely on the host retrying? Leaning: add a
  coarse timeout only once hardware bring-up works.
- **Cache maintenance while MMU off.** Confirm on hardware whether firmware
  leaves caches on; the `IC IALLU` + `DSB`/`ISB` sequence is written to be safe
  either way, but this needs measuring, not assuming.

## `cargo-pi-load` — host transport (0.1.0)

**Crate:** `cargo-pi`.
**Breaking change:** No — new subcommand behavior.
**Depends on:** the shipped `Decoder` and message structs.

### Motivation

`cargo pi load` and `cargo pi console` are stubs. The acceptance criterion is a
`cargo build --release && cargo pi load` dev loop, which needs a real serial
transport.

### Design

Add `serialport` (transport), and Cargo integration to (1) locate the built
AArch64 binary via `cargo build --message-format=json` / `cargo metadata`, (2)
read config from `[package.metadata.pi]`, (3) discover the `/dev/cu.*` device.
`load`: open the port, `HELLO`/`READY`, send `HEADER`, stream `DATA` in
`max_chunk` pieces with per-frame ACK and bounded retries, verify the final ACK,
send `BOOT`, then optionally fall through to `console`. Keep the dependency set
minimal — `serialport` plus `serde`/`toml` for config; hand-rolled arg parsing.

### Open questions

- **Config source.** `[package.metadata.pi]` in the payload's `Cargo.toml`
  vs. a separate `pi.toml`. Leaning on `[package.metadata.pi]` so one file
  configures the payload crate, with CLI flags overriding.
- **Device auto-select.** When multiple `/dev/cu.*` match, prompt or require
  `--port`? Leaning: pick the sole match automatically, else error listing
  candidates.

## `hardening` — malformed-input and repeat-load test suite (0.1.0)

**Crate:** workspace.
**Breaking change:** No.
**Depends on:** all of the above.

### Motivation

Phase 6 requires confidence against truncated/malformed frames, bad checksums,
oversized images, invalid addresses, disconnects, retries, and repeated loads
without power-cycling — the failure modes a dev tool hits daily.

### Design

An offline protocol test suite (host-side, no hardware) driving the
encoder/decoder against corrupted, truncated, reordered, and oversize frames and
asserting the exact `ErrorCode`. A host↔loader integration harness that replays
a load twice to prove the loader returns to `READY`. Fuzz the decoder with
`cargo fuzz` on `Decoder::push` to prove it never panics on arbitrary bytes.

### Open questions

- **Loader-side testing without hardware.** Whether to abstract `Uart` behind a
  byte-stream trait so the loader state machine can be exercised on the host
  against an in-memory pipe, or rely on QEMU (`-M raspi2`). Leaning: the trait,
  since it also keeps the state machine unit-testable.
