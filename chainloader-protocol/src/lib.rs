//! Wire protocol shared by the Raspberry Pi UART chainloader and the host
//! `cargo-pi` tool.
//!
//! The protocol is a small, versioned, endian-explicit framing over a raw UART
//! byte stream. It is deliberately allocation-free and `no_std` so the exact
//! same encode/decode code runs on the bare-metal loader and on the host, which
//! is the whole point of factoring it into its own crate: there is one wire
//! format, defined once, testable off-target.
//!
//! # Layout
//!
//! - [`crc`] — CRC-32/ISO-HDLC, used both per-frame and end-to-end over an image.
//! - [`frame`] — the on-wire frame: magic, version, type, length, payload, CRC,
//!   plus the one-shot [`encode_frame`].
//! - [`decoder`] — the streaming, resyncing [`Decoder`] that turns a raw byte
//!   stream back into validated frames.
//! - [`message`] — typed payload structs ([`Hello`], [`Ready`], [`ImageHeader`],
//!   [`DataFrame`], [`Ack`], [`ErrorMsg`]) with one definition of each layout.
//!
//! See `docs/PROTOCOL.md` for the full conversation (`HELLO`/`READY`/`HEADER`/
//! `DATA`/`ACK`/`ERROR`/`BOOT`) and `docs/ENTRY_CONTRACT.md` for the AArch64
//! register/cache state the loader establishes before jumping.
//!
//! # Status
//!
//! Framing ([`encode_frame`]), checksums ([`crc32`]), the streaming [`Decoder`],
//! and the typed payload structs are implemented. Wiring these into the loader
//! and host state machines is the next work in `PLANNED.md`.
#![cfg_attr(not(test), no_std)]

// The `std` feature only adds `std::error::Error` impls; link std for them
// while the crate stays `no_std` for the loader's default build.
#[cfg(all(feature = "std", not(test)))]
extern crate std;

pub mod crc;
pub mod decoder;
pub mod frame;
pub mod message;

pub use crc::{Crc32, crc32};
pub use decoder::{DecodeError, Decoded, Decoder};
pub use frame::{
    EncodeError, FrameType, HEADER_LEN, MAGIC, MAGIC_HI, MAGIC_LO, MAX_FRAME, MAX_PAYLOAD,
    PROTOCOL_VERSION, TRAILER_LEN, encode_frame, frame_len,
};
pub use message::{Ack, DataFrame, ErrorCode, ErrorMsg, Hello, ImageHeader, MsgError, Ready};

/// Returns the version of this crate, as recorded in `Cargo.toml`.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
