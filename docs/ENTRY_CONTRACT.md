# AArch64 entry contract

The state the loader guarantees at the instant it branches to a received image.
An image built to these expectations runs identically whether it is the first
or the tenth loaded in a session, without a power cycle. This is normative:
independent loaders and payloads should both be able to rely on it.

## Processor state at entry

| Property        | Value at entry                                                                                                                        |
|-----------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Core            | Core 0 only. Cores 1–3 remain parked in the firmware spin loop.                                                                       |
| Exception level | The EL the firmware delivered to the loader (EL2 in the Pi's 64-bit boot). The loader does **not** change EL.                         |
| MMU             | Off. No translation is enabled; all addresses are physical.                                                                           |
| Caches          | As the firmware left them. The loader performs the maintenance below so the freshly written image is coherent with instruction fetch. |
| `DAIF`          | All masked (D, A, I, F). The payload owns interrupt setup.                                                                            |
| `SP`            | Points into the loader's stack. The payload **must** set its own stack before using one.                                              |
| `PC`            | `load_addr + entry_off`.                                                                                                              |

## Register handoff

| Reg    | Value at entry                                  |
|--------|-------------------------------------------------|
| `x0`   | `load_addr` — physical base of the loaded image |
| `x1`   | `image_len` — image length in bytes             |
| `x2`   | `0` — reserved for a future boot-info pointer   |
| `x3`   | `0` — reserved                                  |
| others | unspecified; do not rely on them                |

A payload that ignores `x0`–`x3` (e.g. one linked to a fixed load address) is
also valid.

## Cache/coherency sequence

The image arrives via ordinary stores while the I-cache may hold stale lines for
the destination range. Before branching, the loader:

1. Zero-fill the declared BSS tail `[load_addr + image_len, load_addr + mem_len)`
   so the image's zero-initialized statics are clear (and free of stale bytes
   from a previous load).
2. `DSB SY` — ensure all image and BSS stores have completed.
3. Clean the data cache to the point of unification over the full footprint
   `[load_addr, load_addr + mem_len)` (or clean+invalidate to PoC if caches are
   on), so instruction fetch sees the written bytes.
4. Invalidate the instruction cache (`IC IALLU`) and the branch predictor.
5. `DSB SY; ISB` — complete maintenance and flush the pipeline.
6. Branch to `load_addr + entry_off`.

If the MMU and caches are off throughout (the conservative default), steps 2–4
still run to cover the case where firmware left caches enabled.

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
