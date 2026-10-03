# Planned Features

This document outlines planned work for the `chainloader` project. It is a
design surface, not a backlog: an entry exists so the decision can be argued
with *before* anything is built, and it should carry enough reasoning that a
reader who disagrees knows exactly which claim to attack.

A shipped entry moves to `CHANGELOG.md` under `[Unreleased]` and is deleted
from here.

In place today: the full `chainloader-protocol` codec; the loader's boot, UART,
and receive/validate/jump path; and `cargo pi load`/`console` end to end — all
validated on a real Pi Zero 2 W (clean 115200 UART, a framed image transfer, and
the EL1 register handoff, with `GET_ARM_MEMORY` sizing the window from real
hardware). What remains are the polish and feature items below. The wire format
(`docs/PROTOCOL.md`) and the jump contract (`docs/ENTRY_CONTRACT.md`) are the
committed references.

---

## Entry format

Each entry is a `##` heading, followed by a metadata block, followed by three
required subsections: `### Motivation`, `### Design`, `### Open questions`.
State `No` explicitly rather than omitting a metadata field.

---

## `console-raw-mode` — raw terminal for the post-load console

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

## `low-power-idle` — `WFI` core-halt for the low-power waiting loader

**Crate:** `chainloader-loader`.
**Breaking change:** No.
**Depends on:** an interrupt-controller bring-up on the loader.

### Motivation

Host-commanded low-power idle already ships: the `MODE` frame (`0x08`) drops the
loader into a quiet state — ACT LED off with a 500 ms flash every 30 s, heartbeat
slowed to ~120 s — and `MODE 0` or any `HELLO` brings it back, each confirmed on
the wire (`IDLE` `0x09` for low-power, `READY` for normal); `cargo pi sleep` /
`wake` drive it. But the loader still **polls** in that state, so the core never
halts and the real power win — sleeping the CPU between events — is still missing.
This entry is that last step.

### Design

Halt the boot core with `WFI` in the low-power idle loop, waking on either an
incoming UART byte or a system-timer deadline (the next blink/heartbeat). `WFI`
resumes on a *pending* interrupt regardless of the `DAIF` mask, so no EL2 handler
is needed — the loop clears the source and polls as today. The catch is that
nothing wakes `WFI` until the interrupt actually reaches the core. A first attempt
wired the BCM interrupt controller (`0x3F00_B210`/`B214`), the PL011 RX interrupt
(`UARTIMSC`), and a free system-timer compare (C1), but `WFI` never woke on the
Zero 2 W — so the missing piece is the **ARM-local** interrupt path: the GPU IRQ
must be routed to the boot core in the local controller (`0x4000_000C`), and
likely per-core enables set, none of which the loader configures yet.

### Open questions

- **What actually wakes `WFI`.** Confirm on hardware which registers make a BCM
  peripheral IRQ reach core 0's IRQ line — the ARM-local GPU-routing register
  (`0x4000_000C`), any per-core enable — since the BCM-controller enables alone
  did not. The system-timer compare and PL011 RX were both armed but never fired
  the wake.
- **PL011 `RTIM`.** Enabling the receive-timeout interrupt (to wake on a small
  command frame below the RX FIFO trigger) coincided with a hang even in a polling
  build, unexplained; understand it before re-enabling.
- **`WFE` + event stream.** Whether the generic-timer event stream driving `WFE`
  is a simpler halt that sidesteps the interrupt controller entirely — a periodic
  wake with no IRQ wiring at all.

## `hardening` — malformed-input and repeat-load test suite

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

## `board-detect` — support the BCM283x (`0x3F00_0000`) family

**Crate:** `chainloader-loader`.
**Breaking change:** No — additive; the current Zero 2 W path stays the default.
**Depends on:** the hardware-validated loader.

### Motivation

Beyond the shared `0x3F00_0000` peripheral base, the loader hardcodes Zero 2 W
specifics: GPIO29 (active-low) for the ACT LED, and the GPIO32/33 Bluetooth
unroute. Sibling boards on the same base — Pi 2 (no Bluetooth), Pi 3, other
Zero 2 variants — differ in the LED pin/polarity and whether the unroute is
needed. Scoping to this one family already covers the boards in reach; the base
is a constant, so a board just needs the right per-model details filled in.

### Design

The peripheral base is fixed for the whole family, so the mailbox (at
`base + 0xB880`) is always addressable — no bootstrapping needed. Query
`GET_BOARD_REVISION` (0x0001_0002) at boot for the exact model, then pick the ACT
LED pin/polarity and whether to run the Bluetooth unroute from a small board
table. `GET_ARM_MEMORY` (already used) sizes the writable window. Unknown boards
fall back to the current Zero 2 W profile with the LED disabled, so detection can
only add support, never break the working path.

### Open questions

- **Inaccessible LEDs.** Some boards (e.g. Pi 3B) wire the ACT LED to the
  VideoCore GPIO expander rather than an ARM GPIO, so bare-metal can't drive it —
  detect and skip the blink instead of toggling a wrong pin. (The
  `hvc-mailbox-services` entry below would make this LED reachable rather than
  skipped, via the expander's `GET`/`SET_GPIO_STATE` mailbox ops.)


## `hvc-mailbox-services` — scalar VideoCore mailbox services over `HVC`

**Crate:** `chainloader-loader` (plus a `payload-example` demo and an
`ENTRY_CONTRACT.md` note when shipped).
**Breaking change:** No — additive. `HVC #0` (reload) and the unknown-immediate
no-op are unchanged; the service lives behind a new immediate.
**Depends on:** the resident EL2 `HVC` service and the mailbox driver (both
shipped); complements the `x10` peripheral-base handoff.

### Motivation

A payload now finds the peripheral base in `x10`, so direct MMIO — UART, GPIO,
the system timer — is board-agnostic without the loader's help. But some
operations are reachable *only* through the VideoCore mailbox, and the most
important are not optional: the SD (EMMC) and USB controllers boot **unpowered**,
so a payload that wants to touch them must `SET_POWER_STATE` (and usually enable a
clock) over the mailbox first — there is no register poke that substitutes.

A payload can ring the mailbox itself with the `x10` base, but that means
re-implementing the property protocol and, once it has enabled its own MMU and
caches, hand-maintaining the 16-byte-aligned, non-cacheable request buffer the
GPU reads — the coherence footgun the loader sidesteps only because it runs with
the MMU off. The loader already owns the mailbox at EL2 in exactly that coherent
state. Forwarding a small set of *scalar* operations therefore gives the payload
board-agnostic access to the handful of mailbox services that are both essential
and awkward to DIY, without exposing the open-ended, buffer-shaped remainder of
the property interface.

### Design

A new `HVC #1` is the mailbox gateway. `HVC #0` (reload) and every other
immediate (the forward-compatible no-op) are untouched, so the gateway is purely
additive. The call is **scalar-only** — no pointer crosses the EL1→EL2 boundary,
so there is no EL1 VA for the MMU-off loader to translate and no caller buffer to
keep coherent:

- `x0` in: operation selector — a small loader-defined enum, *not* the raw
  VideoCore tag, so the payload is decoupled from tag numbers and the loader
  allowlists exactly the safe, scalar ops.
- `x1` in: the id the op addresses (clock id, power device id, GPIO id), or `0`.
- `x2` in: the value, for the write ops.
- `x0` out: status — `0` ok, nonzero for "unknown op" or "mailbox failure".
- `x1` out: the result (a GET's value; a SET echoes the applied value).

One selector is reserved for **capability discovery**: `QUERY` takes the op code
to probe in `x2` (with `x1 = 0`) and returns `x1 = 0` if this loader implements
that op, `1` if not — `0` as the affirmative, matching the `x0` status
convention. A payload probes for an op before relying on it rather than inferring
support from an "unknown op" failure, so a payload written against a newer loader
degrades deliberately on an older one. `QUERY` itself is op `0`, so it is always
present and `QUERY(QUERY)` is the handshake that the gateway exists at all.

The handler runs on the **boot core only**: it resets `SP_EL2` to the loader
stack and rings the mailbox against the loader's own buffer. The mailbox is a
single shared resource, so serializing on the boot core avoids cross-core races
without an EL2 lock; a call from a secondary returns the "wrong core" status
rather than racing. The handler is tiny — it rings the mailbox and returns two
values — and runs on its own EL2 stack, so it touches a small fixed set of
registers: it consumes `x0`–`x2`, defines `x0`/`x1`, and may use `x3` as free
scratch, so its clobber set is **`x0`–`x3`**. One declared scratch (`x3`) spares
the hot path a save/restore pair; anything the mailbox exchange needs beyond that
is saved and restored on the EL2 stack, leaving `x4`–`x30`, `SP`, and the FP/SIMD
file untouched. A payload wrapping the call need preserve nothing beyond `x0`–`x3`.
FIQ is masked on synchronous-exception entry, so the short exchange cannot be
re-entered by a forwarded reload IPI.

The initial allowlist is the scalar, board-agnostic operations a minimal payload
actually needs: `GET_BOARD_REVISION`; `GET`/`SET_POWER_STATE`;
`GET`/`SET_CLOCK_STATE`; `GET`/`SET_CLOCK_RATE`; `GET_THROTTLED`;
`GET_TEMPERATURE`; and the VideoCore expander `GET`/`SET_GPIO_STATE` (the path to
the ACT LED on boards that wire it there — the case `board-detect` flags). The
buffer-shaped families — framebuffer, memory allocation and `EXECUTE_CODE`,
command line, EDID — are deliberately excluded: a payload that needs them can
drive the mailbox itself via `x10`, and keeping them out holds the ABI small and
the loader from drifting into a general firmware-services layer.

### Open questions

- **Writes vs read-only first.** The reads are plainly safe; the writes are not
  uniformly so. `SET_POWER_STATE`/`SET_CLOCK_STATE` are benign, but
  `SET_CLOCK_RATE` (and any later `SET_VOLTAGE`) can destabilize or overheat the
  SoC. Whether the first cut ships rate control at all, or starts
  read-plus-power-only and adds it once a caller needs it, is open — leaning
  power + read-only to start.
- **Any-core access.** Boot-core-only keeps the handler lock-free, but a
  genuinely SMP payload wanting to power a peripheral from a worker core must
  bounce the request to core 0 itself. Whether real use justifies an EL2 spinlock
  (or a forward-to-boot-core path like the reload) is open; start boot-core-only
  and revisit if it bites.


## `usb-transport` — USB CDC-ACM device transport on the OTG port

**Crate:** `chainloader-loader` (plus a small `cargo-pi` touch-up).
**Breaking change:** No — an alternative transport; the UART path stays.
**Depends on:** the hardware-validated loader.

### Motivation

The UART tops out near 11 KB/s and needs a 3.3 V USB-TTL adapter wired to the
header. The Zero 2 W's micro-USB *data* port is a DWC2 OTG controller that can
act as a USB device; presenting a CDC-ACM serial gadget gives a fast,
single-cable, adapter-free link — and the host side is free, since macOS / Linux
/ Windows bind their in-box CDC-ACM driver and it enumerates as an ordinary
serial port.

### Design

The protocol and the receive state machine are transport-agnostic — they touch
only `get_byte` / `put_byte` — so USB is a transport swap, not a rewrite. Extract
a `ByteStream` trait over those two calls, implement it for the existing `Uart`
and for a new `Usb` (DWC2 device mode + CDC-ACM), and select the active one
behind the trait. The DWC2 register map and the CDC-ACM descriptors are already
scaffolded in `loader/src/usb.rs` (unlinked); what remains is enumeration (EP0
SETUP handling — `GET_DESCRIPTOR` / `SET_ADDRESS` / `SET_CONFIGURATION`) and the
bulk IN/OUT FIFO transfers. The host barely changes: `cargo-pi`'s `discover.rs`
already matches USB serial ports, so a CDC-ACM gadget is picked up like any
adapter.

### Open questions

- **PHY speed.** Full speed on the internal serial PHY is simplest (~1 MB/s,
  already ~100x the UART); high speed (480 Mbps) needs more PHY bring-up. Start
  full speed.
- **Interrupt vs polled.** No IRQ controller is configured, so the first cut
  polls `GINTSTS` from the receive loop; interrupts can come later.
- **Transport selection.** Auto-detect which link the host talks on first, or
  configure it explicitly? Either way keep the UART as the always-available
  early-boot / debug fallback.
- **`ByteStream` trait.** The same seam the `hardening` entry wants for host-side
  state-machine testing — extract it once and use it for both.
