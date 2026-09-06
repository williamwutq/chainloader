# Planned Features

This document outlines planned work for the `chainloader` project. It is a
design surface, not a backlog: an entry exists so the decision can be argued
with *before* anything is built, and it should carry enough reasoning that a
reader who disagrees knows exactly which claim to attack.

A shipped entry moves to `CHANGELOG.md` under `[Unreleased]` and is deleted
from here.

In place today (all off-hardware paths implemented and tested): the full
`chainloader-protocol` codec; the loader's boot, UART, and receive/validate/jump
path; and `cargo pi load`/`console` end to end. What remains is validation on a
real Pi 2 and the polish items below. The wire format (`docs/PROTOCOL.md`) and
the jump contract (`docs/ENTRY_CONTRACT.md`) are the committed references.

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
real load with `cargo pi load`. The two decisions that cannot be settled
off-hardware:

### Open questions

- **Timeouts.** The loader blocks on `get_byte`. A watchdog/timeout would let it
  re-announce `READY` after a host disconnect mid-transfer. Poll `FR` with a
  loop counter, or leave blocking and rely on the host retrying? Leaning: add a
  coarse timeout only once hardware bring-up works.
- **Cache maintenance while MMU off.** Confirm on hardware whether firmware
  leaves caches on; the `IC IALLU` + `DSB`/`ISB` sequence is written to be safe
  either way, but this needs measuring, not assuming.
- **Baud clock — resolved, needs hardware confirmation.** The loader pins the
  UART clock to 4 MHz via the mailbox (`src/mailbox.rs`, `MBOX_TAG_SETCLKRATE`)
  and uses `IBRD=2`, `FBRD=0xB`, matching the bztsrc reference, so no
  `init_uart_clock` setting is needed. What remains is confirming on hardware
  that the mailbox exchange succeeds and the banner is legible at 115200.

## `console-raw-mode` — raw terminal for the post-load console (0.2.0)

**Crate:** `cargo-pi`.
**Breaking change:** No.
**Depends on:** the shipped `console` passthrough.

### Motivation

`cargo pi console` currently forwards line-buffered stdin, so keystrokes reach
the Pi a line at a time and there is no character echo control — fine for
watching output, awkward for interacting with a payload's own REPL.

### Design

Put the terminal into raw mode (cbreak, no echo) for the console's lifetime and
restore it on exit, so bytes flow through immediately. Needs a `termios` call on
Unix; weigh a tiny dependency against a small `libc`-free `ioctl` wrapper.

### Open questions

- **Dependency.** Whether a raw-mode helper crate earns its place, or whether a
  minimal hand-rolled `tcsetattr` via `libc` is enough. Leaning hand-rolled to
  keep the dependency set small, consistent with the rest of the tool.

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
