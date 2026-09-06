# chainloader

A minimal development chainloader for the Raspberry Pi Zero 2 W and Pi 2/3 (AArch64):

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

Feature-complete off hardware, not yet validated on a real Pi Zero 2 W. Implemented and
tested: the full `chainloader-protocol` codec (framing, CRC, streaming decoder,
typed messages); the loader's boot, PL011 bring-up (clock set via mailbox), and
receive/validate/jump path (links at `0x80000`); and `cargo pi load`/`console`
end to end — including an ELF flattener verified byte-for-byte against `objcopy`.
Remaining work (hardware bring-up, console raw mode, a hardening suite) is in
[`PLANNED.md`](PLANNED.md).

## Development

```sh
cargo test                                   # host crates: protocol + cargo-pi
cargo clippy --all-targets --all-features    # what CI gates on
cargo fmt --check
( cd loader && cargo build --release )       # the bare-metal loader
```

CI runs on `master` and `main`, one workflow per crate, each filtered to the
paths that affect it (a change to `chainloader-protocol` re-runs all three,
since the others depend on it):

- [`protocol.yml`](.github/workflows/protocol.yml) — tests
  `chainloader-protocol` across a stable/beta/nightly × OS matrix; clippy, fmt, docs.
- [`cargo-pi.yml`](.github/workflows/cargo-pi.yml) — same matrix for the host
  subcommand.
- [`loader.yml`](.github/workflows/loader.yml) — cross-builds the loader for
  `aarch64-unknown-none` on stable/beta/nightly; clippy and fmt.

## Acknowledgements

The bare-metal boot approach follows the lineage of David Welch's Raspberry Pi
bootloaders and the widely-used PL011 register conventions.

## License

MIT — see [LICENSE](LICENSE).
