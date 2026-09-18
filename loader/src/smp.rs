//! Secondary-core (1–3) bring-up.
//!
//! On the BCM2837 the firmware holds cores 1–3 in a spin-table: each waits at
//! EL2 for a jump address to be written to its release word (`0xe0`/`0xe8`/`0xf0`)
//! and an event signalled. Core 0 [`release`]s them into [`secondary_trampoline`],
//! a resident loader routine that enables FP/SIMD, then parks the core in a `WFE`
//! loop polling its slot of [`SMP_MAILBOX`]. When the payload writes an EL1 entry
//! address to that slot and `SEV`s, the core drops EL2→EL1 and branches there with
//! the *same* register handoff core 0 received (differing only in `x8 = core_id`),
//! reconstructed from [`SMP_HANDOFF`]. See `../docs/ENTRY_CONTRACT.md`.
//!
//! A single-core payload simply never writes the mailbox, so the secondaries stay
//! parked in the loader forever — harmless.
//!
//! On an `HVC #0` reload, [`quiesce_secondaries`] forces any running secondary
//! back into the trampoline (via a per-core mailbox IPI routed to EL2 as a FIQ)
//! so the reload is safe with SMP; see [`crate::hvc`].

use core::arch::{asm, global_asm};
use core::ptr::write_volatile;
use core::sync::atomic::{AtomicU64, Ordering};

/// Per-core release slots, indexed by `core_id` (slot 0 is unused — core 0 is the
/// boot core). The payload writes a core's EL1 entry address to its slot and
/// `SEV`s to start it; a slot reading `0` keeps that core parked. Handed to the
/// payload in `x7`.
#[unsafe(no_mangle)]
pub static SMP_MAILBOX: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// The `x0`–`x6` handoff core 0 publishes so a released secondary reconstructs the
/// identical register block without the payload having to stage it in RAM. Order:
/// `load_addr`, `image_len`, `load_addr_min`, `load_addr_max`, `dtb`, `dtb_size`,
/// `abi_version`.
#[unsafe(no_mangle)]
pub static SMP_HANDOFF: [AtomicU64; 7] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// `1` while core *N* is running the payload (has left the trampoline), `0` while
/// parked. Lets [`quiesce_secondaries`] IPI exactly the running cores and wait for
/// each to re-park. Slot 0 (boot core) is unused. Maintained by the trampoline.
#[unsafe(no_mangle)]
pub static SMP_ACTIVE: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Firmware spin-table release words (armstub8): core *N* spins on these at EL2
/// until a non-zero jump address is written and an event signalled.
const SPIN_CORE1: usize = 0xe0;
const SPIN_CORE2: usize = 0xe8;
const SPIN_CORE3: usize = 0xf0;

/// BCM2836/7 ARM-local per-core mailbox-0 write-set register (`+ 0x10*core`).
/// Writing it, with that core's mailbox routed to FIQ, traps the core to EL2 so
/// the loader can re-park it — the reload IPI. See [`quiesce_secondaries`].
const LOCAL_MBOX0_SET: usize = 0x4000_0080;

unsafe extern "C" {
    /// The resident trampoline label released cores enter at EL2.
    static secondary_trampoline: u8;
}

/// Base address of the release mailbox, for the `x7` handoff.
pub fn mailbox_base() -> u64 {
    (&raw const SMP_MAILBOX).cast::<u8>() as u64
}

/// Publishes the `x0`–`x4` handoff and releases cores 1–3 into the trampoline,
/// where they park polling their mailbox slot. Call once, on core 0, just before
/// the jump to the payload — after the image is in place and cleaned to the Point
/// of Coherency, so a secondary that later runs it sees coherent data.
///
/// # Safety
///
/// Must run on core 0 with the MMU off. Writes the firmware spin-table words and
/// signals the secondaries; the loader must stay resident afterwards (it does —
/// the payload window never overlaps it), since the parked cores execute from it.
#[allow(clippy::too_many_arguments)]
pub unsafe fn release(
    load_addr: u64,
    image_len: u64,
    win_min: u64,
    win_max: u64,
    dtb: u64,
    dtb_size: u64,
    abi_version: u64,
) {
    // Publish the handoff before the cores can read it. They only read it once
    // started (well after this returns), but store it first regardless.
    SMP_HANDOFF[0].store(load_addr, Ordering::Relaxed);
    SMP_HANDOFF[1].store(image_len, Ordering::Relaxed);
    SMP_HANDOFF[2].store(win_min, Ordering::Relaxed);
    SMP_HANDOFF[3].store(win_max, Ordering::Relaxed);
    SMP_HANDOFF[4].store(dtb, Ordering::Relaxed);
    SMP_HANDOFF[5].store(dtb_size, Ordering::Relaxed);
    SMP_HANDOFF[6].store(abi_version, Ordering::Relaxed);
    // Slots start zero (static init); keep them zero so cores wait to be started.
    for slot in &SMP_MAILBOX {
        slot.store(0, Ordering::Relaxed);
    }

    let tramp = (&raw const secondary_trampoline) as u64;
    // SAFETY: fixed low-memory spin-table words the firmware armstub polls; MMU
    // off, so these stores are non-cacheable and directly visible to the cores.
    unsafe {
        write_volatile(SPIN_CORE1 as *mut u64, tramp);
        write_volatile(SPIN_CORE2 as *mut u64, tramp);
        write_volatile(SPIN_CORE3 as *mut u64, tramp);
        // Publish the writes, then wake the spinning cores.
        asm!("dsb sy", "sev", options(nostack, preserves_flags));
    }
}

/// Forces every *running* secondary back into the trampoline park loop, for a
/// safe reload. Sends each active core a mailbox IPI — routed to EL2 as a FIQ
/// (`HCR_EL2.FMO` on secondaries), where `fiq_repark` re-enters the trampoline —
/// then waits until it has re-parked. A single-core payload started none, so this
/// is a no-op. Call on the boot core, at EL2, before re-running the receive path.
///
/// # Safety
///
/// Boot core at EL2, MMU off; performs ARM-local-peripheral MMIO.
pub unsafe fn quiesce_secondaries() {
    for (core, active) in SMP_ACTIVE.iter().enumerate().skip(1) {
        if active.load(Ordering::Relaxed) != 0 {
            // SAFETY: fixed per-core mailbox-0 set register; the write traps the
            // core to EL2 (FMO), where `fiq_repark` re-enters the trampoline.
            unsafe { write_volatile((LOCAL_MBOX0_SET + 0x10 * core) as *mut u32, 1) };
        }
    }
    // Wait for each to re-park (the trampoline clears its SMP_ACTIVE slot).
    for slot in &SMP_ACTIVE[1..] {
        while slot.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
    }
}

// The secondary trampoline. Entered at EL2 by a core the firmware releases from
// its spin-table. Enables FP/SIMD, parks in a WFE loop on the core's mailbox
// slot, and on release rebuilds the x0-x4 handoff, adds x7 (mailbox) / x8
// (core_id), drops EL2->EL1, and ERETs to the payload — mirroring core 0's jump.
global_asm!(
    r#"
.section .text
.global secondary_trampoline
secondary_trampoline:
    msr     daifset, #0xf              // mask interrupts through bring-up
    mrs     x8, mpidr_el1
    and     x8, x8, #0xff              // x8 = core_id, carried to the payload

    // FP/SIMD on at EL2 and EL1 (matches the core-0 boot path).
    mrs     x0, cptr_el2
    bic     x0, x0, #(1 << 10)         // CPTR_EL2.TFP = 0
    msr     cptr_el2, x0
    mov     x0, #(3 << 20)             // CPACR_EL1.FPEN = 0b11
    msr     cpacr_el1, x0
    isb

    // Route this core's mailbox 0 to FIQ, so the boot core's reload IPI traps
    // this core to EL2 (see quiesce_secondaries / fiq_repark). FIQ bits are the
    // high nibble of the mailbox int-control register: mailbox 0 -> FIQ = 0x10.
    movz    x9, #0x4000, lsl #16
    add     x9, x9, #0x50
    add     x9, x9, x8, lsl #2         // 0x4000_0050 + 4*core_id (mbox int control)
    mov     w10, #0x10                 // mailbox 0 -> FIQ
    str     w10, [x9]

    // Mark this core parked, then wait for the payload to write its release slot.
    adrp    x11, SMP_ACTIVE
    add     x11, x11, :lo12:SMP_ACTIVE
    str     xzr, [x11, x8, lsl #3]     // SMP_ACTIVE[core_id] = 0
    adrp    x9, SMP_MAILBOX
    add     x9, x9, :lo12:SMP_MAILBOX
    add     x9, x9, x8, lsl #3         // &SMP_MAILBOX[core_id]
1:  wfe
    ldr     x10, [x9]                  // x10 = requested EL1 entry (0 = keep waiting)
    cbz     x10, 1b

    // Released: mark this core active before it runs the payload.
    adrp    x11, SMP_ACTIVE
    add     x11, x11, :lo12:SMP_ACTIVE
    mov     x12, #1
    str     x12, [x11, x8, lsl #3]     // SMP_ACTIVE[core_id] = 1

    // This core's I-cache must see the image (D-cache already clean to PoC).
    ic      iallu
    dsb     sy
    isb

    // Rebuild the x0-x6 contract from core 0's published handoff.
    adrp    x11, SMP_HANDOFF
    add     x11, x11, :lo12:SMP_HANDOFF
    ldp     x0, x1, [x11]              // load_addr, image_len
    ldp     x2, x3, [x11, #16]         // load_addr_min, load_addr_max
    ldp     x4, x5, [x11, #32]         // dtb, dtb_size
    ldr     x6, [x11, #48]             // abi_version
    adrp    x7, SMP_MAILBOX
    add     x7, x7, :lo12:SMP_MAILBOX  // x7 = release mailbox base

    // EL2 -> EL1 config (mirrors the core-0 jump, plus FMO so this secondary's
    // FIQ routes to EL2 for the reload re-park IPI — IRQ stays with the payload).
    movz    x12, #0x8000, lsl #16      // HCR_EL2.RW = 1: EL1 is AArch64
    orr     x12, x12, #0x8             // HCR_EL2.FMO = 1: physical FIQ -> EL2
    msr     hcr_el2, x12
    adrp    x12, el2_vectors           // resident EL2 vectors (HVC service)
    add     x12, x12, :lo12:el2_vectors
    msr     vbar_el2, x12
    mov     x12, #0b11                 // CNTHCTL_EL2: EL1PCTEN | EL1PCEN
    msr     cnthctl_el2, x12
    msr     cntvoff_el2, xzr
    movz    x12, #0x0800
    movk    x12, #0x30d0, lsl #16      // SCTLR_EL1 = 0x30d0_0800 (MMU/caches off, RES1)
    msr     sctlr_el1, x12
    msr     vbar_el1, xzr
    msr     sp_el1, x3                 // SP at the window top
    movz    x12, #0x03c5               // SPSR_EL2 = EL1h, DAIF masked
    msr     spsr_el2, x12
    msr     elr_el2, x10               // return to the payload entry at EL1

    // Scrub every GPR not carrying the contract (x0-x8 stay).
    mov     x9, xzr
    mov     x10, xzr
    mov     x11, xzr
    mov     x12, xzr
    mov     x13, xzr
    mov     x14, xzr
    mov     x15, xzr
    mov     x16, xzr
    mov     x17, xzr
    mov     x18, xzr
    mov     x19, xzr
    mov     x20, xzr
    mov     x21, xzr
    mov     x22, xzr
    mov     x23, xzr
    mov     x24, xzr
    mov     x25, xzr
    mov     x26, xzr
    mov     x27, xzr
    mov     x28, xzr
    mov     x29, xzr
    mov     x30, xzr

    // Scrub the SIMD/FP register file.
    movi    v0.2d, #0
    movi    v1.2d, #0
    movi    v2.2d, #0
    movi    v3.2d, #0
    movi    v4.2d, #0
    movi    v5.2d, #0
    movi    v6.2d, #0
    movi    v7.2d, #0
    movi    v8.2d, #0
    movi    v9.2d, #0
    movi    v10.2d, #0
    movi    v11.2d, #0
    movi    v12.2d, #0
    movi    v13.2d, #0
    movi    v14.2d, #0
    movi    v15.2d, #0
    movi    v16.2d, #0
    movi    v17.2d, #0
    movi    v18.2d, #0
    movi    v19.2d, #0
    movi    v20.2d, #0
    movi    v21.2d, #0
    movi    v22.2d, #0
    movi    v23.2d, #0
    movi    v24.2d, #0
    movi    v25.2d, #0
    movi    v26.2d, #0
    movi    v27.2d, #0
    movi    v28.2d, #0
    movi    v29.2d, #0
    movi    v30.2d, #0
    movi    v31.2d, #0
    eret
"#
);
