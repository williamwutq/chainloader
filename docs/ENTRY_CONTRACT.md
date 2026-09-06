# AArch64 entry contract

The state the loader guarantees at the instant it enters a received image.
An image built to these expectations runs identically whether it is the first
or the tenth loaded in a session, without a power cycle. This is normative:
independent loaders and payloads should both be able to rely on it.

For the target state once the planned work lands (secondary cores, …), see
[`ENTRY_GOAL.md`](ENTRY_GOAL.md) — aspirational, not yet guaranteed.

## Processor state at entry

| Property        | Value at entry                                                                 |
|-----------------|--------------------------------------------------------------------------------|
| Core            | Core 0 only. Cores 1–3 remain parked in the firmware spin loop.                |
| Exception level | EL1 (AArch64).                                                                 |
| MMU             | Off. No translation is enabled; all addresses are physical.                    |
| D-cache         | Loaded image cleaned to the Point of Coherency; otherwise as firmware left it. |
| I-cache         | Invalidated (`IC IALLU`), so instruction fetch sees the loaded image.          |
| `DAIF`          | All masked (D, A, I, F).                                                       |
| FP/SIMD         | Enabled (`CPACR_EL1.FPEN=0b11`); NEON usable.                                  |
| Timers          | Generic timer/counter readable at EL1 (physical & virtual); `CNTVOFF_EL2 = 0`. |
| UART            | PL011 (UART0) up at 115200 8N1 on GPIO14/15 (ALT0); usable without re-init.    |
| `SP`            | `SP_EL1` = `load_addr_max` (top of the writable window).                       |
| `PC`            | `load_addr + entry_off`.                                                       |

## Register handoff

| Reg        | Value at entry                                         |
|------------|--------------------------------------------------------|
| `x0`       | `load_addr` — physical base of the loaded image        |
| `x1`       | `image_len` — image length in bytes                    |
| `x2`       | `load_addr_min` — writable window low bound            |
| `x3`       | `load_addr_max` — writable window high bound           |
| `x4`       | `dtb` — firmware device-tree-blob pointer, `0` if none |
| `x5`–`x30` | `0` — scrubbed for a clean handoff                     |
| `v0`–`v31` | `0` — SIMD/FP register file scrubbed                   |

`x2`/`x3` are the same writable window the loader advertised in `READY`
(`WINDOW_MIN`/`WINDOW_MAX`): a half-open `[x2, x3)` of physical RAM the payload
can use freely. It sits entirely above the loader, so staying within it also
keeps clear of `[__loader_start, __loader_end)`.

`x4` is the device-tree-blob pointer the firmware handed the loader (in `x0` at
its own entry), forwarded verbatim. It is `0` when the firmware loaded no device
tree, so a payload that uses it must handle the null case. A payload that ignores
`x0`–`x4` (e.g. one linked to a fixed load address) is also valid.

## Cache/coherency sequence

The image arrives via ordinary stores while the I-cache may hold stale lines for
the destination range. Before entering the image, the loader:

1. Zero-fill the declared BSS tail `[load_addr + image_len, load_addr + mem_len)`
   so the image's zero-initialized statics are clear (and free of stale bytes
   from a previous load).
2. `DSB SY` — ensure all image and BSS stores have completed.
3. Clean the data cache to the Point of Coherency (`DC CVAC`) over the full
   footprint `[load_addr, load_addr + mem_len)`, so instruction fetch sees the
   written bytes.
4. Invalidate the instruction cache (`IC IALLU`) and the branch predictor.
5. `DSB SY; ISB` — complete maintenance and flush the pipeline.
6. Drop EL2 → EL1 (below) and `ERET` to `load_addr + entry_off`.

If the MMU and caches are off throughout (the conservative default), steps 2–4
still run to cover the case where firmware left caches enabled.

## Dropping to EL1

The image is entered at **EL1 (AArch64)**. Firmware hands the loader EL2; before
the final `ERET` the loader configures the EL1 it returns into:

- `HCR_EL2.RW = 1` — EL1 executes in AArch64.
- `SCTLR_EL1 = 0x30d0_0800` — a reset value with the MMU and caches **off** and
  the architectural RES1 bits set. The payload owns turning the MMU on.
- `VBAR_EL1 = 0` — a null vector base. The loader installs **no** actual EL1
  vector table, so a payload that takes an exception before setting its own
  `VBAR_EL1` will fault. `0` is a deterministic base, not a working handler.
- `CNTHCTL_EL2.{EL1PCTEN,EL1PCEN} = 1`, `CNTVOFF_EL2 = 0` — EL1 can read the
  physical/virtual counters and timers without trapping to EL2.
- `SP_EL1` = `load_addr_max`, the top of the writable window — a valid default
  stack in the payload's own RAM (full-descending, outside the loader), so it
  works immediately. The payload may keep it or set its own.
- `SPSR_EL2` = EL1h with `DAIF` masked, `ELR_EL2 = load_addr + entry_off`.

`x0`–`x4` carry the handoff (above); every other general-purpose register
(`x5`–`x30`) and the whole SIMD/FP register file (`v0`–`v31`) are zeroed just
before the `ERET` for a clean, deterministic entry. A payload that wants to run
at EL2 (e.g. a hypervisor) is not served by this loader.

## Memory image at entry

The loader writes the `image_len` transferred bytes at `load_addr` and zero-fills
the declared BSS tail `[load_addr + image_len, load_addr + mem_len)`, where
`mem_len` is the image's full memory footprint. So a payload's zero-initialized
statics are already zero at entry — it does **not** need to clear its own BSS.
(`mem_len` is derived by the host from the ELF; a raw binary has
`mem_len == image_len` and no zero-filled tail.)

## Secondary cores (1–3)

Everything above describes the **boot core (core 0)** only. Cores 1–3 never enter
the loader: the firmware parks them in its own spin-table (`WFE` on the release
slots `0xe0`/`0xe8`/`0xf0`) at **EL2**, in raw firmware state — no EL1 drop, no
FP/SIMD enable, no `VBAR`, no stack, registers unscrubbed. A payload that starts
a secondary via the spin-table therefore receives it at EL2 and must establish
that core's state itself (drop to EL1, enable FP/SIMD, install vectors, …).

Bringing secondaries up in the same state as core 0 requires the loader to take
them over from the firmware stub and own their release path; that is planned
(`../PLANNED.md`), not yet implemented.

## What the loader does *not* do

- It does not enable or configure the MMU.
- It does not wake secondary cores.
- It does not install an actual exception vector table; it only sets
  `VBAR_EL1 = 0` (above), so a payload taking exceptions must install its own.

## Payload obligations

- A default stack (`SP_EL1` = window top) is provided; set your own only if you
  want it elsewhere.
- If it uses memory outside its own image, respect the writable window the
  loader advertised in `READY` and avoid `[__loader_start, __loader_end)` so a
  subsequent `load` can reuse the still-resident loader.
- Do not assume the caller returns; the loader parks (WFE) if control ever
  comes back, so returning is a dead end.
