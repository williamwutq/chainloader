# chainloader-loader

The Pi-side loader: a `no_std`, bare-metal AArch64 program that boots from the
SD card as `kernel8.img`, brings up the PL011 UART, accepts an AArch64 image
over the wire, validates it, and jumps to it.

It lives on the SD card permanently; payloads arrive over UART and are never
written to the card. See [`../docs/PROTOCOL.md`](../docs/PROTOCOL.md) and
[`../docs/ENTRY_CONTRACT.md`](../docs/ENTRY_CONTRACT.md).

## Building

This package is excluded from the workspace and pins `aarch64-unknown-none` in
[`.cargo/config.toml`](.cargo/config.toml), so build it from this directory:

```sh
cd loader
cargo build --release
```

Then flatten the ELF into the firmware's `kernel8.img` (uses `rust-objcopy`
from the `llvm-tools` component; `cargo install cargo-binutils` provides it):

```sh
rust-objcopy -O binary \
  target/aarch64-unknown-none/release/chainloader kernel8.img
```

Copy `kernel8.img` to the SD card's boot partition once. `config.txt` needs only
`arm_64bit=1` and `enable_uart=1`. The firmware then loads `kernel8.img` to
`0x80000` and enters on core 0. The `enable_uart=1` setting leaves the PL011 on
its default 48 MHz reference clock, which the loader assumes when programming the
baud divisors. On the Bluetooth-equipped boards (Zero 2 W, Pi 3) the loader frees
PL011 from the on-board Bluetooth by returning GPIO32/33 to inputs — so **no
`dtoverlay=disable-bt` is required**.

## Serial console

The loader talks over PL011 UART0 at **115200 8N1** on the GPIO header. Wire a
3.3V USB-TTL adapter (do **not** connect its power pin — power the Pi from its own
supply):

| Adapter | Pi header             |
|---------|-----------------------|
| `RXD`   | pin 8  — GPIO14 (TXD) |
| `TXD`   | pin 10 — GPIO15 (RXD) |
| `GND`   | pin 6 (or any ground) |

The Pi's UART pins are 3.3V and **not** 5V-tolerant; a multi-function adapter
(TTL/RS232/RS485) must be switched to plain **TTL** mode or its TTL pins stay
dead.

**Power-on order matters:** plug the USB adapter into the host **first and wait at
least ~2 seconds** for it to enumerate and initialize, *then* power the Pi. If the
Pi is powered while the adapter is still initializing, the link won't come up (the
adapter's RX light won't even flash) and you'll see nothing.

Until a host speaks the protocol, the loader prints `chainloader: up, waiting for
host (HELLO)...` once per second — a heartbeat to confirm the link and baud. The
first inbound byte silences it, so it never interferes with a real transfer.

The image is 64-bit, so the board must be AArch64-capable: the Zero 2 W, Pi 3,
and Pi 2 rev 1.2 (BCM2837) all qualify; the original Pi 2 rev 1.1 (BCM2836,
Cortex-A7) is 32-bit only and cannot run it.

## Memory map

The linker ([`link.ld`](link.ld)) places the loader at `0x80000` and brackets
its footprint — code, data, BSS, and a 512 KiB stack — between `__loader_start`
and `__loader_end`. The receive path rejects any image window overlapping that
range so the running loader can never overwrite itself.

## Status

Boot trampoline, UART bring-up, banner, and the full framed
receive/validate/jump path are implemented ([`src/receive.rs`](src/receive.rs)):
it advertises `READY`, validates each `HEADER` against the writable window and
the loader's own footprint, streams `DATA` into RAM under a running CRC, and on
`BOOT` runs the cache-maintenance sequence and branches per the entry contract.
UART bring-up is confirmed on a Zero 2 W (banner and heartbeat at 115200); the
end-to-end transfer/jump path is not yet hardware-validated — cache behavior and a
receive timeout are the remaining open questions in
[`../PLANNED.md`](../PLANNED.md).

## License

MIT — see [LICENSE](../LICENSE).
