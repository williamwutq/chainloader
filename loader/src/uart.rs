//! Minimal PL011 (UART0) driver for the Raspberry Pi Zero 2 W and Pi 2/3 — the
//! BCM2836/BCM2837 family, which share the `0x3F00_0000` peripheral base.
//!
//! Register offsets and the GPIO14/15 ALT0 routing follow the BCM2835/2837
//! peripheral manual. [`Uart::init`] computes the baud divisors from the
//! firmware's default 48 MHz PL011 reference clock (see `UART_CLOCK_HZ`).
//!
//! Routing note: on the Bluetooth-equipped boards (Zero 2 W, Pi 3) the firmware
//! wires PL011 to the on-board Bluetooth modem via GPIO32/33 by default. Rather
//! than depend on a `dtoverlay=disable-bt` in `config.txt`, [`Uart::init`]
//! returns GPIO32/33 to plain inputs itself, disconnecting the BT UART so PL011
//! reaches only the GPIO14/15 header pins where the USB-TTL adapter connects.
//! The Pi 2 has no Bluetooth and is unaffected.

use core::ptr::{read_volatile, write_volatile};

/// Peripheral base shared by the BCM2836/BCM2837 family (Pi 2, Pi 3, Zero 2 W).
/// The Pi 1 / Zero base is `0x2000_0000`.
const PERIPHERAL_BASE: usize = 0x3F00_0000;

const GPIO_BASE: usize = PERIPHERAL_BASE + 0x0020_0000;
const GPFSEL1: usize = GPIO_BASE + 0x04; // GPIO10–19
const GPFSEL3: usize = GPIO_BASE + 0x0C; // GPIO30–39 (the BT UART pins)
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

/// Target line rate, 8N1.
const BAUD: u32 = 115_200;
/// PL011 reference clock (`UARTCLK`) in Hz. The Raspberry Pi firmware runs the
/// PL011 from a 48 MHz reference by default when `enable_uart=1` is set (as it
/// is on the card's `config.txt`), and — unlike the mini-UART — this clock does
/// not track the core frequency, so it is stable without pinning.
///
/// An earlier version asked the mailbox to pin this to 4 MHz and read the rate
/// back to derive the divisor, but on real hardware the firmware neither honored
/// the 4 MHz request nor reported the true 48 MHz rate (it echoed the request),
/// so the loader computed a 12x-too-slow divisor and transmitted unreadable
/// ~1.38 Mbaud. Trusting the documented default is both simpler and correct.
const UART_CLOCK_HZ: u32 = 48_000_000;

/// PL011 integer/fractional baud divisors for `baud` at `clock_hz`.
///
/// `BAUDDIV = clock / (16 * baud)`; the integer part goes in `IBRD` and the
/// fraction, in 64ths, in `FBRD`. Computed as `div64 = round(64 * clock /
/// (16 * baud)) = round(4 * clock / baud)`, then split. At 48 MHz / 115200 this
/// yields `(26, 3)`.
const fn baud_divisors(clock_hz: u32, baud: u32) -> (u32, u32) {
    let div64 = (4 * clock_hz + baud / 2) / baud;
    (div64 / 64, div64 % 64)
}

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
            // Derive the baud divisors from the firmware's default 48 MHz PL011
            // reference clock (see UART_CLOCK_HZ). Not routed through the mailbox:
            // on real hardware the clock-rate tags misreported the rate, yielding
            // an unreadable baud.
            let (ibrd, fbrd) = baud_divisors(UART_CLOCK_HZ, BAUD);

            // Disable the UART before reconfiguring.
            write_volatile(UART_CR as *mut u32, 0);

            // Route GPIO14 (TXD) and GPIO15 (RXD) to ALT0 (= 0b100).
            let mut sel = read_volatile(GPFSEL1 as *const u32);
            sel &= !((0b111 << 12) | (0b111 << 15));
            sel |= (0b100 << 12) | (0b100 << 15);
            write_volatile(GPFSEL1 as *mut u32, sel);

            // Free PL011 from the on-board Bluetooth on BCM2837 boards (Pi 3 /
            // Zero 2 W): the firmware routes it to GPIO32 (TXD0) / GPIO33 (RXD0,
            // ALT3), and leaving GPIO33 on ALT3 makes it a second PL011 RXD
            // source alongside GPIO15 above, garbling receive. Returning GPIO32
            // and GPIO33 to plain inputs disconnects the BT UART, so the loader
            // needs no `disable-bt` overlay. (Harmless on the Pi 2, which has no
            // Bluetooth: those pins are inputs already.)
            let mut sel3 = read_volatile(GPFSEL3 as *const u32);
            sel3 &= !(0b111_111 << 6); // GPIO32 = FSEL[8:6], GPIO33 = FSEL[11:9] -> input
            write_volatile(GPFSEL3 as *mut u32, sel3);

            // Disable pull-up/down on pins 14 and 15.
            write_volatile(GPPUD as *mut u32, 0);
            delay(150);
            write_volatile(GPPUDCLK0 as *mut u32, (1 << 14) | (1 << 15));
            delay(150);
            write_volatile(GPPUDCLK0 as *mut u32, 0);

            // Clear pending interrupts, program baud, frame format, then enable.
            write_volatile(UART_ICR as *mut u32, 0x7FF);
            write_volatile(UART_IBRD as *mut u32, ibrd);
            write_volatile(UART_FBRD as *mut u32, fbrd);
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

    /// Receives one byte if the FIFO has one, else returns `None` immediately.
    #[inline]
    pub fn try_get_byte(&self) -> Option<u8> {
        // SAFETY: FR and DR are valid, fixed peripheral registers.
        unsafe {
            if read_volatile(UART_FR as *const u32) & FR_RXFE != 0 {
                None
            } else {
                Some((read_volatile(UART_DR as *const u32) & 0xFF) as u8)
            }
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
