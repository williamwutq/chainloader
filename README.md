# chainloader

A minimal development chainloader for the Raspberry Pi 2 (AArch64):

```text
Pi firmware → SD-resident AArch64 loader → UART → host binary → jump
```

The loader lives on the SD card permanently. Day-to-day development is meant to
be nothing more than a build and a UART load — no SD-card modification between
payload builds:

```sh
cargo build --release   # build your AArch64 bare-metal payload
cargo pi load           # transfer it over UART and boot it
```

[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

## Layout

| Path                                               | What it is                                                                    |
|----------------------------------------------------|-------------------------------------------------------------------------------|
| [`chainloader-protocol/`](chainloader-protocol/)   | `no_std`, allocation-free wire format shared by loader and host.              |
| [`loader/`](loader/)                               | Pi-side bare-metal loader (`aarch64-unknown-none`), flashed as `kernel8.img`. |
| [`cargo-pi/`](cargo-pi/)                           | Host `cargo pi load` / `cargo pi console` subcommand.                         |
| [`docs/PROTOCOL.md`](docs/PROTOCOL.md)             | Normative wire format and handshake.                                          |
| [`docs/ENTRY_CONTRACT.md`](docs/ENTRY_CONTRACT.md) | AArch64 register/cache state at the jump.                                     |

The workspace contains the two host-buildable crates (`chainloader-protocol`,
`cargo-pi`). `loader` is excluded and pins `aarch64-unknown-none`, so it builds
from its own directory — see [`loader/README.md`](loader/README.md).

## Status

Early scaffold. Implemented: the framing/CRC core of the protocol, the loader
boot/UART skeleton (builds and links at `0x80000`, not yet hardware-validated),
and `cargo-pi` argument dispatch. The streaming decoder, typed messages, the
loader receive/validate/jump path, and the host transport are designed in
[`PLANNED.md`](PLANNED.md) against the committed docs above.

## Development

```sh
cargo test                                   # host crates: protocol + cargo-pi
cargo clippy --all-targets --all-features    # what CI gates on
cargo fmt --check
( cd loader && cargo build --release )       # the bare-metal loader
```

CI runs on `master` and `main`: [`ci.yml`](.github/workflows/ci.yml) tests the
host crates across a stable/beta/nightly × OS matrix, and
[`check.yml`](.github/workflows/check.yml) gates clippy, formatting, docs, and a
bare-metal build of the loader.

## Acknowledgements

The bare-metal boot approach follows the lineage of David Welch's Raspberry Pi
bootloaders and the widely-used PL011 register conventions.

## License

MIT — see [LICENSE](LICENSE).
