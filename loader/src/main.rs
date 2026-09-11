//! Raspberry Pi Zero 2 W / Pi 2 AArch64 UART chainloader — Pi-side loader.
//!
//! Boots from SD as `kernel8.img`, brings up the PL011 UART, accepts an AArch64
//! image over the wire, validates it, and jumps to it. See `../docs/PROTOCOL.md`
//! for the wire format and `../docs/ENTRY_CONTRACT.md` for the register/cache
//! state established before the jump.
//!
//! # Status
//!
//! Boot, UART bring-up, and the framed receive/validate/jump path are in place
//! (see [`receive`]); the loader has not yet been validated on hardware. Cache
//! maintenance and timeout behavior are the remaining open questions in
//! `../PLANNED.md`.
#![no_std]
#![no_main]

mod led;
mod mailbox;
mod receive;
mod uart;

use core::arch::global_asm;
use core::fmt::Write as _;
use core::panic::PanicInfo;

use uart::Uart;

// Boot trampoline. Placed at 0x80000 by the linker via `.text.boot`. Parks the
// three secondary cores, points SP at the reserved stack, zeroes BSS, and calls
// `loader_main`. Kept in assembly because none of it is expressible before a
// stack and zeroed statics exist.
global_asm!(
    r#"
.section .text.boot
.global _start
_start:
    mov     x19, x0                // preserve the firmware's DTB pointer (x0) before we clobber it
    mrs     x0, mpidr_el1
    and     x0, x0, #0xFF          // core id in the low bits
    cbz     x0, 2f
1:  wfe                            // secondary cores: park forever
    b       1b
2:  ldr     x0, =__stack_top
    mov     sp, x0
    ldr     x1, =__bss_start
    ldr     x2, =__bss_end
3:  cmp     x1, x2
    b.hs    4f
    str     xzr, [x1], #8
    b       3b
4:  // Enable FP/SIMD (NEON) so compiled SIMD in the loader — and the payload it
    // hands off to — does not trap. At EL2 the trap is CPTR_EL2.TFP; CPACR_EL1
    // covers any later execution at EL1/EL0.
    mrs     x0, CurrentEL
    cmp     x0, #(2 << 2)          // running at EL2?
    b.ne    6f
    mrs     x0, cptr_el2
    bic     x0, x0, #(1 << 10)     // CPTR_EL2.TFP = 0: do not trap FP/SIMD at EL2
    msr     cptr_el2, x0
6:  mov     x0, #(3 << 20)         // CPACR_EL1.FPEN = 0b11: no trap at EL1/EL0
    msr     cpacr_el1, x0
    isb
    mov     x0, x19                // pass the DTB pointer as loader_main's first argument
    bl      loader_main
5:  wfe                            // loader_main must not return; park if it does
    b       5b
"#
);

/// Rust entry point, called from the boot trampoline with a valid stack and
/// zeroed BSS. `dtb` is the device-tree-blob pointer the firmware passed in `x0`
/// (0 if none), forwarded to the loaded image. Must never return.
#[unsafe(no_mangle)]
pub extern "C" fn loader_main(dtb: u64) -> ! {
    // Blink the ACT LED first: a visible sign the loader booted, even with no
    // serial adapter attached. Left lit afterwards as a "waiting for host" light.
    // SAFETY: boot core, first and only GPIO/LED user at startup.
    unsafe { led::signal_alive(3) };

    let mut uart = Uart;
    // SAFETY: first and only UART user, running on the boot core at startup.
    unsafe {
        uart.init();
    }

    let _ = writeln!(
        uart,
        "\nchainloader {} (protocol v{})",
        env!("CARGO_PKG_VERSION"),
        chainloader_protocol::PROTOCOL_VERSION,
    );
    let _ = writeln!(uart, "UART up @ 115200 8N1. Waiting for host (HELLO).");

    // Hand off to the protocol state machine; it never returns (it either
    // branches to a loaded image or keeps servicing the link).
    receive::run(uart, dtb)
}

/// Nothing to unwind to on bare metal: report if the UART is up, then park.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
