//! Raspberry Pi 2 AArch64 UART chainloader — Pi-side loader.
//!
//! Boots from SD as `kernel8.img`, brings up the PL011 UART, and (once the
//! protocol receive path lands) accepts an AArch64 image over the wire,
//! validates it, and jumps to it. See `../docs/PROTOCOL.md` for the wire format
//! and `../docs/ENTRY_CONTRACT.md` for the register/cache state established
//! before the jump.
//!
//! # Status
//!
//! This is the phase-2 skeleton: boot, UART bring-up, a diagnostic banner, and
//! a byte echo. The framed receive/validate/jump path is the next milestone in
//! `../PLANNED.md`; [`chainloader_protocol`] already provides the framing and
//! CRC it will use.
#![no_std]
#![no_main]

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
4:  bl      loader_main
5:  wfe                            // loader_main must not return; park if it does
    b       5b
"#
);

/// Rust entry point, called from the boot trampoline with a valid stack and
/// zeroed BSS. Must never return.
#[unsafe(no_mangle)]
pub extern "C" fn loader_main() -> ! {
    let uart = Uart;
    // SAFETY: first and only UART user, running on the boot core at startup.
    unsafe {
        uart.init();
    }

    let mut u = uart;
    let _ = writeln!(
        u,
        "\nchainloader {} (protocol v{})",
        env!("CARGO_PKG_VERSION"),
        chainloader_protocol::PROTOCOL_VERSION,
    );
    let _ = writeln!(
        u,
        "UART up @ 115200 8N1. Echoing bytes (receive path: TODO)."
    );

    // Placeholder until the framed protocol lands: echo received bytes so the
    // link can be smoke-tested end to end from the host.
    loop {
        let byte = u.get_byte();
        u.put_byte(byte);
    }
}

/// Nothing to unwind to on bare metal: report if the UART is up, then park.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
