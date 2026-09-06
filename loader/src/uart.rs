//! Minimal PL011 (UART0) driver for the Raspberry Pi Zero 2 W and Pi 2/3 — the
//! BCM2836/BCM2837 family, which share the `0x3F00_0000` peripheral base.
//!
//! Register offsets and the GPIO14/15 ALT0 routing follow the BCM2835/2837
//! peripheral manual. Rather than assume `config.txt` sets a particular UART
//! reference clock, [`Uart::init`] pins the clock to a known rate via the
//! VideoCore mailbox (see [`crate::mailbox`]) and computes the baud divisors for
//! it. Written against the datasheet and the bztsrc reference; not yet validated
//! on hardware (see `../PLANNED.md`).
//!
//! Routing note: on the Bluetooth-equipped boards (Zero 2 W, Pi 3) the firmware
//! wires PL011 to the on-board Bluetooth modem by default and hands the
//! GPIO14/15 header pins the mini-UART, so `config.txt` must carry
//! `dtoverlay=disable-bt` (or `miniuart-bt`) to route PL011 to the header pins
//! where the USB-TTL adapter connects. The Pi 2 has no Bluetooth and needs no
//! such overlay.

use core::ptr::{read_volatile, write_volatile};

use crate::mailbox;

/// Peripheral base shared by the BCM2836/BCM2837 family (Pi 2, Pi 3, Zero 2 W).
/// The Pi 1 / Zero base is `0x2000_0000`.
const PERIPHERAL_BASE: usize = 0x3F00_0000;

const GPIO_BASE: usize = PERIPHERAL_BASE + 0x0020_0000;
const GPFSEL1: usize = GPIO_BASE + 0x04;
const GPPUD: usize = GPIO_BASE + 0x94;
const GPPUDCLK0: usize = GPIO_BASE + 0x98;

const UART0_BASE: usize = PERIPHERAL_BASE + 0x0020_1000;
const UART_DR: usize = UART0_BASE; // + 0x00
const UART_FR: usize = UART0_BASE + 0x18;
const UART_IBRD: usize = UART0_BASE + 0x24;
const UART_FBRD: usize = UART0_BASE + 0x28;
const UART_LCRH: usize = UART0_BASE + 0x2C;
const UART_CR: usize = UART0_BASE + 0x30;
const UART_IMSC: usize = UART0_BASE + 0x38;
const UART_ICR: usize = UART0_BASE + 0x44;

const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty

const LCRH_FEN: u32 = 1 << 4; // enable FIFOs
const LCRH_WLEN_8: u32 = 0b11 << 5; // 8-bit words

const CR_UARTEN: u32 = 1; // bit 0
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

/// UART reference clock the loader pins via the mailbox, in Hz.
const UART_CLOCK_HZ: u32 = 4_000_000;
/// Integer baud divisor for 115200 baud at [`UART_CLOCK_HZ`].
///
/// `4_000_000 / (16 * 115_200) = 2.170`.
const IBRD_115200: u32 = 2;
/// Fractional baud divisor: `round(0.170 * 64) = 11` (`0xB`).
const FBRD_115200: u32 = 0xB;

/// A zero-sized handle to the single PL011 peripheral.
pub struct Uart;

impl Uart {
    /// Configures GPIO14/15 for UART0 and brings the PL011 up at 115200 8N1.
    ///
    /// # Safety
    ///
    /// Must run once, early, on the boot core with no other UART user active.
    /// It performs raw MMIO writes to fixed peripheral addresses.
    pub unsafe fn init(&self) {
        unsafe {
            // Pin the UART reference clock so the divisors below are correct
            // regardless of `config.txt`. Best effort: if the mailbox call
            // fails, fall through with whatever clock the firmware set.
            let _ = mailbox::set_uart_clock(UART_CLOCK_HZ);

            // Disable the UART before reconfiguring.
            write_volatile(UART_CR as *mut u32, 0);

            // Route GPIO14 (TXD) and GPIO15 (RXD) to ALT0 (= 0b100).
            let mut sel = read_volatile(GPFSEL1 as *const u32);
            sel &= !((0b111 << 12) | (0b111 << 15));
            sel |= (0b100 << 12) | (0b100 << 15);
            write_volatile(GPFSEL1 as *mut u32, sel);

            // Disable pull-up/down on pins 14 and 15.
            write_volatile(GPPUD as *mut u32, 0);
            delay(150);
            write_volatile(GPPUDCLK0 as *mut u32, (1 << 14) | (1 << 15));
            delay(150);
            write_volatile(GPPUDCLK0 as *mut u32, 0);

            // Clear pending interrupts, program baud, frame format, then enable.
            write_volatile(UART_ICR as *mut u32, 0x7FF);
            write_volatile(UART_IBRD as *mut u32, IBRD_115200);
            write_volatile(UART_FBRD as *mut u32, FBRD_115200);
            write_volatile(UART_LCRH as *mut u32, LCRH_FEN | LCRH_WLEN_8);
            write_volatile(UART_IMSC as *mut u32, 0); // mask all UART interrupts
            write_volatile(UART_CR as *mut u32, CR_UARTEN | CR_TXE | CR_RXE);
        }
    }

    /// Sends one byte, blocking while the transmit FIFO is full.
    #[inline]
    pub fn put_byte(&self, byte: u8) {
        // SAFETY: FR and DR are valid, fixed peripheral registers.
        unsafe {
            while read_volatile(UART_FR as *const u32) & FR_TXFF != 0 {
                core::hint::spin_loop();
            }
            write_volatile(UART_DR as *mut u32, u32::from(byte));
        }
    }

    /// Receives one byte, blocking while the receive FIFO is empty.
    #[inline]
    pub fn get_byte(&self) -> u8 {
        // SAFETY: FR and DR are valid, fixed peripheral registers.
        unsafe {
            while read_volatile(UART_FR as *const u32) & FR_RXFE != 0 {
                core::hint::spin_loop();
            }
            (read_volatile(UART_DR as *const u32) & 0xFF) as u8
        }
    }

    /// Writes a string, translating `\n` into CR+LF for terminal friendliness.
    pub fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.put_byte(b'\r');
            }
            self.put_byte(byte);
        }
    }
}

impl core::fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        Uart::write_str(self, s);
        Ok(())
    }
}

/// Busy-waits for roughly `count` loop iterations. Coarse, for reset timing.
fn delay(count: u32) {
    for _ in 0..count {
        core::hint::spin_loop();
    }
}
