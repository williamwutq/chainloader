//! EL2 reload service.
//!
//! The loader stays resident at EL2 after dropping the payload to EL1. Before
//! each `ERET` it installs [`el2_vectors`] as `VBAR_EL2`, so a running kernel can
//! trap back in via `HVC`:
//!
//! - `HVC #0` — reload: download and boot a new image, replacing the caller.
//! - `HVC #n` (n ≠ 0) — no-op: return to the caller immediately, unchanged.
//!
//! An unknown immediate returning cleanly means an accidental or forward-version
//! `HVC` is a harmless no-op rather than a fault. `HVC #0` reloads from **any**
//! core, but the reload always runs on the **boot core** so it stays core 0: a
//! secondary's `HVC #0` pings core 0's mailbox (routed to its FIQ) and re-parks
//! itself, and core 0's FIQ handler performs the reload. Before reloading, the
//! handler quiesces the running secondaries — see
//! [`crate::smp::quiesce_secondaries`]. Every core routes FIQ to the loader for
//! this; payloads use IRQ. See `../docs/ENTRY_CONTRACT.md`.

use core::arch::global_asm;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::receive;
use crate::uart::Uart;

/// The device-tree pointer from the first boot, replayed to a reloaded image.
static RELOAD_DTB: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    /// The EL2 vector table, installed as `VBAR_EL2` before each jump.
    static el2_vectors: u8;
    /// Clean+invalidate the entire data cache to the point of coherence.
    fn flush_dcache_all();
}

/// Records the boot device-tree pointer so a later `HVC #0` can replay it.
pub fn set_reload_dtb(dtb: u64) {
    RELOAD_DTB.store(dtb, Ordering::Relaxed);
}

/// Address of the EL2 vector table, to install into `VBAR_EL2` before a jump.
pub fn vbar_el2() -> u64 {
    (&raw const el2_vectors) as u64
}

/// Entered from the `HVC #0` handler at EL2 with `SP_EL2` reset to the loader
/// stack. Flushes the caches — the caller may have run with its MMU/caches on,
/// leaving dirty lines the MMU-off download below would not otherwise see — then
/// re-runs the receive path to download and boot a new image. Never returns.
#[unsafe(no_mangle)]
extern "C" fn hvc_reload() -> ! {
    // Force any running secondary back into the loader before the image is
    // overwritten, so the reload is safe with SMP.
    // SAFETY: boot core at EL2.
    unsafe { crate::smp::quiesce_secondaries() };
    // SAFETY: at EL2 on the boot core with a fresh loader stack. Flushing every
    // D-cache level makes the physical, MMU-off download that follows coherent
    // regardless of what the previous payload cached.
    unsafe { flush_dcache_all() };
    let mut uart = Uart;
    // SAFETY: re-establish a known UART state; the payload may have touched it.
    unsafe { uart.init() };
    let _ = writeln!(
        uart,
        "\nchainloader: HVC #0 reload; waiting for host (HELLO)."
    );
    // Two ACT-LED blinks: a reload is visible without a serial console, and
    // distinct from the boot burst (three). Also reclaims the LED from a payload
    // that was driving it (a re-parked secondary no longer touches it). Leaves it
    // lit as the steady "waiting for host" light.
    // SAFETY: boot core at EL2; secondaries are quiesced, so this is the only
    // GPIO user now.
    unsafe { crate::led::signal_alive(2) };
    receive::run(uart, RELOAD_DTB.load(Ordering::Relaxed))
}

global_asm!(
    r#"
.section .text

// EL2 exception vector table: 16 entries of 0x80 bytes, 2 KiB-aligned. Only the
// "lower EL, AArch64, synchronous" slot (0x400) matters — that is where an HVC
// from the EL1 payload traps. Every other slot returns to the caller.
.balign 2048
.global el2_vectors
el2_vectors:
    b   el2_return          // 0x000 Current EL SP0 Sync
    .balign 0x80
    b   el2_return          // 0x080 Current EL SP0 IRQ
    .balign 0x80
    b   el2_return          // 0x100 Current EL SP0 FIQ
    .balign 0x80
    b   el2_return          // 0x180 Current EL SP0 SError
    .balign 0x80
    b   el2_return          // 0x200 Current EL SPx Sync
    .balign 0x80
    b   el2_return          // 0x280 Current EL SPx IRQ
    .balign 0x80
    b   el2_return          // 0x300 Current EL SPx FIQ
    .balign 0x80
    b   el2_return          // 0x380 Current EL SPx SError
    .balign 0x80
    b   hvc_dispatch        // 0x400 Lower EL AArch64 Sync  <- HVC lands here
    .balign 0x80
    b   el2_return          // 0x480 Lower EL AArch64 IRQ
    .balign 0x80
    b   fiq_dispatch        // 0x500 Lower EL AArch64 FIQ  <- reload/re-park IPI
    .balign 0x80
    b   el2_return          // 0x580 Lower EL AArch64 SError
    .balign 0x80
    b   el2_return          // 0x600 Lower EL AArch32 Sync
    .balign 0x80
    b   el2_return          // 0x680 Lower EL AArch32 IRQ
    .balign 0x80
    b   el2_return          // 0x700 Lower EL AArch32 FIQ
    .balign 0x80
    b   el2_return          // 0x780 Lower EL AArch32 SError
    .balign 0x80

// Dispatch a synchronous trap from EL1. HVC #0 from ANY core reloads; anything
// else returns to the caller unchanged. Uses no stack — secondaries have no valid
// SP_EL2 — so it stashes its one scratch register in TPIDR_EL2 (an EL2-only
// scratch the EL1 payload cannot touch) instead of pushing, and never reads x1, so
// both caller registers survive the no-op return.
hvc_dispatch:
    msr     tpidr_el2, x0          // stash caller x0 (no stack); x1 left untouched
    mrs     x0, esr_el2
    lsr     x0, x0, #26            // EC = ESR_EL2[31:26]
    cmp     x0, #0x16              // HVC in AArch64?
    b.ne    hvc_noop
    mrs     x0, esr_el2
    and     x0, x0, #0xffff        // ISS[15:0] = the HVC immediate
    cbnz    x0, hvc_noop           // non-zero immediate: no-op
    // HVC #0. Reload always runs on the BOOT core, so it stays core 0 (secondaries
    // never become the boot core). From the boot core, reload here; from a
    // secondary, ping core 0's mailbox — its FIQ is routed to EL2, where
    // fiq_dispatch runs the reload — and re-park this core.
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff          // core_id (nonzero on a secondary)
    cbz     x0, hvc_reload_entry   // boot core: reload here
    // Secondary: mark parked BEFORE requesting, so the boot core's quiesce sees
    // this core already down and does not send it a redundant re-park IPI. Then
    // ping core 0's mailbox and re-park. x0/x1 are dead across the trampoline,
    // which rebuilds them.
    adrp    x1, SMP_ACTIVE
    add     x1, x1, :lo12:SMP_ACTIVE
    str     xzr, [x1, x0, lsl #3]  // SMP_ACTIVE[core_id] = 0
    dsb     sy                     // visible before the ping traps core 0
    movz    x1, #0x4000, lsl #16
    add     x1, x1, #0x80          // 0x4000_0080 = core 0 mailbox-0 write-set
    mov     w0, #1
    str     w0, [x1]
    b       secondary_trampoline   // re-park self; core 0 performs the reload
hvc_reload_entry:
    ldr     x0, =__stack_top       // reset SP to the loader stack; reload never returns
    mov     sp, x0
    b       hvc_reload

hvc_noop:
    mrs     x0, tpidr_el2          // restore caller x0 (x1 was never touched)
el2_return:
    eret

// A core's FIQ, routed to EL2 by HCR_EL2.FMO, lands here. Every core routes FIQ
// to the loader now. If this core's mailbox 0 is pending (the loader's IPI), clear
// it: on the BOOT core that IPI is a secondary's reload request, so run the
// reload; on a SECONDARY it is the reload re-park, so re-enter the trampoline.
// Otherwise return to the caller. Runs at EL2, uses no stack.
fiq_dispatch:
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff          // core_id
    movz    x1, #0x4000, lsl #16
    add     x1, x1, #0xc0
    add     x1, x1, x0, lsl #4     // 0x4000_00C0 + 0x10*core: mailbox-0 read/clear
    ldr     w2, [x1]
    cbz     w2, 1f                 // not our IPI: return to the caller
    str     w2, [x1]               // write-1-to-clear, deasserting the FIQ
    cbz     x0, hvc_reload_entry   // boot core: reload request -> perform reload
    b       secondary_trampoline   // secondary: re-park (does not return here)
1:  eret

// Clean and invalidate the entire data cache to the point of coherence, so a
// caller that ran with caches on leaves no dirty lines behind the MMU-off reload.
// Standard set/way sweep over CLIDR_EL1 levels. Clobbers x0-x11.
.global flush_dcache_all
flush_dcache_all:
    dsb     sy
    mrs     x0, clidr_el1
    and     x3, x0, #0x7000000
    lsr     x3, x3, #23            // x3 = LoC * 2
    cbz     x3, 5f
    mov     x10, #0                // x10 = level * 2
1:  add     x2, x10, x10, lsr #1   // x2 = level * 3 (CLIDR field position)
    lsr     x1, x0, x2
    and     x1, x1, #7             // cache type at this level
    cmp     x1, #2
    b.lt    4f                     // no data cache at this level
    msr     csselr_el1, x10        // select this level's data cache
    isb
    mrs     x1, ccsidr_el1
    and     x2, x1, #7
    add     x2, x2, #4             // x2 = log2(line size in bytes)
    mov     x4, #0x3ff
    and     x4, x4, x1, lsr #3     // x4 = max way index
    clz     w5, w4                 // w5 = way field shift
    mov     x7, #0x7fff
    and     x7, x7, x1, lsr #13    // x7 = max set index
2:  mov     x9, x4                 // x9 = way counter
3:  lsl     x6, x9, x5
    orr     x11, x10, x6           // level | (way << way_shift)
    lsl     x6, x7, x2
    orr     x11, x11, x6           // | (set << line_shift)
    dc      cisw, x11
    subs    x9, x9, #1
    b.ge    3b
    subs    x7, x7, #1
    b.ge    2b
4:  add     x10, x10, #2           // next level
    cmp     x3, x10
    b.gt    1b
5:  dsb     sy
    isb
    ret
"#
);
