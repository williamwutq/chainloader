//! Frame layout and the one-shot encoder.
//!
//! Every message on the wire is a single frame:
//!
//! ```text
//! offset  size  field
//!   0      2    magic    = 0x50AE, little-endian (bytes 0xAE 0x50)
//!   2      1    version  = PROTOCOL_VERSION
//!   3      1    type     = FrameType
//!   4      2    length   = payload byte count, little-endian, 0..=MAX_PAYLOAD
//!   6      N    payload  = `length` bytes
//!  6+N     4    crc32    = CRC-32 over bytes [2 .. 6+N] (version..payload), little-endian
//! ```
//!
//! The magic is only a resync marker; the CRC covers everything after it, so a
//! receiver that locks onto a spurious magic still rejects the frame. All
//! multi-byte fields are little-endian, matching the AArch64 default so the
//! loader never byte-swaps.

use crate::crc::crc32;

/// Wire magic, little-endian. Byte 0 is `0xAE`, byte 1 is `0x50`.
pub const MAGIC: u16 = 0x50AE;
/// Low byte of [`MAGIC`] — the first byte of every frame.
pub const MAGIC_LO: u8 = (MAGIC & 0xff) as u8;
/// High byte of [`MAGIC`] — the second byte of every frame.
pub const MAGIC_HI: u8 = (MAGIC >> 8) as u8;

/// Protocol version this crate speaks. Bump on any incompatible wire change.
pub const PROTOCOL_VERSION: u8 = 1;

/// Largest payload a single frame may carry, in bytes.
///
/// Sized so the loader's receive buffer is a little over 1 KiB — small enough
/// to sit comfortably in bare-metal RAM, large enough that per-frame overhead
/// is negligible against the payload.
pub const MAX_PAYLOAD: usize = 1024;

/// Bytes preceding the payload: magic(2) + version(1) + type(1) + length(2).
pub const HEADER_LEN: usize = 6;
/// Bytes following the payload: the CRC-32 trailer.
pub const TRAILER_LEN: usize = 4;
/// Largest possible complete frame, in bytes.
pub const MAX_FRAME: usize = HEADER_LEN + MAX_PAYLOAD + TRAILER_LEN;

/// Total on-wire length of a frame carrying `payload_len` payload bytes.
#[must_use]
pub const fn frame_len(payload_len: usize) -> usize {
    HEADER_LEN + payload_len + TRAILER_LEN
}

/// The kind of a frame, occupying the single `type` byte.
///
/// Direction is a convention, not enforced by the type: `Hello`, `Header`,
/// `Data`, and `Boot` travel host→Pi; `Ready`, `Ack`, and `Error` travel
/// Pi→host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameType {
    /// host→Pi: connection knock. Payload: `host_version: u32`.
    Hello = 0x01,
    /// Pi→host: loader capabilities and writable window (version, max image
    /// length, load-address bounds, alignment, max chunk size).
    Ready = 0x02,
    /// host→Pi: describes the image about to be sent (load address, length,
    /// image CRC-32, entry offset, flags).
    Header = 0x03,
    /// host→Pi: one chunk of image bytes. Payload: `offset: u32` then chunk bytes.
    Data = 0x04,
    /// Pi→host: positive acknowledgement. Payload: `next_offset: u32`.
    Ack = 0x05,
    /// Pi→host: negative acknowledgement. Payload: `code: u16` then `detail: u32`.
    Error = 0x06,
    /// host→Pi: hand control to the received image. No payload.
    Boot = 0x07,
}

impl FrameType {
    /// Returns the [`FrameType`] for a raw type byte, or `None` if unrecognized.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Hello),
            0x02 => Some(Self::Ready),
            0x03 => Some(Self::Header),
            0x04 => Some(Self::Data),
            0x05 => Some(Self::Ack),
            0x06 => Some(Self::Error),
            0x07 => Some(Self::Boot),
            _ => None,
        }
    }

    /// The raw type byte for this frame type.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Why [`encode_frame`] could not build a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The payload exceeded [`MAX_PAYLOAD`].
    PayloadTooLarge,
    /// The output buffer was smaller than [`frame_len`] of the payload.
    BufferTooSmall,
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PayloadTooLarge => write!(f, "payload exceeds MAX_PAYLOAD ({MAX_PAYLOAD} bytes)"),
            Self::BufferTooSmall => write!(f, "output buffer too small for frame"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for EncodeError {}

/// Encodes one frame into `out`, returning the exact bytes written.
///
/// `out` must be at least [`frame_len(payload.len())`](frame_len) bytes;
/// [`MAX_FRAME`] always suffices. No allocation occurs.
///
/// # Errors
///
/// Returns [`EncodeError::PayloadTooLarge`] if `payload` exceeds
/// [`MAX_PAYLOAD`], or [`EncodeError::BufferTooSmall`] if `out` cannot hold the
/// frame.
pub fn encode_frame<'a>(
    ty: FrameType,
    payload: &[u8],
    out: &'a mut [u8],
) -> Result<&'a [u8], EncodeError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(EncodeError::PayloadTooLarge);
    }
    let total = frame_len(payload.len());
    if out.len() < total {
        return Err(EncodeError::BufferTooSmall);
    }
    out[0] = MAGIC_LO;
    out[1] = MAGIC_HI;
    out[2] = PROTOCOL_VERSION;
    out[3] = ty.as_u8();
    let len = payload.len() as u16;
    out[4] = (len & 0xff) as u8;
    out[5] = (len >> 8) as u8;
    let body_end = HEADER_LEN + payload.len();
    out[HEADER_LEN..body_end].copy_from_slice(payload);
    // CRC covers version..payload, i.e. everything after the magic.
    let crc = crc32(&out[2..body_end]);
    out[body_end..body_end + TRAILER_LEN].copy_from_slice(&crc.to_le_bytes());
    Ok(&out[..total])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_shape() {
        let mut buf = [0u8; MAX_FRAME];
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let frame = encode_frame(FrameType::Data, &payload, &mut buf).unwrap();
        assert_eq!(frame.len(), frame_len(payload.len()));
        assert_eq!(frame[0], MAGIC_LO);
        assert_eq!(frame[1], MAGIC_HI);
        assert_eq!(frame[2], PROTOCOL_VERSION);
        assert_eq!(frame[3], FrameType::Data.as_u8());
        assert_eq!(
            u16::from_le_bytes([frame[4], frame[5]]),
            payload.len() as u16
        );
        assert_eq!(&frame[HEADER_LEN..HEADER_LEN + 4], &payload);
    }

    #[test]
    fn buffer_too_small() {
        let mut buf = [0u8; 4];
        assert_eq!(
            encode_frame(FrameType::Boot, &[], &mut buf),
            Err(EncodeError::BufferTooSmall)
        );
    }

    #[test]
    fn payload_too_large() {
        let mut buf = [0u8; MAX_FRAME + 8];
        let payload = [0u8; MAX_PAYLOAD + 1];
        assert_eq!(
            encode_frame(FrameType::Data, &payload, &mut buf),
            Err(EncodeError::PayloadTooLarge)
        );
    }

    #[test]
    fn frame_type_byte_round_trip() {
        for ty in [
            FrameType::Hello,
            FrameType::Ready,
            FrameType::Header,
            FrameType::Data,
            FrameType::Ack,
            FrameType::Error,
            FrameType::Boot,
        ] {
            assert_eq!(FrameType::from_u8(ty.as_u8()), Some(ty));
        }
        assert_eq!(FrameType::from_u8(0x00), None);
        assert_eq!(FrameType::from_u8(0xFF), None);
    }
}
