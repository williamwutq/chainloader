# AArch64 entry contract

The state the loader guarantees at the instant it enters a received image — the
same for the boot core at handoff and for each secondary when released. An image
built to these expectations runs identically whether it is the first or the tenth
loaded in a session, without a power cycle. This is normative: independent loaders
and payloads should both be able to rely on it.

## Processor state at entry (every core)

The boot core reaches this at handoff; each secondary reaches it when released —
every core enters the payload in the same state.

| Property        | Value at entry                                                                      |
|-----------------|-------------------------------------------------------------------------------------|
| Cores           | All four usable: core 0 at handoff, cores 1–3 via the release mailbox (`x7`)        |
| Exception level | EL1 (AArch64).                                                                      |
| MMU             | Off. All addresses physical.                                                        |
| D-cache         | Image cleaned to the Point of Coherency — coherent across **all** cores.            |
| I-cache         | Invalidated, so instruction fetch sees the loaded image.                            |
| `DAIF`          | All masked (D, A, I, F).                                                            |
| FP/SIMD         | Enabled (`CPACR_EL1.FPEN=0b11`); NEON usable.                                       |
| Timers          | Counters readable at EL1; `CNTVOFF_EL2 = 0`; `CNTFRQ_EL0` validated (see Clocks)    |
| UART            | PL011 (UART0) up at 115200 8N1 on GPIO14/15 (ALT0); usable without re-init.         |
| `SP`            | `SP_EL1` = `load_addr_max` (window top)                                             |
| `PC`            | Core 0: `load_addr + entry_off`. Secondary: the address the payload released it to. |
| EL2 service     | Loader stays resident at EL2; reachable from EL1 via `HVC` (see below)              |

Secondaries are **quiescent** until released: none touches memory before the
payload starts it, so core-0 init needs no cross-core synchronization.

## Register handoff (every core)

Every core — the boot core at handoff and each secondary when released — receives
this identical block, differing only in `x8` (`core_id`). Duplicating it means a
core never has to read shared RAM to learn the layout.

| Reg        | Value at entry                                                                     |
|------------|------------------------------------------------------------------------------------|
| `x0`       | `load_addr` — physical base of the loaded image                                    |
| `x1`       | `image_len` — image length in bytes                                                |
| `x2`       | `load_addr_min` — writable window low bound                                        |
| `x3`       | `load_addr_max` — writable window high bound                                       |
| `x4`       | `dtb` — device-tree pointer (`0` if none or invalid)                               |
| `x5`       | `dtb_size` — verified FDT total size in bytes (`0` if no valid DTB)                |
| `x6`       | `abi_version` — entry-ABI generation, for forward compatibility                    |
| `x7`       | `smp_release` — base of the secondary release mailbox                              |
| `x8`       | `core_id` — normalized core index (`0` for the boot core, `1`–`3` for secondaries) |
| `x9`       | `core_freq_hz` — pinned ARM core frequency in Hz (ABI gen 2; see Clocks)           |
| `x10`      | `periph_base` — peripheral (MMIO) base for this board (ABI gen 2; see Peripherals) |
| `x11`–`x30`| `0` — scrubbed                                                                     |
| `v0`–`v31` | `0` — scrubbed                                                                     |

`x4`/`x5` bound the device tree as `[dtb, dtb + dtb_size)`: the loader checks the
FDT magic at `dtb` and reads its `totalsize`, so both are `0` for no valid tree.
That region may fall inside the writable window, so a payload that needs the DTB
should copy it out (or avoid the range) before reusing the memory.

## Clocks at entry (every core)

Two clocks matter to a payload, and the loader fixes a definite guarantee for
each before handoff. Neither tracks the other: the generic timer and the ARM
core clock are separately sourced, and a third reference — the BCM 1 MHz system
timer — is what the loader cross-checks against at boot.

**Generic timer — the timebase.** `CNTFRQ_EL0` is **guaranteed valid**: nonzero,
and within 10% of the frequency implied by counting `CNTVCT_EL0` against the
independent 1 MHz system timer over a 10 ms window. With `CNTVOFF_EL2 = 0` and
EL1 counter access granted, a payload reads `CNTFRQ_EL0` for the rate and
`CNTPCT_EL0`/`CNTVCT_EL0` for the count — this is the correct way to measure
wall-clock time at EL1. If the check fails (firmware left `CNTFRQ_EL0` zero or
bogus), the loader **refuses to boot a payload** and halts with a message on the
UART, rather than hand over a broken timebase.

**ARM core clock — pinned and reported.** The CPU core frequency is not
self-describing at EL1 (no architected register reports it), so the loader
requests **1 GHz** (the Zero 2 W's rated maximum) through the VideoCore mailbox —
the firmware clamps the request to its configured max, so a board or `config.txt`
with a lower cap is honored and nothing is overclocked — and reports the rate it
reads back in **`x9`** (`core_freq_hz`). A payload that needs its core clock —
e.g. to calibrate a cycle-counted busy-loop — takes it from `x9` rather than
guessing. This is **best-effort**, not a hard guarantee for all time: firmware
thermal or under-voltage throttling can still lower the core clock after handoff.
A payload that needs exact timing must therefore use the generic timer, never a
cycle count. `x9` is re-pinned and re-reported on every `HVC #0` reload, so a
payload that changed the core clock does not leave a stale value for its
successor.

## Peripherals at entry (every core)

The loader hands the board's **peripheral (MMIO) base** to the payload in `x10`
(`periph_base`), so a payload can locate the PL011 UART, the GPIO block, the
VideoCore mailbox, and the system timer without hardcoding an address. The
peripheral registers sit at fixed offsets from this base across the supported
family — for example the mailbox at `periph_base + 0xB880` and the 1 MHz system
timer at `periph_base + 0x3004` — so `x10` plus those offsets is enough to drive
them on any board the loader supports. On the BCM2836/7 family (Pi 2 / 3 /
Zero 2 W) this is `0x3F00_0000` today; it becomes a detected value under the
planned `board-detect` work, at which point a payload that reads `x10` needs no
change. The ARM-local peripherals (per-core timers, mailboxes) live at their own
fixed `0x4000_0000` base and are not derived from `x10`.

Both `x9` and `x10` are present from ABI generation 2 (`x6`). A payload built
against generation 1 must not read them.

## Secondary release mailbox

`x7` points at an array of one `u64` per core, indexed by `core_id` (slot *N* =
`x7 + N*8`). The loader zero-fills every slot — including the unused slot 0 (core
0 is the boot core) — so a parked core waits while its slot reads `0`. To start
core *N*: write its EL1 entry address to slot *N*, then `SEV`. The core leaves
its `WFE`, adopts the processor state above, and branches there with the full
register handoff (its own `x8 = N`).

The slot is a **doorbell**: the loader clears it back to `0` the moment the core
takes it, so the value is not readable afterwards as a "started" flag, and a core
that later re-parks (e.g. after an `HVC #0`) waits again rather than relaunching
on the old address. Write it once per start.

| Slot      | Core | Contents                                       |
|-----------|------|------------------------------------------------|
| `x7 + 0`  | 0    | `0` — unused (boot core), zeroed like the rest |
| `x7 + 8`  | 1    | `0`, then a `u64` entry address (then `SEV`)   |
| `x7 + 16` | 2    | `0`, then a `u64` entry address                |
| `x7 + 24` | 3    | `0`, then a `u64` entry address                |

The mailbox is physical RAM; once the payload enables its MMU/caches it must map
it non-cacheable or maintain it by hand (the standard spin-table caveat).

Every core wakes on the same default `SP` (the window top), so a payload starting
more than one secondary must get each off it before the next runs — release them
serially, or have each switch to its own stack immediately on entry.

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
cycle; the handler flushes the data cache first, so a caller that had its MMU and
caches on leaves no stale lines behind the download.

`HVC #0` may be issued from **any** core. The reload always runs on the **boot
core** (so it stays core 0): a secondary's `HVC #0` is forwarded to the boot core
via a per-core IPI and the secondary re-parks itself. The reloading boot core then
forces any still-running secondary back into its parked state with the same IPI,
so reload is safe with SMP regardless of which core requested it.

This uses one interrupt per core: **every core routes its FIQ to the loader**
(for the reload request on the boot core, and the re-park on secondaries), so a
payload uses **IRQ** (not FIQ) for its own interrupts on every core. FIQ belongs
to the loader everywhere; IRQ is usable everywhere.

## Payload notes

`VBAR_EL1 = 0` is a null base, not a handler: install a vector table before
taking any exception. The loader zero-fills the BSS tail `[load_addr + image_len,
load_addr + mem_len)`, so zero-initialized statics are already clear. Stay within
the writable window `[x2, x3)` and clear of `[__loader_start, __loader_end)`, so a
later `load` or `HVC #0` reload can reuse the still-resident loader. The loader
never expects control back — it parks if a payload returns.
