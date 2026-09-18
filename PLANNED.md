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

## `low-power-idle` — host-commanded low-power idle for the waiting loader

**Crate:** `chainloader-protocol` + `chainloader-loader` (plus a `cargo-pi` command).
**Breaking change:** No — a new frame type; an older loader answers `UnknownType`, an older host never sends it.
**Depends on:** the shipped protocol codec and the loader receive loop.

### Motivation

A loader left waiting for a host holds the ACT LED steady-on and emits a 1 s
heartbeat forever — fine on a bench, wasteful for a board left powered between
loads: LED current, constant UART traffic, and a core spinning the poll loop at
full tilt. A host that knows it will not load for a while should be able to tell
the loader to idle quietly, and to bring it back when it is ready to work again.

### Design

Add one host→Pi frame, `MODE` (type `0x08`), payload `mode: u8` (`0` = ready /
normal, `1` = low-power; other values rejected). The loader tracks a runtime idle
mode, default normal, reset to normal on every boot and `HVC #0` reload (a fresh
wait). Each change is confirmed on the wire: `MODE 0` re-sends `READY`; `MODE 1`
sends a new Pi→host `IDLE` frame (type `0x09`, payload `heartbeat_secs: u16`)
reporting the reduced beat, so the host does not read the coming silence as a
disconnect.

In low-power:

- **LED:** off, with a 500 ms liveness flash every 30 s instead of steady-on.
- **Heartbeat:** the `up, waiting` line drops from every 1 s to every ~120 s.
- **CPU:** enable the PL011 RX interrupt (`UARTIMSC.RXIM`) and a free system-timer
  compare for the next blink/heartbeat deadline, both asserted at the BCM
  interrupt controller, then `WFI` between events so the core halts until a byte
  arrives or the timer fires. `WFI` wakes on a pending interrupt regardless of the
  `DAIF` mask, so no EL2 IRQ handler is needed: on wake the loop clears the source
  (read the UART, re-arm the compare) and polls the decoder as today.

A new `HELLO` (a host starting a load) implicitly returns to normal
responsiveness, so a load never has to be preceded by a wake command; `MODE 0`
is the explicit resume for a host that wants the LED and heartbeat back without
loading. `MODE` is only meaningful while idle — one arriving mid-transfer is
rejected with `Unexpected`. `cargo pi` grows `sleep` / `wake` subcommands that
send the frame. No `PROTOCOL_VERSION` bump: the project is unpublished and both
new type bytes are additive.

### Open questions

- **Wake-timer wiring.** Which free system-timer compare drives the periodic wake
  (C0/C2 belong to the GPU; C1/C3 are free), and confirming that a PL011 RX
  interrupt enabled only at the controller — never unmasked into an EL handler —
  reliably wakes `WFI` on this silicon. If the interrupt path proves fiddly, a
  first cut keeps polling and just slows the LED and heartbeat, banking the LED
  and UART-traffic win without the core-halt.

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
  detect and skip the blink instead of toggling a wrong pin.


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
