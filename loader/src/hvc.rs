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
//! `HVC` is a harmless no-op rather than a fault. Reload is a **boot-core**
//! operation — a secondary's `HVC #0` is treated as a no-op (see the multi-core
//! caveat in `../PLANNED.md`). See `../docs/ENTRY_CONTRACT.md`.

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
    b   el2_return          // 0x500 Lower EL AArch64 FIQ
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

// Dispatch a synchronous trap from EL1. HVC #0 on the boot core reloads; anything
// else returns to the caller unchanged. Preserves the caller's x0/x1 (the only
// registers it reads) across the no-op return path.
hvc_dispatch:
    stp     x0, x1, [sp, #-16]!    // save caller x0/x1 (SP_EL2 is a valid stack)
    mrs     x0, esr_el2
    lsr     x1, x0, #26            // EC = ESR_EL2[31:26]
    cmp     x1, #0x16              // HVC in AArch64?
    b.ne    el2_return_restore
    and     x0, x0, #0xffff        // ISS[15:0] = the HVC immediate
    cbnz    x0, el2_return_restore // non-zero immediate: no-op
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff          // reload only from the boot core
    cbnz    x1, el2_return_restore
    // HVC #0 on core 0: reset SP_EL2 to the loader stack and hand off to Rust.
    ldr     x0, =__stack_top
    mov     sp, x0
    b       hvc_reload

el2_return_restore:
    ldp     x0, x1, [sp], #16      // restore caller x0/x1
el2_return:
    eret

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
