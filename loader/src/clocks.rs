//! Clock establishment and the entry-contract clock guarantees.
//!
//! The entry contract pins down *rates*, not just which clocks are accessible:
//!
//! - The architected generic timer is the payload's timebase. `CNTFRQ_EL0` is
//!   guaranteed valid — nonzero and consistent with the independent 1 MHz BCM
//!   system timer — so a payload that derives delays from `CNTFRQ_EL0` /
//!   `CNTVCT_EL0` gets correct wall-clock time. An invalid `CNTFRQ_EL0` is fatal:
//!   the loader refuses to hand off rather than let a payload inherit a broken
//!   timebase (see [`establish`]).
//! - The ARM **core** frequency is firmware-managed and not self-describing at
//!   EL1 (no architected register reports it). The loader requests 1 GHz — the
//!   Zero 2 W's rated maximum — over the VideoCore mailbox (the firmware clamps
//!   to its real max, so this never overclocks) and reports the achieved rate to
//!   the payload in `x9`, so a payload that must know its core clock (e.g. to
//!   calibrate a cycle-based busy-loop) has a definite value. The guarantee is
//!   best-effort: firmware thermal/undervoltage throttling can still move the
//!   core clock after handoff, so a payload needing exact time must use the
//!   generic timer, not a cycle count.
//!
//! See `../docs/ENTRY_CONTRACT.md`.

use core::arch::asm;
use core::fmt::Write as _;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::mailbox;
use crate::uart::Uart;

/// Low word of the free-running 1 MHz BCM system timer — a reference clock
/// independent of both the ARM core clock and the generic timer, used to sanity
/// check `CNTFRQ_EL0`.
const ST_CLO: usize = 0x3F00_3004;

/// Cross-check interval, in 1 MHz system-timer ticks (10 ms). Long enough that
/// the implied generic-timer frequency is accurate to well under 1%.
const CHECK_US: u32 = 10_000;

/// Allowed deviation, in percent, between `CNTFRQ_EL0` and the frequency implied
/// by counting `CNTVCT_EL0` ticks over a fixed system-timer interval. The two
/// clocks share the SoC crystal, so they track closely; 10% only rejects a
/// grossly wrong `CNTFRQ_EL0` (e.g. firmware left it zero-ish or bogus).
const CNTFRQ_TOLERANCE_PCT: u64 = 10;

/// Target ARM core clock, in Hz: 1 GHz, the Raspberry Pi Zero 2 W's rated
/// maximum. Requested via the mailbox, which clamps to the firmware's configured
/// maximum (so a board or `config.txt` with a lower cap is honored, never
/// overclocked); the rate read back afterward is what actually applied.
const TARGET_ARM_HZ: u32 = 1_000_000_000;

/// The pinned ARM core frequency in Hz, handed to the payload in `x9`. Set by
/// [`establish`]; `0` only before the first establish (never handed off).
static CORE_FREQ_HZ: AtomicU32 = AtomicU32::new(0);

/// The pinned ARM core frequency in Hz, for the `x9` handoff (boot core and
/// secondaries). Valid once [`establish`] has run.
pub fn core_freq_hz() -> u32 {
    CORE_FREQ_HZ.load(Ordering::Relaxed)
}

/// `CNTFRQ_EL0` — the generic-timer frequency firmware programmed, in Hz.
fn cntfrq() -> u64 {
    let hz: u64;
    // SAFETY: a plain system-register read, always permitted at EL2.
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) hz, options(nomem, nostack)) };
    hz
}

/// `CNTVCT_EL0` — the generic-timer virtual count. `ISB` first so the read is
/// ordered after prior instructions (the counter is otherwise freely reordered).
fn cntvct() -> u64 {
    let cnt: u64;
    // SAFETY: a plain system-register read, always permitted at EL2.
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) cnt, options(nomem, nostack)) };
    cnt
}

/// The 1 MHz system-timer count (low 32 bits).
fn systimer() -> u32 {
    // SAFETY: fixed, read-only system-timer counter register.
    unsafe { read_volatile(ST_CLO as *const u32) }
}

/// Reports the fatal clock fault over the UART and parks the core. A payload is
/// never entered with a timebase the loader could not vouch for.
fn fatal(uart: &mut Uart, msg: &str, detail: u64) -> ! {
    let _ = writeln!(
        uart,
        "\nchainloader: FATAL clock check: {msg} ({detail:#x})."
    );
    let _ = writeln!(uart, "chainloader: refusing to boot a payload. Halting.");
    loop {
        // SAFETY: park; nothing will wake it (boot core, no handler installed).
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// Establishes the entry-contract clock guarantees on the boot core, or halts.
///
/// 1. Validates `CNTFRQ_EL0`: nonzero and within [`CNTFRQ_TOLERANCE_PCT`] of the
///    frequency implied by counting `CNTVCT_EL0` against the 1 MHz system timer.
///    A failure is fatal ([`fatal`]) — the generic timer is the payload's
///    timebase, so a wrong rate must not reach it.
/// 2. Pins the ARM core clock to the board maximum and records the rate read
///    back, for the `x9` handoff. Inability to read any ARM rate is also fatal,
///    since the contract then cannot report a core frequency.
///
/// Idempotent: safe to re-run on an `HVC #0` reload to restore the pin after a
/// payload may have changed the core clock.
///
/// # Safety
///
/// Boot core, MMU off (so the mailbox exchange is coherent with VideoCore).
pub unsafe fn establish(uart: &mut Uart) {
    // --- 1. Validate the generic-timer frequency against the system timer. ---
    let declared = cntfrq();
    if declared == 0 {
        fatal(uart, "CNTFRQ_EL0 is zero", 0);
    }
    let st_start = systimer();
    let ct_start = cntvct();
    while systimer().wrapping_sub(st_start) < CHECK_US {
        core::hint::spin_loop();
    }
    let st_elapsed = u64::from(systimer().wrapping_sub(st_start)); // ~CHECK_US µs
    let ct_elapsed = cntvct().wrapping_sub(ct_start);
    // implied Hz = ticks / seconds = ct_elapsed * 1_000_000 / st_elapsed_µs.
    let implied = ct_elapsed * 1_000_000 / st_elapsed;
    let diff = declared.abs_diff(implied);
    if diff * 100 > declared * CNTFRQ_TOLERANCE_PCT {
        fatal(uart, "CNTFRQ_EL0 disagrees with the system timer", implied);
    }

    // --- 2. Pin the ARM core clock to 1 GHz and report the rate actually set. ---
    // The request is clamped to the firmware's max, so this cannot overclock.
    // SAFETY: boot core, MMU off; best-effort — the read-back below is authoritative.
    let _ = unsafe { mailbox::set_clock_rate(mailbox::CLOCK_ID_ARM, TARGET_ARM_HZ) };
    // SAFETY: boot core, MMU off; synchronous mailbox exchange.
    let core_hz = match unsafe { mailbox::get_clock_rate(mailbox::CLOCK_ID_ARM) } {
        Some(hz) => hz,
        None => fatal(uart, "cannot read the ARM core clock", 0),
    };
    CORE_FREQ_HZ.store(core_hz, Ordering::Relaxed);

    let _ = writeln!(
        uart,
        "clocks: generic timer {declared} Hz (verified); ARM core {core_hz} Hz (pinned)."
    );
}
