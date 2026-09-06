//! Typed payload structs for each frame type.
//!
//! [`frame`](crate::frame) moves opaque payload bytes; this module gives those
//! bytes names and a single definition of their little-endian layout, so the
//! loader and host cannot drift apart. Every layout matches `docs/PROTOCOL.md`
//! exactly.
//!
//! Fixed-size payloads expose a `LEN`, a `to_bytes(&self) -> [u8; LEN]`, and a
//! `from_bytes(&[u8]) -> Result<Self, MsgError>`. The variable-size `DATA`
//! payload is the borrowing [`DataFrame`] view instead. Nothing here allocates.
//!
//! ```
//! use chainloader_protocol::{Ack, FrameType, Decoder, Decoded, encode_frame};
//!
//! // Encode a typed payload into a frame...
//! let mut buf = [0u8; chainloader_protocol::MAX_FRAME];
//! let frame = encode_frame(FrameType::Ack, &Ack { next_offset: 42 }.to_bytes(), &mut buf).unwrap();
//!
//! // ...decode the frame and read the payload back as the same struct.
//! let mut dec = Decoder::new();
//! let mut ack = None;
//! for &b in frame {
//!     if let Decoded::Frame(FrameType::Ack) = dec.push(b) {
//!         ack = Some(Ack::from_bytes(dec.payload()).unwrap());
//!     }
//! }
//! assert_eq!(ack, Some(Ack { next_offset: 42 }));
//! ```

/// Why a payload could not be decoded (or encoded into too small a buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgError {
    /// Fewer bytes than the payload's fixed layout requires.
    Truncated {
        /// Minimum bytes the layout needs.
        expected: usize,
        /// Bytes actually provided.
        got: usize,
    },
}

impl core::fmt::Display for MsgError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated { expected, got } => {
                write!(f, "payload truncated: need {expected} bytes, got {got}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for MsgError {}

/// Checks that `bytes` holds at least `need` bytes.
#[inline(always)]
const fn require(len: usize, need: usize) -> Result<(), MsgError> {
    if len < need {
        Err(MsgError::Truncated {
            expected: need,
            got: len,
        })
    } else {
        Ok(())
    }
}

// Small little-endian readers. Callers guarantee the slice is long enough.
#[inline(always)]
fn rd_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
#[inline(always)]
fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
#[inline(always)]
fn rd_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

/// `HELLO` payload (host→Pi): the host's protocol knock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    /// Protocol version the host speaks.
    pub host_version: u32,
}

impl Hello {
    /// Encoded length in bytes.
    pub const LEN: usize = 4;

    /// Serializes to its fixed little-endian byte layout.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        self.host_version.to_le_bytes()
    }

    /// Parses from at least [`LEN`](Self::LEN) bytes; extra trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if fewer than [`LEN`](Self::LEN) bytes are given.
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MsgError> {
        require(bytes.len(), Self::LEN)?;
        Ok(Self {
            host_version: rd_u32(bytes, 0),
        })
    }
}

/// `READY` payload (Pi→host): the loader's capabilities and writable window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    /// Loader build version.
    pub loader_version: u32,
    /// Largest image the loader will accept, in bytes.
    pub max_image_len: u32,
    /// Inclusive low bound of the writable window.
    pub load_addr_min: u64,
    /// Exclusive high bound of the writable window.
    pub load_addr_max: u64,
    /// Required `load_addr` alignment (a power of two).
    pub alignment: u32,
    /// Largest `DATA` chunk the loader accepts, in bytes.
    pub max_chunk: u16,
}

impl Ready {
    /// Encoded length in bytes (includes a 2-byte reserved field, sent as zero).
    pub const LEN: usize = 32;

    /// Serializes to its fixed little-endian byte layout.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&self.loader_version.to_le_bytes());
        out[4..8].copy_from_slice(&self.max_image_len.to_le_bytes());
        out[8..16].copy_from_slice(&self.load_addr_min.to_le_bytes());
        out[16..24].copy_from_slice(&self.load_addr_max.to_le_bytes());
        out[24..28].copy_from_slice(&self.alignment.to_le_bytes());
        out[28..30].copy_from_slice(&self.max_chunk.to_le_bytes());
        // out[30..32] reserved, left zero.
        out
    }

    /// Parses from at least [`LEN`](Self::LEN) bytes; the reserved field and any
    /// trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if fewer than [`LEN`](Self::LEN) bytes are given.
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MsgError> {
        require(bytes.len(), Self::LEN)?;
        Ok(Self {
            loader_version: rd_u32(bytes, 0),
            max_image_len: rd_u32(bytes, 4),
            load_addr_min: rd_u64(bytes, 8),
            load_addr_max: rd_u64(bytes, 16),
            alignment: rd_u32(bytes, 24),
            max_chunk: rd_u16(bytes, 28),
        })
    }
}

/// `HEADER` payload (host→Pi): describes the image about to be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHeader {
    /// Physical address to load the image at.
    pub load_addr: u64,
    /// Total image length in bytes.
    pub image_len: u32,
    /// CRC-32 of the whole image, for end-to-end integrity.
    pub image_crc32: u32,
    /// Byte offset added to `load_addr` for the entry PC.
    pub entry_off: u32,
    /// Reserved flags; send zero.
    pub flags: u32,
}

impl ImageHeader {
    /// Encoded length in bytes.
    pub const LEN: usize = 24;

    /// Serializes to its fixed little-endian byte layout.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..8].copy_from_slice(&self.load_addr.to_le_bytes());
        out[8..12].copy_from_slice(&self.image_len.to_le_bytes());
        out[12..16].copy_from_slice(&self.image_crc32.to_le_bytes());
        out[16..20].copy_from_slice(&self.entry_off.to_le_bytes());
        out[20..24].copy_from_slice(&self.flags.to_le_bytes());
        out
    }

    /// Parses from at least [`LEN`](Self::LEN) bytes; extra trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if fewer than [`LEN`](Self::LEN) bytes are given.
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MsgError> {
        require(bytes.len(), Self::LEN)?;
        Ok(Self {
            load_addr: rd_u64(bytes, 0),
            image_len: rd_u32(bytes, 8),
            image_crc32: rd_u32(bytes, 12),
            entry_off: rd_u32(bytes, 16),
            flags: rd_u32(bytes, 20),
        })
    }
}

/// `ACK` payload (Pi→host): positive acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    /// Total bytes accepted so far, i.e. the offset expected in the next `DATA`.
    pub next_offset: u32,
}

impl Ack {
    /// Encoded length in bytes.
    pub const LEN: usize = 4;

    /// Serializes to its fixed little-endian byte layout.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        self.next_offset.to_le_bytes()
    }

    /// Parses from at least [`LEN`](Self::LEN) bytes; extra trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if fewer than [`LEN`](Self::LEN) bytes are given.
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MsgError> {
        require(bytes.len(), Self::LEN)?;
        Ok(Self {
            next_offset: rd_u32(bytes, 0),
        })
    }
}

/// A protocol-level error condition, as carried in an `ERROR` frame.
///
/// The wire value is a `u16`. [`ErrorMsg`] stores the raw code so an unknown
/// value from a newer loader still round-trips and can be reported; use
/// [`ErrorMsg::known_code`] to recover this enum when the code is recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// Frame `version` not supported.
    BadVersion = 1,
    /// Frame CRC mismatch.
    BadCrc = 2,
    /// `length` out of range for the frame type.
    BadLength = 3,
    /// Unrecognized `type` byte.
    UnknownType = 4,
    /// Frame not valid in the current state.
    Unexpected = 5,
    /// Image window outside `[load_addr_min, load_addr_max)`.
    AddrOutOfRange = 6,
    /// `load_addr` violates the required alignment.
    AddrMisaligned = 7,
    /// Image window overlaps the running loader.
    AddrOverlap = 8,
    /// `image_len` exceeds `max_image_len`.
    ImageTooLarge = 9,
    /// `DATA` `offset` did not match the expected next offset.
    OffsetMismatch = 10,
    /// Assembled image CRC did not match `image_crc32`.
    ImageCrc = 11,
    /// `BOOT` arrived before a complete, verified image.
    NoImage = 12,
}

impl ErrorCode {
    /// The wire value for this code.
    #[inline(always)]
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    /// The [`ErrorCode`] for a raw wire value, or `None` if unrecognized.
    #[inline]
    #[must_use]
    pub const fn from_u16(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::BadVersion),
            2 => Some(Self::BadCrc),
            3 => Some(Self::BadLength),
            4 => Some(Self::UnknownType),
            5 => Some(Self::Unexpected),
            6 => Some(Self::AddrOutOfRange),
            7 => Some(Self::AddrMisaligned),
            8 => Some(Self::AddrOverlap),
            9 => Some(Self::ImageTooLarge),
            10 => Some(Self::OffsetMismatch),
            11 => Some(Self::ImageCrc),
            12 => Some(Self::NoImage),
            _ => None,
        }
    }
}

impl core::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::BadVersion => "unsupported protocol version",
            Self::BadCrc => "frame CRC mismatch",
            Self::BadLength => "frame length out of range",
            Self::UnknownType => "unknown frame type",
            Self::Unexpected => "unexpected frame for current state",
            Self::AddrOutOfRange => "load address outside writable window",
            Self::AddrMisaligned => "load address misaligned",
            Self::AddrOverlap => "image overlaps the loader",
            Self::ImageTooLarge => "image exceeds maximum size",
            Self::OffsetMismatch => "data offset mismatch",
            Self::ImageCrc => "image CRC mismatch",
            Self::NoImage => "boot with no verified image",
        };
        f.write_str(s)
    }
}

/// `ERROR` payload (Pi→host): negative acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorMsg {
    /// Raw error code. Recognized values map to [`ErrorCode`] via [`known_code`](Self::known_code).
    pub code: u16,
    /// Context for the error, e.g. the offset at which it occurred.
    pub detail: u32,
}

impl ErrorMsg {
    /// Encoded length in bytes.
    pub const LEN: usize = 6;

    /// Builds an error message from a known [`ErrorCode`].
    #[inline]
    #[must_use]
    pub fn new(code: ErrorCode, detail: u32) -> Self {
        Self {
            code: code.as_u16(),
            detail,
        }
    }

    /// Returns the typed [`ErrorCode`] if `code` is recognized.
    #[inline]
    #[must_use]
    pub fn known_code(&self) -> Option<ErrorCode> {
        ErrorCode::from_u16(self.code)
    }

    /// Serializes to its fixed little-endian byte layout.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..2].copy_from_slice(&self.code.to_le_bytes());
        out[2..6].copy_from_slice(&self.detail.to_le_bytes());
        out
    }

    /// Parses from at least [`LEN`](Self::LEN) bytes; extra trailing bytes are ignored.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if fewer than [`LEN`](Self::LEN) bytes are given.
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MsgError> {
        require(bytes.len(), Self::LEN)?;
        Ok(Self {
            code: rd_u16(bytes, 0),
            detail: rd_u32(bytes, 2),
        })
    }
}

/// `DATA` payload (host→Pi): a chunk of image bytes at a byte offset.
///
/// Unlike the fixed payloads, `DATA` is variable length and borrows its chunk
/// rather than copying it (the loader cannot afford a copy). The wire layout is
/// `offset: u32` followed by the chunk bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataFrame<'a> {
    /// Byte offset of this chunk within the image.
    pub offset: u32,
    /// The chunk bytes.
    pub chunk: &'a [u8],
}

impl<'a> DataFrame<'a> {
    /// Bytes preceding the chunk: the `offset` field.
    pub const HEADER: usize = 4;

    /// On-wire payload length: [`HEADER`](Self::HEADER) plus the chunk.
    #[inline(always)]
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        Self::HEADER + self.chunk.len()
    }

    /// Writes the payload (offset then chunk) into `out`, returning the byte count.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if `out` is smaller than [`encoded_len`](Self::encoded_len).
    #[inline]
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, MsgError> {
        let need = self.encoded_len();
        require(out.len(), need)?;
        out[0..4].copy_from_slice(&self.offset.to_le_bytes());
        out[4..need].copy_from_slice(self.chunk);
        Ok(need)
    }

    /// Parses a `DATA` payload, borrowing the chunk from `bytes`.
    ///
    /// # Errors
    ///
    /// [`MsgError::Truncated`] if `bytes` is shorter than [`HEADER`](Self::HEADER).
    #[inline]
    pub fn decode(bytes: &'a [u8]) -> Result<Self, MsgError> {
        require(bytes.len(), Self::HEADER)?;
        Ok(Self {
            offset: rd_u32(bytes, 0),
            chunk: &bytes[Self::HEADER..],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FrameType, MAX_FRAME, encode_frame};

    #[test]
    fn documented_lengths() {
        // Must match docs/PROTOCOL.md.
        assert_eq!(Hello::LEN, 4);
        assert_eq!(Ready::LEN, 32);
        assert_eq!(ImageHeader::LEN, 24);
        assert_eq!(Ack::LEN, 4);
        assert_eq!(ErrorMsg::LEN, 6);
        assert_eq!(DataFrame::HEADER, 4);
    }

    #[test]
    fn hello_round_trip() {
        let m = Hello {
            host_version: 0x0102_0304,
        };
        assert_eq!(Hello::from_bytes(&m.to_bytes()), Ok(m));
        // Explicit little-endian check.
        assert_eq!(m.to_bytes(), [0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn ready_round_trip_and_reserved_is_zero() {
        let m = Ready {
            loader_version: 1,
            max_image_len: 0x0010_0000,
            load_addr_min: 0x0020_0000,
            load_addr_max: 0x3000_0000,
            alignment: 0x800,
            max_chunk: 1024,
        };
        let bytes = m.to_bytes();
        assert_eq!(&bytes[30..32], &[0, 0], "reserved field must be zero");
        assert_eq!(Ready::from_bytes(&bytes), Ok(m));
    }

    #[test]
    fn image_header_round_trip() {
        let m = ImageHeader {
            load_addr: 0x0020_0000,
            image_len: 4096,
            image_crc32: 0xDEAD_BEEF,
            entry_off: 0,
            flags: 0,
        };
        assert_eq!(ImageHeader::from_bytes(&m.to_bytes()), Ok(m));
    }

    #[test]
    fn ack_round_trip() {
        let m = Ack {
            next_offset: 0x1234,
        };
        assert_eq!(Ack::from_bytes(&m.to_bytes()), Ok(m));
    }

    #[test]
    fn error_round_trip_and_known_code() {
        let m = ErrorMsg::new(ErrorCode::ImageCrc, 0xAABB_CCDD);
        assert_eq!(m.code, 11);
        assert_eq!(m.known_code(), Some(ErrorCode::ImageCrc));
        assert_eq!(ErrorMsg::from_bytes(&m.to_bytes()), Ok(m));
    }

    #[test]
    fn unknown_error_code_still_round_trips() {
        let m = ErrorMsg {
            code: 999,
            detail: 7,
        };
        assert_eq!(m.known_code(), None);
        assert_eq!(ErrorMsg::from_bytes(&m.to_bytes()), Ok(m));
    }

    #[test]
    fn error_code_enum_round_trips() {
        for code in [
            ErrorCode::BadVersion,
            ErrorCode::BadCrc,
            ErrorCode::BadLength,
            ErrorCode::UnknownType,
            ErrorCode::Unexpected,
            ErrorCode::AddrOutOfRange,
            ErrorCode::AddrMisaligned,
            ErrorCode::AddrOverlap,
            ErrorCode::ImageTooLarge,
            ErrorCode::OffsetMismatch,
            ErrorCode::ImageCrc,
            ErrorCode::NoImage,
        ] {
            assert_eq!(ErrorCode::from_u16(code.as_u16()), Some(code));
        }
        assert_eq!(ErrorCode::from_u16(0), None);
        assert_eq!(ErrorCode::from_u16(13), None);
    }

    #[test]
    fn truncated_is_reported() {
        assert_eq!(
            Ready::from_bytes(&[0u8; 31]),
            Err(MsgError::Truncated {
                expected: 32,
                got: 31
            })
        );
        assert_eq!(
            DataFrame::decode(&[0u8; 3]),
            Err(MsgError::Truncated {
                expected: 4,
                got: 3
            })
        );
    }

    #[test]
    fn data_frame_round_trip() {
        let chunk = b"a slice of the image";
        let df = DataFrame {
            offset: 0x40,
            chunk,
        };
        let mut buf = [0u8; 64];
        let n = df.encode(&mut buf).unwrap();
        assert_eq!(n, df.encoded_len());
        assert_eq!(n, 4 + chunk.len());

        let back = DataFrame::decode(&buf[..n]).unwrap();
        assert_eq!(back, df);
        assert_eq!(back.chunk, chunk);
    }

    #[test]
    fn data_frame_encode_buffer_too_small() {
        let df = DataFrame {
            offset: 0,
            chunk: &[1, 2, 3, 4],
        };
        let mut small = [0u8; 4];
        assert_eq!(
            df.encode(&mut small),
            Err(MsgError::Truncated {
                expected: 8,
                got: 4
            })
        );
    }

    #[test]
    fn full_stack_ready_through_frame() {
        // Encode a Ready payload into a frame, then decode it back to a struct.
        let ready = Ready {
            loader_version: 7,
            max_image_len: 0x0080_0000,
            load_addr_min: 0x0020_0000,
            load_addr_max: 0x3000_0000,
            alignment: 0x1000,
            max_chunk: 512,
        };
        let mut fbuf = [0u8; MAX_FRAME];
        let frame = encode_frame(FrameType::Ready, &ready.to_bytes(), &mut fbuf).unwrap();

        let mut dec = crate::Decoder::new();
        let mut decoded = None;
        for &b in frame {
            if let crate::Decoded::Frame(FrameType::Ready) = dec.push(b) {
                decoded = Some(Ready::from_bytes(dec.payload()).unwrap());
            }
        }
        assert_eq!(decoded, Some(ready));
    }
}
