# chainloader-loader

The Pi-side loader: a `no_std`, bare-metal AArch64 program that boots from the
SD card as `kernel8.img`, brings up the PL011 UART, and — once the receive path
lands — accepts an AArch64 image over the wire, validates it, and jumps to it.

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

Copy `kernel8.img` to the SD card's boot partition once. With `arm_64bit=1` and
`enable_uart=1` in `config.txt`, the firmware loads it to `0x80000` and enters
on core 0. The loader pins the UART reference clock itself via the VideoCore
mailbox, so no `init_uart_clock` setting is required — a stock card works.

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
Not yet validated on hardware — cache behavior and a receive timeout are the
remaining open questions in [`../PLANNED.md`](../PLANNED.md).

## License

MIT — see [LICENSE](../LICENSE).
