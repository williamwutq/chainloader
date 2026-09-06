# Planned Features

This document outlines planned work for the `chainloader` project. It is a
design surface, not a backlog: an entry exists so the decision can be argued
with *before* anything is built, and it should carry enough reasoning that a
reader who disagrees knows exactly which claim to attack.

A shipped entry moves to `CHANGELOG.md` under `[Unreleased]` and is deleted
from here.

The scaffold in place today: the framing/CRC half of `chainloader-protocol`,
the loader boot/UART skeleton, and `cargo-pi` argument dispatch. The wire format
(`docs/PROTOCOL.md`) and the jump contract (`docs/ENTRY_CONTRACT.md`) are
committed designs these entries implement.

---

## Entry format

Each entry is a `##` heading, followed by a metadata block, followed by three
required subsections: `### Motivation`, `### Design`, `### Open questions`.
State `No` explicitly rather than omitting a metadata field.

---

## `decoder` — streaming resync frame decoder (0.1.0)

**Crate:** `chainloader-protocol`.
**Breaking change:** No — additive.
**Depends on:** nothing.

### Motivation

The loader reads the UART one byte at a time and cannot allocate. It needs to
turn a byte stream — which may start mid-frame or contain line noise — into
validated frames using only a fixed buffer. `encode_frame` exists; there is no
decoder yet, so the loader has nothing to drive its state machine.

### Design

A byte-fed state machine over a fixed `[u8; MAX_FRAME]` buffer:

```rust
pub enum Decoded { None, Frame(FrameType), Error(DecodeError) }

impl Decoder {
    pub const fn new() -> Self;
    pub fn push(&mut self, b: u8) -> Decoded; // feed one byte
    pub fn payload(&self) -> &[u8];           // valid until the next push
}
```

Phases: scan for `MAGIC_LO`/`MAGIC_HI`; collect the 4 fixed bytes
(version/type/length); collect `length + 4` body bytes; verify the CRC. On any
failure (bad version, unknown type, oversize length, CRC mismatch) it emits
`Decoded::Error` and returns to scanning — resync is scanning for the next
magic. `payload()` borrows the internal buffer and is valid until the next
`push`. No allocation, no panics on malformed input.

### Open questions

- **Payload lifetime.** "Valid until the next `push`" is the simplest contract
  and enough for a lockstep loader; a queue-of-frames API would be friendlier
  but needs storage the loader does not have. Keep the borrow contract?
- **Resync cost.** After a CRC failure, scanning one byte at a time is O(n) in
  garbage length. Fine for a wired dev link; worth revisiting only if noise is
  observed on real hardware.

## `messages` — typed payload structs (0.1.0)

**Crate:** `chainloader-protocol`.
**Breaking change:** No — additive.
**Depends on:** nothing (pairs with `decoder`).

### Motivation

`docs/PROTOCOL.md` fixes the byte layout of `READY`, `HEADER`, `DATA`, `ACK`,
and `ERROR`, but callers currently index raw slices. That duplicates the layout
on both sides and invites drift between loader and host.

### Design

One `#[repr(C)]`-free plain struct per payload with a `const LEN`, an
`encode(&self, out: &mut [u8])`, and a `decode(bytes: &[u8]) -> Result<Self,
MsgError>`, all little-endian via `to_le_bytes`/`from_le_bytes`. `DATA` stays a
thin `{ offset, &[u8] }` view rather than a struct, since its chunk is
borrowed. `ErrorCode` becomes an enum with `from_u16`/`as_u16` and `Display`.

### Open questions

- **Serde.** Whether to offer optional `serde` impls behind a feature for the
  host, or keep hand-rolled LE only. Leaning hand-rolled: the layouts are tiny
  and fixed, and it keeps the loader's dependency set empty.
- **`DATA` as a struct.** Whether a borrowing view is worth the asymmetry with
  the other payloads, versus copying the chunk (which the loader cannot afford).

## `loader-receive` — receive, validate, and jump (0.1.0)

**Crate:** `chainloader-loader`.
**Breaking change:** No — the loader has no public API.
**Depends on:** `decoder`, `messages`.

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
**Depends on:** `decoder`, `messages`.

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
