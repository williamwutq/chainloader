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

## Configuration (planned)

`cargo pi` will read defaults from the payload package's
`[package.metadata.pi]` table — serial device, baud rate, load address,
binary/package/target selection, and whether to drop into a console after
load — with sensible Pi 2 defaults and command-line overrides.

## Status

Argument dispatch and help are implemented; the serial transport is not yet.
See [`../PLANNED.md`](../PLANNED.md).

## License

MIT — see [LICENSE](../LICENSE).
