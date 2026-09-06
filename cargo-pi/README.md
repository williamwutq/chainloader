# cargo-pi

A Cargo subcommand for the Raspberry Pi UART chainloader development loop:

```sh
cargo build --release   # build your AArch64 bare-metal payload
cargo pi load           # transfer it to the Pi over UART and boot it
cargo pi console        # attach as a plain serial console
```

No SD-card modification is required between payload builds — the chainloader
lives on the SD card permanently and receives each new image over the wire.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)

## Installation

```sh
cargo install --path .
```

Cargo exposes any `cargo-*` binary on `PATH` as a subcommand, so this becomes
`cargo pi`.

## What `load` does

1. Builds the payload (`cargo build`, release by default) and locates the
   resulting AArch64 executable.
2. Flattens its ELF loadable segments into a raw image — no external `objcopy`
   needed — deriving the load address and entry offset from the ELF. (A file
   that is already a flat binary is used as-is at the configured address.)
3. Opens the serial port, performs the `HELLO`/`READY` handshake, sends the
   image header, and streams the image as `DATA` frames with per-frame
   acknowledgement and retries.
4. The loader verifies the end-to-end CRC; `cargo pi load` then sends `BOOT`
   and, with `--console`, stays attached.

## Configuration

Reads defaults from the payload package's `[package.metadata.pi]` table,
overridable per invocation:

```toml
[package.metadata.pi]
port = "/dev/cu.usbserial-XXXX"   # default: sole USB serial adapter
baud = 115200
load-address = "0x200000"          # used only for raw (non-ELF) images
release = true
console = true                      # attach a console after loading
```

Flags: `--port`, `--baud`, `--package`, `--bin`, `--release`/`--debug`,
`--load-address`, `--console`/`--no-console`. Run `cargo pi --help` for the full
list.

## Status

Implemented and offline-tested: Cargo build/artifact location, the ELF
flattener (verified byte-for-byte against `objcopy`), config resolution, device
discovery, and the full protocol client. Not yet exercised against real
hardware. See [`../PLANNED.md`](../PLANNED.md).

## License

MIT — see [LICENSE](../LICENSE).
