//! Minimal example payload for the chainloader.
//!
//! Built for `aarch64-unknown-none` and linked at `0x200000`, this is the image
//! `cargo pi load` transfers and the loader jumps to. It exercises the entry
//! contract (`../docs/ENTRY_CONTRACT.md`): the loader hands it a live UART, a
//! stack, EL1 with FP/SIMD enabled, and the `x0`–`x4` register handoff. It
//! prints those and parks — a quick end-to-end check that the whole pipeline
//! (build → flatten → transfer → validate → jump) works on real hardware.

#![no_std]
#![no_main]

use core::arch::asm;
use core::fmt::Write;
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

// PL011 UART0, already brought up by the loader (115200 8N1); reused as-is.
const UART0_DR: usize = 0x3F20_1000;
const UART0_FR: usize = 0x3F20_1018;
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full

/// A small buffer that lands in `.bss`: zero-initialized, so it is *not* part of
/// the transferred image (only its `memsz` is). It reads as zero at entry only
/// if the loader zero-filled the image's BSS tail — see the check in `_start`.
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

/// Payload entry. The loader `ERET`s here at EL1 with the register handoff in
/// `x0`–`x4`, which map to these `extern "C"` parameters, and `SP` already set.
#[unsafe(no_mangle)]
pub extern "C" fn _start(
    load_addr: u64,
    image_len: u64,
    win_min: u64,
    win_max: u64,
    dtb: u64,
) -> ! {
    let mut uart = Uart;

    // CurrentEL[3:2] is the exception level: proves the loader dropped us to EL1.
    let current_el: u64;
    // SAFETY: reads a system register.
    unsafe { asm!("mrs {}, CurrentEL", out(reg) current_el) };

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

    let _ = writeln!(uart, "[payload-example] done; parking.");

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
