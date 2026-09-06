//! CRC-32/ISO-HDLC (the "zlib"/PKZIP CRC): reflected polynomial `0xEDB88320`,
//! init `0xFFFF_FFFF`, final XOR `0xFFFF_FFFF`.
//!
//! Chosen because it is the CRC everyone already has a reference for — the host
//! can cross-check against `crc32fast`, `zlib`, or `python -c "import
//! binascii"` — which matters for an interoperable wire format. The
//! implementation is a plain 256-entry table generated at compile time, so it
//! is `const`-clean and needs no build step.

/// Precomputed lookup table, one `u32` per possible input byte.
const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// An incremental CRC-32 accumulator.
///
/// Feed bytes with [`update`](Self::update) in any number of chunks, then take
/// the result with [`finalize`](Self::finalize). Splitting a buffer across
/// several `update` calls yields the same value as one call over the
/// concatenation, which is what lets the loader hash an image as it streams in.
#[derive(Debug, Clone)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    /// Creates an accumulator primed with the standard initial value.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// Folds `bytes` into the running CRC.
    #[inline]
    pub fn update(&mut self, bytes: &[u8]) {
        let mut state = self.state;
        for &b in bytes {
            let idx = ((state ^ u32::from(b)) & 0xff) as usize;
            state = (state >> 8) ^ TABLE[idx];
        }
        self.state = state;
    }

    /// Consumes the accumulator and returns the final CRC-32 value.
    #[inline]
    #[must_use]
    pub const fn finalize(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

impl Default for Crc32 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// Computes the CRC-32 of `data` in one call.
#[inline]
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Canonical CRC-32 check values.
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn streaming_matches_oneshot() {
        let data = b"the whole payload, split however you like";
        let oneshot = crc32(data);
        let mut acc = Crc32::new();
        acc.update(&data[..7]);
        acc.update(&data[7..7]); // empty chunk is a no-op
        acc.update(&data[7..20]);
        acc.update(&data[20..]);
        assert_eq!(acc.finalize(), oneshot);
    }
}
