# AArch64 entry goal

The **aspirational** entry state — what the loader aims to guarantee once the
planned work lands (secondary-core bring-up and the EL2 reload service). It is
**not implemented**; [`ENTRY_CONTRACT.md`](ENTRY_CONTRACT.md) is the current,
normative contract. This file fixes only the target *state* (the tables), so a
kernel can be pre-planned against the eventual ABI — not how the loader will get
there. Rows marked **†** differ from today.

## Processor state at entry (every core)

The boot core reaches this at handoff; each secondary reaches it when released —
every core enters the payload in the same state.

| Property        | Value at entry                                                                        |
|-----------------|---------------------------------------------------------------------------------------|
| Cores           | All four usable: core 0 at handoff, cores 1–3 via the release mailbox (`x7`) †        |
| Exception level | EL1 (AArch64).                                                                        |
| MMU             | Off. All addresses physical.                                                          |
| D-cache         | Image cleaned to the Point of Coherency — coherent across **all** cores.              |
| I-cache         | Invalidated, so instruction fetch sees the loaded image.                              |
| `DAIF`          | All masked (D, A, I, F).                                                              |
| FP/SIMD         | Enabled (`CPACR_EL1.FPEN=0b11`); NEON usable.                                         |
| Timers          | Counters readable at EL1; `CNTVOFF_EL2 = 0`; `CNTFRQ_EL0` valid †                     |
| UART            | PL011 (UART0) up at 115200 8N1 on GPIO14/15 (ALT0); usable without re-init.           |
| `SP`            | A valid, distinct per-core stack (`SP_EL1`) †                                         |
| `PC`            | Core 0: `load_addr + entry_off`. Secondary: the address the payload released it to. † |
| EL2 service     | Loader stays resident at EL2; reachable from EL1 via `HVC` (see below) †              |

Secondaries are **quiescent** until released: none touches memory before the
payload starts it, so core-0 init needs no cross-core synchronization.

## Register handoff (every core)

Every core — the boot core at handoff and each secondary when released — receives
this identical block, differing only in `x8` (`core_id`). Duplicating it means a
core never has to read shared RAM to learn the layout.

| Reg        | Value at entry                                                                       |
|------------|--------------------------------------------------------------------------------------|
| `x0`       | `load_addr` — physical base of the loaded image                                      |
| `x1`       | `image_len` — image length in bytes                                                  |
| `x2`       | `load_addr_min` — writable window low bound                                          |
| `x3`       | `load_addr_max` — writable window high bound                                         |
| `x4`       | `dtb` — device-tree pointer (`0` if none or invalid)                                 |
| `x5`       | `dtb_size` — verified FDT total size in bytes (`0` if no valid DTB) †                |
| `x6`       | `abi_version` — entry-ABI generation, for forward compatibility †                    |
| `x7`       | `smp_release` — base of the secondary release mailbox †                              |
| `x8`       | `core_id` — normalized core index (`0` for the boot core, `1`–`3` for secondaries) † |
| `x9`–`x30` | `0` — scrubbed                                                                       |
| `v0`–`v31` | `0` — scrubbed                                                                       |

`x4`/`x5` bound the device tree as `[dtb, dtb + dtb_size)`: the loader checks the
FDT magic at `dtb` and reads its `totalsize`, so both are `0` for no valid tree.
That region may fall inside the writable window, so a payload that needs the DTB
should copy it out (or avoid the range) before reusing the memory.

## Secondary release mailbox

`x7` points at an array of one `u64` per core, indexed by `core_id` (slot *N* =
`x7 + N*8`). The loader zero-fills every slot — including the unused slot 0 (core
0 is the boot core) — so a parked core waits while its slot reads `0`. To start
core *N*: write its EL1 entry address to slot *N*, then `SEV`. The core leaves
its `WFE`, adopts the processor state above, and branches there with the full
register handoff (its own `x8 = N`).

| Slot      | Core | Contents                                       |
|-----------|------|------------------------------------------------|
| `x7 + 0`  | 0    | `0` — unused (boot core), zeroed like the rest |
| `x7 + 8`  | 1    | `0`, then a `u64` entry address (then `SEV`)   |
| `x7 + 16` | 2    | `0`, then a `u64` entry address                |
| `x7 + 24` | 3    | `0`, then a `u64` entry address                |

The mailbox is physical RAM; once the payload enables its MMU/caches it must map
it non-cacheable or maintain it by hand (the standard spin-table caveat).

## Loader service (EL2)

The loader stays resident at EL2 with a vector table installed, so the EL1 kernel
can call back into it via `HVC`:

| Call             | Effect                                                        |
|------------------|---------------------------------------------------------------|
| `HVC #0`         | Download and boot a new kernel (reload), replacing the caller |
| `HVC #n` (n ≠ 0) | Invalid — returns to the caller immediately, with no effect   |

Unknown immediates return with no effect, so an accidental `HVC`, or one built
for a newer loader, is a safe no-op rather than a fault. The reload (`#0`) re-runs
the transfer at EL2 and re-enters the payload under this same contract — no power
cycle. See [`../PLANNED.md`](../PLANNED.md) (`hvc-reload-service`) for the cache
and multi-core caveats.
