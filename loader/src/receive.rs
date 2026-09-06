//! The loader's protocol state machine: receive an image, validate it, jump.
//!
//! Driven by [`Decoder`] over [`Uart::get_byte`], replying with
//! [`encode_frame`] over [`Uart::put_byte`]. The conversation and payload
//! layouts are defined in `../docs/PROTOCOL.md`; the state established before
//! the branch is defined in `../docs/ENTRY_CONTRACT.md`.
//!
//! Safety of the whole design rests on [`validate_header`]: every byte written
//! to RAM lands inside a window that has been checked to sit within physical
//! RAM and to not overlap the running loader (whose extent is read from the
//! linker symbols, not assumed), so the loader can never corrupt itself or the
//! payload's neighbours.

use core::arch::asm;
use core::ptr::write_volatile;

use chainloader_protocol::{
    Ack, Crc32, DataFrame, DecodeError, Decoded, Decoder, ErrorCode, ErrorMsg, FrameType,
    ImageHeader, MAX_FRAME, MAX_PAYLOAD, Ready, encode_frame,
};

use crate::uart::Uart;

/// Inclusive low bound of the writable window: 2 MiB. Below this sits the
/// loader (at 0x80000), the firmware's low-memory structures, and the exception
/// vector area, all of which must stay untouched.
const WINDOW_MIN: u64 = 0x0020_0000;
/// Exclusive high bound of the writable window: 448 MiB. This is the safe floor
/// across the supported boards — set by the Pi Zero 2 W's 512 MiB minus a
/// default 64 MiB `gpu_mem` split (leaving the ARM the bottom 448 MiB =
/// 0x1C00_0000), and still valid, just conservative, on the 1 GiB Pi 2/3. A
/// smaller `gpu_mem` raises the true ceiling; a `GET_ARM_MEMORY` mailbox query
/// would size it per board instead of assuming the smallest.
const WINDOW_MAX: u64 = 0x1C00_0000;
/// Required alignment of a load address (advertised in `READY`).
const REQUIRED_ALIGN: u64 = 0x800;
/// Largest image the loader will accept: the whole window.
const MAX_IMAGE_LEN: u32 = (WINDOW_MAX - WINDOW_MIN) as u32;
/// Largest `DATA` chunk: a full frame payload minus the `offset` field.
const MAX_CHUNK: u16 = (MAX_PAYLOAD - DataFrame::HEADER) as u16;
/// Loader build version, advertised in `READY`.
const LOADER_VERSION: u32 = 1;

unsafe extern "C" {
    static __loader_start: u8;
    static __loader_end: u8;
}

/// Physical extent `[start, end)` the running loader occupies, from the linker.
fn loader_bounds() -> (u64, u64) {
    let start = (&raw const __loader_start) as usize as u64;
    let end = (&raw const __loader_end) as usize as u64;
    (start, end)
}

/// State of an in-progress image transfer.
struct Transfer {
    header: ImageHeader,
    /// Bytes written so far; also the offset expected in the next `DATA`.
    received: u32,
    /// Running CRC over the bytes written so far.
    crc: Crc32,
    /// Whether the completed image's CRC matched the header.
    verified: bool,
}

/// Runs the loader forever: services the protocol until a valid `BOOT` branches
/// away to the loaded image (and never returns).
pub fn run(uart: Uart) -> ! {
    send_ready(&uart); // greet a host that is already listening
    let mut decoder = Decoder::new();
    let mut transfer: Option<Transfer> = None;

    loop {
        match decoder.push(uart.get_byte()) {
            Decoded::None => {}
            Decoded::Error(e) => send_error(&uart, decode_error_code(e), 0),
            Decoded::Frame(ty) => handle_frame(&uart, &mut transfer, ty, decoder.payload()),
        }
    }
}

/// Dispatches one validated frame. May branch to the image and never return.
fn handle_frame(uart: &Uart, transfer: &mut Option<Transfer>, ty: FrameType, payload: &[u8]) {
    match ty {
        FrameType::Hello => {
            *transfer = None; // a new session abandons any partial transfer
            send_ready(uart);
        }
        FrameType::Header => match ImageHeader::from_bytes(payload) {
            Ok(header) => match validate_header(&header) {
                Ok(()) => {
                    *transfer = Some(Transfer {
                        header,
                        received: 0,
                        crc: Crc32::new(),
                        verified: false,
                    });
                    send_ack(uart, 0);
                }
                Err(code) => {
                    *transfer = None;
                    send_error(uart, code, 0);
                }
            },
            Err(_) => send_error(uart, ErrorCode::BadLength, 0),
        },
        FrameType::Data => handle_data(uart, transfer, payload),
        FrameType::Boot => match transfer.as_ref() {
            Some(t) if t.verified => {
                send_ack(uart, t.received);
                let entry = t.header.load_addr + u64::from(t.header.entry_off);
                // SAFETY: the image was validated (in-window, non-overlapping,
                // aligned) and its CRC verified; `jump` establishes the
                // documented entry contract before branching.
                unsafe { jump(entry, t.header.load_addr, t.header.image_len) }
            }
            _ => send_error(uart, ErrorCode::NoImage, 0),
        },
        // These only ever travel Pi→host; receiving one means a confused peer.
        FrameType::Ready | FrameType::Ack | FrameType::Error => {
            send_error(uart, ErrorCode::Unexpected, 0);
        }
    }
}

/// Handles a `DATA` frame: bounds-check, write to RAM, fold into the CRC, ack.
fn handle_data(uart: &Uart, transfer: &mut Option<Transfer>, payload: &[u8]) {
    let Some(t) = transfer.as_mut() else {
        send_error(uart, ErrorCode::Unexpected, 0);
        return;
    };
    let Ok(data) = DataFrame::decode(payload) else {
        send_error(uart, ErrorCode::BadLength, 0);
        return;
    };
    if data.offset != t.received {
        send_error(uart, ErrorCode::OffsetMismatch, t.received);
        return;
    }
    let end = u64::from(data.offset) + data.chunk.len() as u64;
    if end > u64::from(t.header.image_len) {
        send_error(uart, ErrorCode::BadLength, data.offset);
        return;
    }

    // SAFETY: `validate_header` proved [load_addr, load_addr+image_len) is
    // in-window and non-overlapping; `end <= image_len` keeps this write inside it.
    unsafe { write_image(t.header.load_addr, data.offset, data.chunk) }
    t.crc.update(data.chunk);
    t.received += data.chunk.len() as u32;

    if t.received != t.header.image_len {
        send_ack(uart, t.received);
        return;
    }

    // Image complete: verify the end-to-end CRC before allowing a boot.
    let got = t.crc.clone().finalize();
    if got == t.header.image_crc32 {
        t.verified = true;
        send_ack(uart, t.received);
    } else {
        send_error(uart, ErrorCode::ImageCrc, got);
        *transfer = None; // force the host to restart from HEADER
    }
}

/// Validates an image header against the writable window, alignment, and the
/// loader's own footprint. Errors map directly to the wire [`ErrorCode`].
fn validate_header(h: &ImageHeader) -> Result<(), ErrorCode> {
    if h.image_len == 0 {
        return Err(ErrorCode::BadLength);
    }
    if h.image_len > MAX_IMAGE_LEN {
        return Err(ErrorCode::ImageTooLarge);
    }
    if h.load_addr % REQUIRED_ALIGN != 0 {
        return Err(ErrorCode::AddrMisaligned);
    }
    let start = h.load_addr;
    let end = start
        .checked_add(u64::from(h.image_len))
        .ok_or(ErrorCode::AddrOutOfRange)?;
    if start < WINDOW_MIN || end > WINDOW_MAX {
        return Err(ErrorCode::AddrOutOfRange);
    }
    let (loader_start, loader_end) = loader_bounds();
    // Half-open ranges [start,end) and [loader_start,loader_end) overlap iff:
    if start < loader_end && loader_start < end {
        return Err(ErrorCode::AddrOverlap);
    }
    if h.entry_off >= h.image_len {
        return Err(ErrorCode::AddrOutOfRange);
    }
    Ok(())
}

/// Writes `chunk` to `load_addr + offset` with volatile stores.
///
/// # Safety
///
/// The destination range must lie within a validated, writable window (see
/// [`validate_header`]) and must not overlap the loader.
unsafe fn write_image(load_addr: u64, offset: u32, chunk: &[u8]) {
    let base = (load_addr as usize + offset as usize) as *mut u8;
    for (i, &byte) in chunk.iter().enumerate() {
        // SAFETY: caller guarantees the whole range is valid, writable RAM.
        unsafe { write_volatile(base.add(i), byte) }
    }
}

/// Establishes the entry contract and branches to the loaded image. Never returns.
///
/// # Safety
///
/// `entry` must point at a validated, CRC-verified image loaded at `load_addr`.
/// Masks interrupts, makes the written image coherent with instruction fetch,
/// and hands over `x0=load_addr`, `x1=image_len`, `x2=x3=0` per
/// `../docs/ENTRY_CONTRACT.md`.
unsafe fn jump(entry: u64, load_addr: u64, image_len: u32) -> ! {
    unsafe {
        asm!("msr daifset, #0xf"); // mask D, A, I, F
        clean_dcache(load_addr, load_addr + u64::from(image_len));
        asm!(
            "ic iallu",  // invalidate all I-cache to PoU
            "dsb sy",
            "isb",
            "br {entry}",
            entry = in(reg) entry,
            in("x0") load_addr,
            in("x1") u64::from(image_len),
            in("x2") 0u64,
            in("x3") 0u64,
            options(noreturn, nostack),
        )
    }
}

/// Cleans the data cache to the point of coherency over `[start, end)`, so the
/// bytes just written are visible to instruction fetch. Uses the cache line
/// size reported by `CTR_EL0`.
///
/// # Safety
///
/// Executes cache-maintenance instructions; the range should cover the image.
unsafe fn clean_dcache(start: u64, end: u64) {
    unsafe {
        let ctr: u64;
        asm!("mrs {}, ctr_el0", out(reg) ctr);
        // CTR_EL0.DminLine (bits [19:16]) is log2 of the line size in words.
        let line = 4u64 << ((ctr >> 16) & 0xf);
        let mut addr = start & !(line - 1);
        while addr < end {
            asm!("dc cvac, {}", in(reg) addr);
            addr += line;
        }
        asm!("dsb sy");
    }
}

/// Maps a frame-decode failure to the wire error code the host expects.
fn decode_error_code(e: DecodeError) -> ErrorCode {
    match e {
        DecodeError::BadVersion(_) => ErrorCode::BadVersion,
        DecodeError::UnknownType(_) => ErrorCode::UnknownType,
        DecodeError::LengthTooLarge(_) => ErrorCode::BadLength,
        DecodeError::BadCrc { .. } => ErrorCode::BadCrc,
    }
}

/// Encodes and transmits one frame. A frame that will not encode is dropped;
/// on a wired link the host retries, which is preferable to blocking here.
fn send_frame(uart: &Uart, ty: FrameType, payload: &[u8]) {
    let mut buf = [0u8; MAX_FRAME];
    if let Ok(frame) = encode_frame(ty, payload, &mut buf) {
        for &b in frame {
            uart.put_byte(b);
        }
    }
}

fn send_ready(uart: &Uart) {
    let ready = Ready {
        loader_version: LOADER_VERSION,
        max_image_len: MAX_IMAGE_LEN,
        load_addr_min: WINDOW_MIN,
        load_addr_max: WINDOW_MAX,
        alignment: REQUIRED_ALIGN as u32,
        max_chunk: MAX_CHUNK,
    };
    send_frame(uart, FrameType::Ready, &ready.to_bytes());
}

fn send_ack(uart: &Uart, next_offset: u32) {
    send_frame(uart, FrameType::Ack, &Ack { next_offset }.to_bytes());
}

fn send_error(uart: &Uart, code: ErrorCode, detail: u32) {
    send_frame(
        uart,
        FrameType::Error,
        &ErrorMsg::new(code, detail).to_bytes(),
    );
}
