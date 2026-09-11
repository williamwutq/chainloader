//! Minimal ACT-LED blink — a visible "the loader is alive" sign, no serial
//! adapter required.
//!
//! On the Raspberry Pi Zero 2 W the green ACT LED is on GPIO29, active-low (from
//! the board device tree; the same pin as the Pi 3B+). The loader drives it
//! directly, so a short blink burst at boot confirms `kernel8.img` loaded and
//! ran even before — or without — the UART.

use core::ptr::{read_volatile, write_volatile};

const PERIPHERAL_BASE: usize = 0x3F00_0000;
const GPIO_BASE: usize = PERIPHERAL_BASE + 0x0020_0000;
const GPFSEL2: usize = GPIO_BASE + 0x08; // GPIO20–29 function select
const GPSET0: usize = GPIO_BASE + 0x1C; // drive GPIO0–31 high
const GPCLR0: usize = GPIO_BASE + 0x28; // drive GPIO0–31 low

/// Green ACT LED pin on the Zero 2 W (active-low: low = on).
const ACT_PIN: u32 = 29;

/// Low word of the free-running 1 MHz system timer, for coarse delays.
const ST_CLO: usize = PERIPHERAL_BASE + 0x0000_3004;

/// Drives the LED on (active-low → pin low).
#[inline]
fn on() {
    // SAFETY: fixed GPIO output-clear register.
    unsafe { write_volatile(GPCLR0 as *mut u32, 1 << ACT_PIN) };
}

/// Drives the LED off (pin high).
#[inline]
fn off() {
    // SAFETY: fixed GPIO output-set register.
    unsafe { write_volatile(GPSET0 as *mut u32, 1 << ACT_PIN) };
}

/// Busy-waits about `us` microseconds against the 1 MHz system timer, which
/// free-runs from power-on and so needs no setup.
fn delay_us(us: u32) {
    // SAFETY: fixed, read-only timer register.
    let start = unsafe { read_volatile(ST_CLO as *const u32) };
    while unsafe { read_volatile(ST_CLO as *const u32) }.wrapping_sub(start) < us {
        core::hint::spin_loop();
    }
}

/// Sets GPIO29 to output, blinks the ACT LED `blinks` times (200 ms on/off),
/// and leaves it lit as a steady "loader up, waiting" indicator.
///
/// # Safety
///
/// Performs raw MMIO to the GPIO block. Run once, early, on the boot core.
pub unsafe fn signal_alive(blinks: u32) {
    // GPIO29 function select = output (0b001) in GPFSEL2 bits [29:27].
    let shift = (ACT_PIN - 20) * 3;
    // SAFETY: fixed GPIO function-select register, boot-core-only.
    unsafe {
        let mut sel = read_volatile(GPFSEL2 as *const u32);
        sel &= !(0b111 << shift);
        sel |= 0b001 << shift;
        write_volatile(GPFSEL2 as *mut u32, sel);
    }

    off();
    for _ in 0..blinks {
        on();
        delay_us(200_000); // 200 ms on
        off();
        delay_us(200_000); // 200 ms off
    }
    on(); // steady on: "loader running, waiting for host"
}
