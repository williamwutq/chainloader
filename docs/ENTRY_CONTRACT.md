# AArch64 entry contract

The state the loader guarantees at the instant it enters a received image.
An image built to these expectations runs identically whether it is the first
or the tenth loaded in a session, without a power cycle. This is normative:
independent loaders and payloads should both be able to rely on it.

## Processor state at entry

| Property        | Value at entry                                                                                                                         |
|-----------------|----------------------------------------------------------------------------------------------------------------------------------------|
| Core            | Core 0 only. Cores 1–3 remain parked in the firmware spin loop.                                                                        |
| Exception level | EL1 (AArch64). Firmware delivers the loader at EL2; the loader configures EL1 and drops to it via `ERET` before entry (see below).     |
| MMU             | Off. No translation is enabled; all addresses are physical.                                                                            |
| Caches          | As the firmware left them. The loader performs the maintenance below so the freshly written image is coherent with instruction fetch.  |
| `DAIF`          | All masked (D, A, I, F). The payload owns interrupt setup.                                                                             |
| FP/SIMD         | Enabled. The loader clears the FP/SIMD trap (`CPTR_EL2.TFP=0` at EL2, `CPACR_EL1.FPEN=0b11`), so NEON is usable without further setup. |
| `SP`            | Points into the loader's stack. The payload **must** set its own stack before using one.                                               |
| `PC`            | `load_addr + entry_off`.                                                                                                               |

## Register handoff

| Reg    | Value at entry                                  |
|--------|-------------------------------------------------|
| `x0`   | `load_addr` — physical base of the loaded image |
| `x1`   | `image_len` — image length in bytes             |
| `x2`   | `load_addr_min` — writable window low bound     |
| `x3`   | `load_addr_max` — writable window high bound    |
| others | unspecified; do not rely on them                |

`x2`/`x3` are the same writable window the loader advertised in `READY`
(`WINDOW_MIN`/`WINDOW_MAX`): a half-open `[x2, x3)` of physical RAM the payload
can use freely. It sits entirely above the loader, so staying within it also
keeps clear of `[__loader_start, __loader_end)`. A payload that ignores `x0`–`x3`
(e.g. one linked to a fixed load address) is also valid.

## Cache/coherency sequence

The image arrives via ordinary stores while the I-cache may hold stale lines for
the destination range. Before entering the image, the loader:

1. Zero-fill the declared BSS tail `[load_addr + image_len, load_addr + mem_len)`
   so the image's zero-initialized statics are clear (and free of stale bytes
   from a previous load).
2. `DSB SY` — ensure all image and BSS stores have completed.
3. Clean the data cache to the point of unification over the full footprint
   `[load_addr, load_addr + mem_len)` (or clean+invalidate to PoC if caches are
   on), so instruction fetch sees the written bytes.
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
- `CNTHCTL_EL2.{EL1PCTEN,EL1PCEN} = 1`, `CNTVOFF_EL2 = 0` — EL1 can read the
  physical/virtual counters and timers without trapping to EL2.
- `SP_EL1` = the loader's stack top, so the payload has a valid (if temporary)
  stack immediately; it **must** still switch to its own before real use.
- `SPSR_EL2` = EL1h with `DAIF` masked, `ELR_EL2 = load_addr + entry_off`.

The loader installs **no** EL1 vector table (`VBAR_EL1` is left as-is), so a
payload that takes an exception before installing its own will fault. A payload
that wants to run at EL2 (e.g. a hypervisor) is not served by this loader.

## Memory image at entry

The loader writes the `image_len` transferred bytes at `load_addr` and zero-fills
the declared BSS tail `[load_addr + image_len, load_addr + mem_len)`, where
`mem_len` is the image's full memory footprint. So a payload's zero-initialized
statics are already zero at entry — it does **not** need to clear its own BSS.
(`mem_len` is derived by the host from the ELF; a raw binary has
`mem_len == image_len` and no zero-filled tail.)

## What the loader does *not* do

- It does not enable or configure the MMU.
- It does not wake secondary cores.
- It does not install an exception vector table for the payload; `VBAR_ELx` is
  left as-is. A payload taking exceptions must install its own.
- It does not clear general-purpose registers other than the handoff set.

## Payload obligations

- Set `SP` before using a stack.
- If it uses memory outside its own image, respect the writable window the
  loader advertised in `READY` and avoid `[__loader_start, __loader_end)` so a
  subsequent `load` can reuse the still-resident loader.
- Do not assume the caller returns; the loader parks (WFE) if control ever
  comes back, so returning is a dead end.
