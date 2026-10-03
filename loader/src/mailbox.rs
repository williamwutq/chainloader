//! Minimal VideoCore mailbox client (property channel).
//!
//! Used to query the ARM-visible RAM size, so the writable window is sized to
//! the board rather than hardcoded. It works at early boot with the MMU off
//! because ARM data accesses then bypass the caches, so the VideoCore sees our
//! request buffer and we see its response without cache maintenance.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, compiler_fence};

/// Mailbox register block base (peripheral base + 0xB880).
const MBOX_BASE: usize = 0x3F00_B880;
const MBOX_READ: usize = MBOX_BASE; // + 0x00
const MBOX_STATUS: usize = MBOX_BASE + 0x18;
const MBOX_WRITE: usize = MBOX_BASE + 0x20;

/// Status bit: outbound mailbox is full.
const MBOX_FULL: u32 = 0x8000_0000;
/// Status bit: inbound mailbox is empty.
const MBOX_EMPTY: u32 = 0x4000_0000;

/// Property-tags channel (ARM → VideoCore).
const CHANNEL_PROP: u32 = 8;

const REQUEST_CODE: u32 = 0x0000_0000;
const RESPONSE_SUCCESS: u32 = 0x8000_0000;
const TAG_GET_ARM_MEMORY: u32 = 0x0001_0005;
const TAG_GET_CLOCK_RATE: u32 = 0x0003_0002;
const TAG_SET_CLOCK_RATE: u32 = 0x0003_8002;

/// VideoCore clock id for the ARM core clock (the frequency the A53 runs at).
pub const CLOCK_ID_ARM: u32 = 0x3;

/// A 16-byte-aligned property-message buffer. The mailbox requires the message
/// address to be 16-aligned (its low nibble carries the channel number).
#[repr(C, align(16))]
struct Message {
    words: [u32; 9],
}

/// Rings the mailbox doorbell for `msg` and waits for the response, returning
/// `true` if the VideoCore reported success. The response is written back into
/// `msg` in place.
///
/// # Safety
///
/// Performs raw MMIO to the mailbox registers. Run on the boot core with the
/// MMU off, so ARM accesses bypass the caches and stay coherent with VideoCore.
unsafe fn exchange(msg: &mut Message) -> bool {
    // A raw pointer the VideoCore writes its response through.
    let base = (&raw mut *msg).cast::<u32>();
    let addr = base as usize as u32;
    // Low nibble must be free for the channel; the type is 16-aligned.
    let write_val = (addr & !0xF) | CHANNEL_PROP;

    // Ensure the buffer is fully written before the doorbell.
    compiler_fence(Ordering::SeqCst);

    // SAFETY: fixed mailbox registers; `msg` outlives the synchronous exchange.
    unsafe {
        while read_volatile(MBOX_STATUS as *const u32) & MBOX_FULL != 0 {
            core::hint::spin_loop();
        }
        write_volatile(MBOX_WRITE as *mut u32, write_val);

        // Wait for the response addressed to our channel.
        loop {
            while read_volatile(MBOX_STATUS as *const u32) & MBOX_EMPTY != 0 {
                core::hint::spin_loop();
            }
            if read_volatile(MBOX_READ as *const u32) == write_val {
                break;
            }
        }

        compiler_fence(Ordering::SeqCst);
        // words[1] holds the response code the VideoCore wrote back.
        read_volatile(base.add(1)) == RESPONSE_SUCCESS
    }
}

/// Queries the ARM-visible RAM region as `(base, size)` in bytes — already net
/// of the VideoCore `gpu_mem` split. Returns `None` if the exchange fails.
///
/// # Safety
///
/// Performs raw MMIO to the mailbox registers. Run once, early, on the boot core.
pub unsafe fn arm_memory() -> Option<(u64, u64)> {
    let mut msg = Message {
        words: [
            8 * 4,              // total size in bytes
            REQUEST_CODE,       // request
            TAG_GET_ARM_MEMORY, // tag
            8,                  // value buffer size (base + size)
            0,                  // tag request code
            0,                  // base (response)
            0,                  // size (response)
            0,                  // end tag
            0,                  // unused
        ],
    };
    // SAFETY: boot core, MMU off; a single synchronous mailbox exchange.
    if !unsafe { exchange(&mut msg) } {
        return None;
    }
    // SAFETY: on success the VideoCore wrote base into words[5], size into [6].
    let base = unsafe { read_volatile(&raw const msg.words[5]) };
    let size = unsafe { read_volatile(&raw const msg.words[6]) };
    Some((u64::from(base), u64::from(size)))
}

/// Current rate of `clock_id` in Hz, or `None` on a failed exchange or a zero
/// rate (the VideoCore reports `0` for an unknown clock id). The value buffer is
/// `[clock_id (in), rate (out)]`.
///
/// # Safety
///
/// Performs raw MMIO to the mailbox registers. Run on the boot core with the MMU
/// off, so ARM accesses bypass the caches and stay coherent with VideoCore.
pub unsafe fn get_clock_rate(clock_id: u32) -> Option<u32> {
    let mut msg = Message {
        words: [
            8 * 4,              // total size in bytes (8 words used)
            REQUEST_CODE,       // request
            TAG_GET_CLOCK_RATE, // tag
            8,                  // value buffer size (clock_id + rate)
            0,                  // tag request code
            clock_id,           // in: clock id
            0,                  // out: rate
            0,                  // end tag
            0,                  // unused
        ],
    };
    // SAFETY: boot core, MMU off; a single synchronous mailbox exchange.
    if !unsafe { exchange(&mut msg) } {
        return None;
    }
    // SAFETY: on success the VideoCore wrote the rate into words[6].
    let rate = unsafe { read_volatile(&raw const msg.words[6]) };
    (rate != 0).then_some(rate)
}

/// Sets `clock_id` to `rate` Hz (with the matching turbo/voltage change, so a
/// raised clock actually holds), returning the rate the VideoCore reports it
/// applied, or `None` on a failed exchange. The value buffer is `[clock_id,
/// rate, skip_setting_turbo]`; `skip_setting_turbo = 0` lets the firmware raise
/// the voltage with the clock.
///
/// # Safety
///
/// Performs raw MMIO to the mailbox registers. Run on the boot core with the MMU
/// off, so ARM accesses bypass the caches and stay coherent with VideoCore.
pub unsafe fn set_clock_rate(clock_id: u32, rate: u32) -> Option<u32> {
    let mut msg = Message {
        words: [
            9 * 4,              // total size in bytes (9 words used)
            REQUEST_CODE,       // request
            TAG_SET_CLOCK_RATE, // tag
            12,                 // value buffer size (clock_id + rate + skip_turbo)
            0,                  // tag request code
            clock_id,           // in: clock id
            rate,               // in: requested rate; out: applied rate
            0,                  // skip_setting_turbo = 0 (raise voltage with clock)
            0,                  // end tag
        ],
    };
    // SAFETY: boot core, MMU off; a single synchronous mailbox exchange.
    if !unsafe { exchange(&mut msg) } {
        return None;
    }
    // SAFETY: on success the VideoCore wrote the applied rate into words[6].
    Some(unsafe { read_volatile(&raw const msg.words[6]) })
}
