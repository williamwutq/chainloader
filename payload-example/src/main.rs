//! Minimal example payload for the chainloader.
//!
//! Built for `aarch64-unknown-none` and linked at `0x200000`, this is the image
//! `cargo pi load` transfers and the loader jumps to. It exercises the entry
//! contract (`../docs/ENTRY_CONTRACT.md`): the loader hands it a live UART, a
//! stack, EL1 with FP/SIMD enabled, and the `x0`–`x8` register handoff. It
//! prints those and parks — a quick end-to-end check that the whole pipeline
//! (build → flatten → transfer → validate → jump) works on real hardware.
//!
//! It also smoke-tests secondary-core bring-up: the boot core starts core 1 via
//! the release mailbox (`x7`), and the secondary re-enters this same image at
//! EL1 with `x8 = 1` and reports in. Finally the boot core `HVC #0`s back into
//! the loader (the EL2 reload service), so it is ready for the next load without
//! a power cycle.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

// PL011 UART0, already brought up by the loader (115200 8N1); reused as-is.
const UART0_DR: usize = 0x3F20_1000;
const UART0_FR: usize = 0x3F20_1018;
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full

/// A small buffer that lands in `.bss`: zero-initialized, so it is *not* part of
/// the transferred image (only its `memsz` is). It reads as zero at entry only
/// if the loader zero-filled the image's BSS tail — see the check in `main`.
const SCRATCH_LEN: usize = 1024;
static mut SCRATCH: [u8; SCRATCH_LEN] = [0; SCRATCH_LEN];

/// Zero-sized handle to the UART the loader left running.
struct Uart;

impl Uart {
    #[inline]
    fn put(&self, byte: u8) {
        // SAFETY: fixed MMIO; the loader left the PL011 enabled and configured.
        unsafe {
            while read_volatile(UART0_FR as *const u32) & FR_TXFF != 0 {
                core::hint::spin_loop();
            }
            write_volatile(UART0_DR as *mut u32, u32::from(byte));
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.put(b'\r');
            }
            self.put(b);
        }
        Ok(())
    }
}

/// The full `x0`–`x8` register handoff, captured by the `_start` shim, in the
/// order it pushes them. `#[repr(C)]` so the field offsets match the stores.
#[repr(C)]
pub struct Handoff {
    load_addr: u64,
    image_len: u64,
    win_min: u64,
    win_max: u64,
    dtb: u64,
    dtb_size: u64,
    abi_version: u64,
    mailbox: u64,
    core_id: u64,
}

// Entry shim. The loader `ERET`s here at EL1 with the handoff in `x0`–`x8`, one
// more register than the C ABI carries as arguments, so push all nine to the
// stack and pass a pointer to that `Handoff`. Secondaries first drop `SP` by
// `core_id * 64 KiB` so each runs on its own stack rather than colliding with
// core 0 at the window top.
global_asm!(
    r#"
.section .text.boot
.global _start
_start:
    cbz     x8, 1f                 // boot core keeps SP at the window top
    mov     x9, sp
    sub     x9, x9, x8, lsl #16    // secondary N: SP -= N * 64 KiB (own stack)
    mov     sp, x9
1:  stp     x0, x1, [sp, #-80]!    // push the x0-x8 handoff; sp -> Handoff base
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    str     x8, [sp, #64]
    mov     x0, sp                 // &Handoff
    b       main
"#
);

unsafe extern "C" {
    /// This image's entry address (the `_start` shim), written to a core's
    /// mailbox slot to release it into the same image.
    static _start: u8;
}

/// Payload body, reached from the `_start` shim with a pointer to the captured
/// [`Handoff`]. Runs on every core that enters.
// The pointer comes from the entry shim (an FFI boundary), not arbitrary callers.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn main(handoff: *const Handoff) -> ! {
    let mut uart = Uart;
    // SAFETY: the shim pushed a valid `Handoff` at this address on the current
    // core's stack, above `main`'s frame, so it stays live for this read.
    let h = unsafe { &*handoff };

    // CurrentEL[3:2] is the exception level: proves the loader dropped us to EL1.
    let current_el: u64;
    // SAFETY: reads a system register.
    unsafe { asm!("mrs {}, CurrentEL", out(reg) current_el) };

    if h.core_id != 0 {
        // A secondary the boot core released via the mailbox.
        let _ = writeln!(
            uart,
            "[core {}] up at EL{}, sp own, abi {}, mailbox {:#x}",
            h.core_id,
            current_el >> 2,
            h.abi_version,
            h.mailbox
        );
        park();
    }
    let (load_addr, image_len, win_min, win_max, dtb, mailbox) = (
        h.load_addr,
        h.image_len,
        h.win_min,
        h.win_max,
        h.dtb,
        h.mailbox,
    );

    // FP/SIMD smoke test: if the trap were still set this faults. `black_box`
    // stops the multiply being const-folded, so a real `fmul` runs. Printed as
    // an integer to avoid pulling in float formatting.
    let a = core::hint::black_box(2.0f64);
    let b = core::hint::black_box(3.0f64);
    let fp = (a * b) as u64;

    let _ = writeln!(uart, "\n[payload-example] running");
    let _ = writeln!(uart, "  EL           = {}", current_el >> 2);
    let _ = writeln!(uart, "  x0 load_addr = {load_addr:#x}");
    let _ = writeln!(uart, "  x1 image_len = {image_len:#x}");
    let _ = writeln!(uart, "  x2 win_min   = {win_min:#x}");
    let _ = writeln!(uart, "  x3 win_max   = {win_max:#x}");
    let _ = writeln!(uart, "  x4 dtb       = {dtb:#x}");
    let _ = writeln!(uart, "  x5 dtb_size  = {:#x}", h.dtb_size);
    let _ = writeln!(uart, "  x6 abi_ver   = {}", h.abi_version);
    let _ = writeln!(uart, "  x7 mailbox   = {mailbox:#x}");
    let _ = writeln!(uart, "  x8 core_id   = {}", h.core_id);
    let _ = writeln!(uart, "  fp 2.0*3.0   = {fp}");

    // BSS zeroing check. `SCRATCH` is in .bss, so it is never transferred; it is
    // zero here only if the loader zero-filled the memsz tail. Read it through
    // volatile loads so the check reflects real RAM, not the compiler's
    // knowledge of the static's initial value.
    let bss = (&raw mut SCRATCH).cast::<u8>();
    let mut nonzero = 0u32;
    for i in 0..SCRATCH_LEN {
        // SAFETY: `bss` is the base of the SCRATCH array; `i < SCRATCH_LEN`.
        if unsafe { read_volatile(bss.add(i)) } != 0 {
            nonzero += 1;
        }
    }
    let _ = writeln!(uart, "  bss[{SCRATCH_LEN}] nz = {nonzero} (expect 0)");

    // Dirty the BSS before parking, so a *second* load (no power cycle) proves
    // the loader re-zeroed it rather than merely finding fresh zeros.
    for i in 0..SCRATCH_LEN {
        // SAFETY: writing within SCRATCH's bounds.
        unsafe { write_volatile(bss.add(i), 0xAA) };
    }

    let _ = writeln!(uart, "[payload-example] core 0 done.");

    // SMP smoke test, as the last thing core 0 does so its own output is out
    // first: release core 1 by writing this image's entry to its mailbox slot
    // (`x7 + 1*8`) and signalling an event. Core 1 re-enters `_start` with the
    // loader's handoff (its own `x8 = 1`) and reports in above.
    if mailbox != 0 {
        let _ = writeln!(uart, "[payload-example] starting core 1...");
        let slot1 = (mailbox + 8) as *mut u64;
        let entry = (&raw const _start) as u64;
        // SAFETY: `slot1` is core 1's release word in the loader-owned mailbox;
        // MMU off, so the store is non-cacheable and visible to the parked core.
        unsafe {
            write_volatile(slot1, entry);
            asm!("dsb sy", "sev");
        }
    }

    // Jump right back into the loader via the EL2 reload service, so the loader
    // is ready for the next `cargo pi load` without a power cycle. `HVC #0` on the
    // boot core does not return; the loader re-parks core 1 first, so re-loading
    // is safe even though this run started a secondary.
    let _ = writeln!(uart, "[payload-example] HVC #0 -> loader reload.");
    // SAFETY: traps to the resident EL2 loader, which reloads and never returns.
    unsafe { asm!("hvc #0") };
    park(); // unreachable on the boot core; a stray return still parks safely
}

/// Idle the core forever.
fn park() -> ! {
    loop {
        // SAFETY: wait-for-event to idle the core.
        unsafe { asm!("wfe") };
    }
}

/// Nothing to unwind to on bare metal; park.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
