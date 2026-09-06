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

## `loader-hardware-bringup` — validate the loader on a real Pi 2 (0.1.0)

**Crate:** `chainloader-loader`.
**Breaking change:** No — the loader has no public API.
**Depends on:** the shipped receive path.

### Motivation

The receive/validate/jump path (`loader/src/receive.rs`) is implemented and the
UART and cache/jump instructions are verified in disassembly, but none of it has
run on hardware. The PL011 baud constants, the GPIO routing, and the
cache-maintenance sequence are written from the datasheet and need measuring.

### Design

Flash `kernel8.img`, confirm the banner over a USB-UART adapter, then drive a
real load with `cargo pi load` once that exists. The two decisions that cannot
be settled off-hardware:

### Open questions

- **Timeouts.** The loader blocks on `get_byte`. A watchdog/timeout would let it
  re-announce `READY` after a host disconnect mid-transfer. Poll `FR` with a
  loop counter, or leave blocking and rely on the host retrying? Leaning: add a
  coarse timeout only once hardware bring-up works.
- **Cache maintenance while MMU off.** Confirm on hardware whether firmware
  leaves caches on; the `IC IALLU` + `DSB`/`ISB` sequence is written to be safe
  either way, but this needs measuring, not assuming.
- **Baud clock.** The loader currently assumes `init_uart_clock=48000000` in
  `config.txt` (`IBRD=26`, `FBRD=3`), a verified-correct pairing but an external
  dependency. The bztsrc raspi3 tutorial instead sets the UART clock to a known
  rate via the mailbox property interface (`MBOX_TAG_SETCLKRATE`), making baud
  independent of `config.txt`. Adopt the mailbox approach so a stock SD card
  works — strongest single robustness win found during research.

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
