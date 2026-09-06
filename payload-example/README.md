# payload-example

A minimal AArch64 payload for the chainloader — the image you load to prove the
whole pipeline works end to end on real hardware.

Built for `aarch64-unknown-none` and linked at `0x200000`. On entry it relies on
the state the loader guarantees ([`../docs/ENTRY_CONTRACT.md`](../docs/ENTRY_CONTRACT.md))
— a live UART, a stack, EL1 with FP/SIMD enabled, and the `x0`–`x4` register
handoff — to print what it received and park:

- the exception level (expects **EL1**);
- `x0`–`x4`: `load_addr`, `image_len`, the writable window `[min, max)`, and the
  DTB pointer;
- an FP multiply — which **faults if NEON were still trapped**, so a clean line
  proves FP/SIMD is enabled;
- a `.bss` read-back (`bss[1024] nz = 0`) — the array lives in `.bss`, so it is
  never transferred and reads zero only if the loader zero-filled the image's
  `memsz` tail. The payload then dirties it before parking, so a **second load
  without a power cycle** proves the loader *re-zeroed* it rather than merely
  finding fresh zeros.

A successful run prints a `[payload-example] running` banner over the serial
console — the end-to-end signal that build → flatten → transfer → validate →
jump all worked.

## Build and load

```sh
cd payload-example
cargo pi load        # build, flatten, transfer over UART, and boot
```

## Why it's a separate crate

Like the loader, it only builds for the bare-metal target, so it is excluded
from the workspace and pins `aarch64-unknown-none` in its own
[`.cargo/config.toml`](.cargo/config.toml). It uses **no** external crates — not
even `chainloader-protocol` — because a booted payload speaks to nothing; it
just consumes the register/UART state the loader established.

## License

MIT — see [../LICENSE](../LICENSE).
