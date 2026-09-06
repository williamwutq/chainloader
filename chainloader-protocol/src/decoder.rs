//! Streaming, allocation-free frame decoder with resync.
//!
//! The loader reads the UART one byte at a time and cannot allocate. [`Decoder`]
//! turns that raw byte stream — which may start mid-frame or contain line
//! noise — into validated frames using a single fixed buffer. It is the receive
//! counterpart to [`encode_frame`](crate::encode_frame).
//!
//! ```
//! use chainloader_protocol::{Decoder, Decoded, FrameType, encode_frame};
//!
//! let mut buf = [0u8; chainloader_protocol::MAX_FRAME];
//! let frame = encode_frame(FrameType::Data, b"hi", &mut buf).unwrap();
//!
//! let mut dec = Decoder::new();
//! let mut got = None;
//! for &b in frame {
//!     if let Decoded::Frame(ty) = dec.push(b) {
//!         got = Some((ty, dec.payload().to_vec()));
//!     }
//! }
//! assert_eq!(got, Some((FrameType::Data, b"hi".to_vec())));
//! ```

use crate::crc::crc32;
use crate::frame::{
    FrameType, HEADER_LEN, MAGIC_HI, MAGIC_LO, MAX_PAYLOAD, PROTOCOL_VERSION, TRAILER_LEN,
};

/// Header bytes covered by the CRC and buffered by the decoder: everything in
/// the header except the two magic bytes (version, type, length_lo, length_hi).
const FIXED_LEN: usize = HEADER_LEN - 2;

/// Internal buffer size: the CRC-covered fixed bytes, the largest payload, and
/// the CRC trailer. The magic is never stored.
const DECODE_BUF: usize = FIXED_LEN + MAX_PAYLOAD + TRAILER_LEN;

/// Why a frame was rejected. On any of these the decoder discards the frame and
/// resumes scanning for the next magic (see [`Decoder`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The frame's `version` byte did not match [`PROTOCOL_VERSION`].
    BadVersion(u8),
    /// The `type` byte did not name a known [`FrameType`].
    UnknownType(u8),
    /// The `length` field exceeded [`MAX_PAYLOAD`].
    LengthTooLarge(u16),
    /// The trailer CRC did not match the CRC computed over the frame body.
    BadCrc {
        /// CRC read from the frame trailer.
        found: u32,
        /// CRC computed over the received `version..payload` bytes.
        expected: u32,
    },
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadVersion(v) => {
                write!(
                    f,
                    "unsupported protocol version {v} (expected {PROTOCOL_VERSION})"
                )
            }
            Self::UnknownType(t) => write!(f, "unknown frame type 0x{t:02x}"),
            Self::LengthTooLarge(n) => {
                write!(f, "frame length {n} exceeds MAX_PAYLOAD ({MAX_PAYLOAD})")
            }
            Self::BadCrc { found, expected } => {
                write!(
                    f,
                    "frame CRC 0x{found:08x} does not match computed 0x{expected:08x}"
                )
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecodeError {}

/// The result of feeding one byte to a [`Decoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// More bytes are needed before anything can be reported.
    None,
    /// A complete, CRC-validated frame is available. Read its payload with
    /// [`Decoder::payload`] before the next [`push`](Decoder::push).
    Frame(FrameType),
    /// A frame was rejected; the decoder has resynced and is scanning again.
    Error(DecodeError),
}

/// Phase of the byte-fed state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Scanning for the low magic byte.
    Sync0,
    /// Saw the low magic byte; expecting the high magic byte.
    Sync1,
    /// Collecting the four CRC-covered header bytes.
    Fixed,
    /// Collecting `length` payload bytes plus the four-byte CRC trailer.
    Body,
}

/// A resynchronizing frame decoder over a fixed internal buffer.
///
/// Feed bytes with [`push`](Self::push). It returns [`Decoded::None`] until a
/// frame completes ([`Decoded::Frame`]) or is rejected ([`Decoded::Error`]).
/// After [`Decoded::Frame`], the payload is available via [`payload`](Self::payload)
/// until the next `push`.
///
/// Resync: the two magic bytes are only a relock marker and are not covered by
/// the CRC, so a coincidental magic that begins a bad frame still fails
/// validation. On any rejection the decoder returns to scanning from the
/// following byte. On a wired link the host retransmits rejected frames, so a
/// genuine frame whose magic happened to fall inside a discarded region is
/// recovered on the resend.
pub struct Decoder {
    phase: Phase,
    buf: [u8; DECODE_BUF],
    /// Number of fixed header bytes collected so far (0..=FIXED_LEN).
    fixed_n: usize,
    /// Body bytes collected so far (payload + trailer).
    body_got: usize,
    /// Body bytes still expected: `length + TRAILER_LEN`.
    body_need: usize,
    /// Frame type of the frame currently in `buf` (meaningful after a `Frame`).
    ty: FrameType,
    /// Payload length of the frame currently in `buf`.
    len: usize,
}

impl Decoder {
    /// Creates a decoder ready to scan for the start of a frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: Phase::Sync0,
            buf: [0; DECODE_BUF],
            fixed_n: 0,
            body_got: 0,
            body_need: 0,
            ty: FrameType::Hello,
            len: 0,
        }
    }

    /// Returns the payload of the most recently completed frame.
    ///
    /// Only meaningful immediately after [`push`](Self::push) returned
    /// [`Decoded::Frame`], and only until the next `push`.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.buf[FIXED_LEN..FIXED_LEN + self.len]
    }

    /// Discards any in-progress frame and returns to scanning for a magic.
    pub fn reset(&mut self) {
        self.phase = Phase::Sync0;
        self.fixed_n = 0;
        self.body_got = 0;
    }

    /// Feeds one byte to the decoder.
    pub fn push(&mut self, b: u8) -> Decoded {
        match self.phase {
            Phase::Sync0 => {
                if b == MAGIC_LO {
                    self.phase = Phase::Sync1;
                }
                Decoded::None
            }
            Phase::Sync1 => {
                if b == MAGIC_HI {
                    self.phase = Phase::Fixed;
                    self.fixed_n = 0;
                } else if b != MAGIC_LO {
                    // Not a magic; fall back to scanning. A repeated low byte
                    // keeps us here in case it is the true start of the magic.
                    self.phase = Phase::Sync0;
                }
                Decoded::None
            }
            Phase::Fixed => self.push_fixed(b),
            Phase::Body => self.push_body(b),
        }
    }

    /// Collects a CRC-covered header byte; parses the header once all four are in.
    fn push_fixed(&mut self, b: u8) -> Decoded {
        self.buf[self.fixed_n] = b;
        self.fixed_n += 1;
        if self.fixed_n < FIXED_LEN {
            return Decoded::None;
        }

        let version = self.buf[0];
        if version != PROTOCOL_VERSION {
            self.reset();
            return Decoded::Error(DecodeError::BadVersion(version));
        }
        let Some(ty) = FrameType::from_u8(self.buf[1]) else {
            let raw = self.buf[1];
            self.reset();
            return Decoded::Error(DecodeError::UnknownType(raw));
        };
        let len = u16::from_le_bytes([self.buf[2], self.buf[3]]);
        if usize::from(len) > MAX_PAYLOAD {
            self.reset();
            return Decoded::Error(DecodeError::LengthTooLarge(len));
        }

        self.ty = ty;
        self.len = usize::from(len);
        self.body_need = self.len + TRAILER_LEN;
        self.body_got = 0;
        self.phase = Phase::Body;
        Decoded::None
    }

    /// Collects payload + trailer bytes; validates the CRC once the body is full.
    fn push_body(&mut self, b: u8) -> Decoded {
        self.buf[FIXED_LEN + self.body_got] = b;
        self.body_got += 1;
        if self.body_got < self.body_need {
            return Decoded::None;
        }

        let body_end = FIXED_LEN + self.len;
        let expected = crc32(&self.buf[..body_end]);
        let found = u32::from_le_bytes([
            self.buf[body_end],
            self.buf[body_end + 1],
            self.buf[body_end + 2],
            self.buf[body_end + 3],
        ]);

        // Return to scanning for the next frame either way; the payload stays in
        // `buf` so `payload()` is valid until the next push.
        self.phase = Phase::Sync0;
        self.fixed_n = 0;
        self.body_got = 0;

        if found == expected {
            Decoded::Frame(self.ty)
        } else {
            Decoded::Error(DecodeError::BadCrc { found, expected })
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{MAX_FRAME, encode_frame, frame_len};

    /// Feeds every byte and returns the (type, payload) of each completed frame
    /// and every error, in order.
    fn run(bytes: &[u8]) -> (Vec<(FrameType, Vec<u8>)>, Vec<DecodeError>) {
        let mut dec = Decoder::new();
        let mut frames = Vec::new();
        let mut errors = Vec::new();
        for &b in bytes {
            match dec.push(b) {
                Decoded::None => {}
                Decoded::Frame(ty) => frames.push((ty, dec.payload().to_vec())),
                Decoded::Error(e) => errors.push(e),
            }
        }
        (frames, errors)
    }

    fn encode(ty: FrameType, payload: &[u8]) -> Vec<u8> {
        let mut buf = [0u8; MAX_FRAME];
        encode_frame(ty, payload, &mut buf).unwrap().to_vec()
    }

    #[test]
    fn round_trip_every_frame_type() {
        for (ty, payload) in [
            (FrameType::Hello, &b"\x01\x00\x00\x00"[..]),
            (FrameType::Ready, &b""[..]),
            (FrameType::Data, &b"the quick brown fox"[..]),
            (FrameType::Boot, &b""[..]),
        ] {
            let (frames, errors) = run(&encode(ty, payload));
            assert!(errors.is_empty(), "{ty:?} produced errors: {errors:?}");
            assert_eq!(frames, vec![(ty, payload.to_vec())]);
        }
    }

    #[test]
    fn max_payload_round_trips() {
        let payload = [0xA5u8; MAX_PAYLOAD];
        let (frames, errors) = run(&encode(FrameType::Data, &payload));
        assert!(errors.is_empty());
        assert_eq!(frames, vec![(FrameType::Data, payload.to_vec())]);
    }

    #[test]
    fn two_frames_back_to_back() {
        let mut stream = encode(FrameType::Hello, b"\x01\x00\x00\x00");
        stream.extend_from_slice(&encode(FrameType::Data, b"payload"));
        let (frames, errors) = run(&stream);
        assert!(errors.is_empty());
        assert_eq!(
            frames,
            vec![
                (FrameType::Hello, b"\x01\x00\x00\x00".to_vec()),
                (FrameType::Data, b"payload".to_vec()),
            ]
        );
    }

    #[test]
    fn leading_garbage_is_skipped() {
        // Random bytes that are not a valid frame precede a good one.
        let mut stream = vec![0x00, 0xFF, 0xAE, 0x11, 0x50, 0x42, 0xAE, 0xAE];
        stream.extend_from_slice(&encode(FrameType::Data, b"after noise"));
        let (frames, _errors) = run(&stream);
        assert_eq!(frames, vec![(FrameType::Data, b"after noise".to_vec())]);
    }

    #[test]
    fn corrupted_payload_fails_crc_then_recovers() {
        let mut frame = encode(FrameType::Data, b"important");
        let payload_start = HEADER_LEN;
        frame[payload_start] ^= 0xFF; // flip a payload byte
        let mut stream = frame;
        stream.extend_from_slice(&encode(FrameType::Data, b"resent"));

        let (frames, errors) = run(&stream);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], DecodeError::BadCrc { .. }));
        // The decoder resynced and decoded the following good frame.
        assert_eq!(frames, vec![(FrameType::Data, b"resent".to_vec())]);
    }

    #[test]
    fn bad_version_is_rejected() {
        let mut frame = encode(FrameType::Boot, b"");
        frame[2] = PROTOCOL_VERSION.wrapping_add(1); // version byte
        let (frames, errors) = run(&frame);
        assert!(frames.is_empty());
        assert_eq!(errors, vec![DecodeError::BadVersion(PROTOCOL_VERSION + 1)]);
    }

    #[test]
    fn unknown_type_is_rejected() {
        let mut frame = encode(FrameType::Boot, b"");
        frame[3] = 0x7F; // type byte, not a known FrameType
        let (frames, errors) = run(&frame);
        assert!(frames.is_empty());
        assert_eq!(errors, vec![DecodeError::UnknownType(0x7F)]);
    }

    #[test]
    fn oversize_length_is_rejected() {
        // Hand-build a header claiming a payload larger than MAX_PAYLOAD.
        let bad_len = (MAX_PAYLOAD + 1) as u16;
        let header = [
            MAGIC_LO,
            MAGIC_HI,
            PROTOCOL_VERSION,
            FrameType::Data.as_u8(),
            (bad_len & 0xff) as u8,
            (bad_len >> 8) as u8,
        ];
        let (frames, errors) = run(&header);
        assert!(frames.is_empty());
        assert_eq!(errors, vec![DecodeError::LengthTooLarge(bad_len)]);
    }

    #[test]
    fn split_across_pushes_is_fine() {
        // Feeding one byte at a time is already what run() does; assert the
        // frame only appears on the final byte, never early.
        let frame = encode(FrameType::Data, b"x");
        let mut dec = Decoder::new();
        for &b in &frame[..frame.len() - 1] {
            assert_eq!(dec.push(b), Decoded::None);
        }
        assert_eq!(
            dec.push(*frame.last().unwrap()),
            Decoded::Frame(FrameType::Data)
        );
        assert_eq!(dec.payload(), b"x");
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        // A cheap stand-in for fuzzing: a pseudo-random stream must never panic
        // and must leave the decoder usable for a real frame afterwards.
        let mut state = 0x1234_5678u32;
        let mut noise = Vec::new();
        for _ in 0..10_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise.push((state >> 24) as u8);
        }
        let (_f, _e) = run(&noise); // must not panic

        let mut stream = noise;
        stream.extend_from_slice(&encode(FrameType::Data, b"still works"));
        let (frames, _errors) = run(&stream);
        assert!(frames.contains(&(FrameType::Data, b"still works".to_vec())));
    }

    #[test]
    fn frame_len_matches_encoded() {
        assert_eq!(encode(FrameType::Data, b"abcd").len(), frame_len(4));
    }
}
