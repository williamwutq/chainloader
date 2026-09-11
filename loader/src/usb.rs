//! Work-in-progress USB device transport: a USB CDC-ACM serial gadget on the
//! Zero 2 W's micro-USB data port, driven by the Synopsys DWC2 OTG core.
//!
//! **Not yet wired into the loader.** This module is scaffolding for the planned
//! USB transport (`../PLANNED.md`, `usb-transport`) — a faster, single-cable,
//! adapter-free alternative to the UART. It defines the DWC2 register map and
//! the CDC-ACM descriptors (the pieces that can be pinned down from the
//! datasheet), and sketches device-mode bring-up. The enumeration and FIFO
//! transfer paths are stubbed: they are interrupt/poll-driven and need hardware
//! to develop and debug.
//!
//! When finished it will expose the same `get_byte` / `put_byte` shape as
//! [`crate::uart::Uart`], so the receive state machine can run over either
//! transport behind one `ByteStream` trait (extracting that trait, and wiring
//! this in, is the integration step — deliberately left undone here).
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

/// DWC2 OTG core base on BCM2837 (peripheral base + `0x98_0000`).
const USB_BASE: usize = 0x3F98_0000;

// --- Core global registers (offsets from USB_BASE) ---
const GOTGCTL: usize = USB_BASE; // + 0x00 OTG control and status
const GAHBCFG: usize = USB_BASE + 0x008; // AHB configuration
const GUSBCFG: usize = USB_BASE + 0x00C; // core USB configuration
const GRSTCTL: usize = USB_BASE + 0x010; // reset control
const GINTSTS: usize = USB_BASE + 0x014; // interrupt status
const GINTMSK: usize = USB_BASE + 0x018; // interrupt mask
const GRXSTSP: usize = USB_BASE + 0x020; // Rx status pop (read to dequeue)
const GRXFSIZ: usize = USB_BASE + 0x024; // Rx FIFO size
const GNPTXFSIZ: usize = USB_BASE + 0x028; // non-periodic Tx FIFO size

// --- Device-mode registers ---
const DCFG: usize = USB_BASE + 0x800; // device configuration
const DCTL: usize = USB_BASE + 0x804; // device control
const DSTS: usize = USB_BASE + 0x808; // device status
const DIEPMSK: usize = USB_BASE + 0x810; // IN-endpoint common interrupt mask
const DOEPMSK: usize = USB_BASE + 0x814; // OUT-endpoint common interrupt mask
const DAINTMSK: usize = USB_BASE + 0x81C; // per-endpoint interrupt mask

/// IN endpoint `n` register block base (`DIEPCTLn` at +0x00, then INT/TSIZ/…).
const DIEP_BASE: usize = USB_BASE + 0x900;
/// OUT endpoint `n` register block base.
const DOEP_BASE: usize = USB_BASE + 0xB00;
/// Stride between consecutive endpoint register blocks.
const DEP_STRIDE: usize = 0x20;
/// Per-endpoint data FIFO window base (push/pop `n` at +`n*0x1000`).
const DFIFO_BASE: usize = USB_BASE + 0x1000;
const DFIFO_STRIDE: usize = 0x1000;

const PCGCCTL: usize = USB_BASE + 0xE00; // power and clock gating

// --- Selected bit definitions ---
const GRSTCTL_CSRST: u32 = 1; // core soft reset (bit 0)
const GRSTCTL_AHBIDLE: u32 = 1 << 31; // AHB master idle
const GAHBCFG_GLBLINTRMSK: u32 = 1; // unmask the global interrupt (bit 0)
const GUSBCFG_PHYSEL_FS: u32 = 1 << 6; // select the full-speed serial PHY
const GUSBCFG_FORCEDEVMODE: u32 = 1 << 30; // force device mode
const DCTL_SFTDISCON: u32 = 1 << 1; // soft disconnect
const DCFG_DEVSPD_FS: u32 = 0b11; // device speed = full speed (internal PHY)

// Global interrupt-status/mask bits we care about in device mode.
const GINT_RXFLVL: u32 = 1 << 4; // Rx FIFO non-empty
const GINT_USBRST: u32 = 1 << 12; // USB reset detected
const GINT_ENUMDONE: u32 = 1 << 13; // enumeration (speed) done
const GINT_IEPINT: u32 = 1 << 18; // IN-endpoint interrupt
const GINT_OEPINT: u32 = 1 << 19; // OUT-endpoint interrupt

// --- CDC-ACM device descriptor (18 bytes) ---
// bDeviceClass = Communications (0x02). VID/PID are development placeholders
// (0x1209 = pid.codes, the open-source USB VID); the host binds its in-box
// CDC-ACM driver by class, not by VID/PID.
#[rustfmt::skip]
const DEVICE_DESCRIPTOR: [u8; 18] = [
    0x12, // bLength = 18
    0x01, // bDescriptorType = DEVICE
    0x00, 0x02, // bcdUSB = 2.00
    0x02, // bDeviceClass = CDC
    0x00, // bDeviceSubClass
    0x00, // bDeviceProtocol
    0x40, // bMaxPacketSize0 = 64
    0x09, 0x12, // idVendor = 0x1209 (placeholder)
    0x01, 0x00, // idProduct = 0x0001 (placeholder)
    0x00, 0x01, // bcdDevice = 1.00
    0x00, // iManufacturer (no string descriptors yet)
    0x00, // iProduct
    0x00, // iSerialNumber
    0x01, // bNumConfigurations = 1
];

// --- CDC-ACM configuration descriptor (67 bytes total) ---
// Config → CDC Communications interface (ACM, with the four functional
// descriptors + a notification interrupt-IN endpoint) → CDC-Data interface
// (bulk OUT + bulk IN). Endpoint map: EP1 IN = notify, EP2 OUT/IN = data.
#[rustfmt::skip]
const CONFIG_DESCRIPTOR: [u8; 67] = [
    // Configuration descriptor.
    0x09, 0x02, 0x43, 0x00, 0x02, 0x01, 0x00, 0xC0, 0x32,
    // Interface 0: CDC Communications, ACM.
    0x09, 0x04, 0x00, 0x00, 0x01, 0x02, 0x02, 0x01, 0x00,
    // CDC Header functional descriptor (bcdCDC 1.10).
    0x05, 0x24, 0x00, 0x10, 0x01,
    // CDC Call Management functional descriptor (none; data interface 1).
    0x05, 0x24, 0x01, 0x00, 0x01,
    // CDC ACM functional descriptor (supports line coding + serial state).
    0x04, 0x24, 0x02, 0x02,
    // CDC Union functional descriptor (control 0, subordinate 1).
    0x05, 0x24, 0x06, 0x00, 0x01,
    // Notification endpoint: EP1 IN, interrupt, 8-byte, bInterval 255.
    0x07, 0x05, 0x81, 0x03, 0x08, 0x00, 0xFF,
    // Interface 1: CDC Data.
    0x09, 0x04, 0x01, 0x00, 0x02, 0x0A, 0x00, 0x00, 0x00,
    // Bulk OUT endpoint: EP2 OUT, 64-byte.
    0x07, 0x05, 0x02, 0x02, 0x40, 0x00, 0x00,
    // Bulk IN endpoint: EP2 IN, 64-byte.
    0x07, 0x05, 0x82, 0x02, 0x40, 0x00, 0x00,
];

/// A handle to the single DWC2 USB device controller.
pub(crate) struct Usb;

impl Usb {
    /// Brings up the DWC2 core in device mode and soft-connects the CDC-ACM
    /// gadget. The enumeration and transfer handling that must follow is not yet
    /// implemented (see the module docs).
    ///
    /// # Safety
    ///
    /// Performs raw MMIO to the USB controller. Run once, early, on the boot
    /// core with no other USB user active.
    pub(crate) unsafe fn init(&self) {
        // SAFETY: fixed USB controller registers; single early caller.
        unsafe {
            // Wait for the AHB master to go idle, then soft-reset the core.
            while read_volatile(GRSTCTL as *const u32) & GRSTCTL_AHBIDLE == 0 {
                core::hint::spin_loop();
            }
            write_volatile(GRSTCTL as *mut u32, GRSTCTL_CSRST);
            while read_volatile(GRSTCTL as *const u32) & GRSTCTL_CSRST != 0 {
                core::hint::spin_loop();
            }

            // Select the full-speed internal PHY and force device mode.
            let mut usbcfg = read_volatile(GUSBCFG as *const u32);
            usbcfg |= GUSBCFG_PHYSEL_FS | GUSBCFG_FORCEDEVMODE;
            write_volatile(GUSBCFG as *mut u32, usbcfg);

            // Configure the device for full speed.
            let mut dcfg = read_volatile(DCFG as *const u32);
            dcfg |= DCFG_DEVSPD_FS;
            write_volatile(DCFG as *mut u32, dcfg);

            // TODO(usb): size the FIFOs (GRXFSIZ / GNPTXFSIZ / DIEPTXFn), unmask
            // the device interrupts we service (USBRST, ENUMDONE, RXFLVL, IN/OUT
            // endpoint) and set the GAHBCFG global interrupt enable.

            // Soft-connect: clear SFTDISCON so the host detects the device.
            let mut dctl = read_volatile(DCTL as *const u32);
            dctl &= !DCTL_SFTDISCON;
            write_volatile(DCTL as *mut u32, dctl);
        }

        // TODO(usb): the rest is interrupt/poll-driven and needs hardware:
        //  - on USBRST: flush FIFOs, arm EP0 for the next SETUP packet.
        //  - on ENUMDONE: read the enumerated speed, set EP0 max packet size.
        //  - on RXFLVL: pop GRXSTSP and route SETUP / OUT-data packets.
        //  - standard requests: GET_DESCRIPTOR (device/config), SET_ADDRESS,
        //    SET_CONFIGURATION; CDC class: SET_LINE_CODING / SET_CONTROL_LINE.
        //  - open the bulk IN/OUT endpoints of the CDC-Data interface.
    }

    /// Blocks until one received byte is available.
    ///
    /// TODO(usb): drain the bulk-OUT endpoint FIFO once enumeration works. This
    /// placeholder mirrors [`crate::uart::Uart::get_byte`] for the future
    /// `ByteStream` trait and is not yet functional.
    pub(crate) fn get_byte(&self) -> u8 {
        0
    }

    /// Queues one byte for transmission.
    ///
    /// TODO(usb): push to the bulk-IN endpoint FIFO. Placeholder, not yet
    /// functional.
    pub(crate) fn put_byte(&self, _byte: u8) {}
}
