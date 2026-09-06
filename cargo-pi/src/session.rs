//! The host side of the wire protocol: open the port, handshake, transfer an
//! image with per-frame acknowledgement and retries, boot, and (optionally)
//! act as a console. Uses the shared `chainloader_protocol` codec, so the wire
//! format is defined in exactly one place.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use chainloader_protocol::{
    Ack, DataFrame, Decoded, Decoder, ErrorMsg, FrameType, Hello, ImageHeader, MAX_FRAME,
    MAX_PAYLOAD, PROTOCOL_VERSION, Ready, encode_frame,
};
use serialport::SerialPort;

use crate::Result;
use crate::artifact::Image;

/// How long to wait for a single reply before giving up (or retrying).
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// How many times to resend a frame that is not acknowledged.
const MAX_RETRIES: u32 = 5;

/// An open protocol session over a serial port.
pub(crate) struct Session {
    port: Box<dyn SerialPort>,
    decoder: Decoder,
}

impl Session {
    /// Opens `port_name` at `baud` (8N1) and readies a decoder.
    pub(crate) fn open(port_name: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| format!("could not open {port_name} at {baud} baud: {e}"))?;
        Ok(Self {
            port,
            decoder: Decoder::new(),
        })
    }

    /// Sends HELLO and waits for the loader's READY, returning its capabilities.
    pub(crate) fn handshake(&mut self) -> Result<Ready> {
        let hello = Hello {
            host_version: u32::from(PROTOCOL_VERSION),
        };
        self.write_frame(FrameType::Hello, &hello.to_bytes())?;
        let payload = self.expect(FrameType::Ready)?;
        Ready::from_bytes(&payload).map_err(|e| format!("malformed READY: {e}").into())
    }

    /// Transfers `image` to the loader: HEADER, streamed DATA with retries, and
    /// the loader's end-to-end CRC check on the final acknowledgement.
    pub(crate) fn transfer(&mut self, ready: &Ready, image: &Image) -> Result<()> {
        let len = u32::try_from(image.bytes.len()).map_err(|_| "image too large")?;
        preflight(ready, image, len)?;

        let header = ImageHeader {
            load_addr: image.load_addr,
            image_len: len,
            image_crc32: image.crc32,
            entry_off: image.entry_off,
            flags: 0,
        };
        self.write_frame(FrameType::Header, &header.to_bytes())?;
        self.expect(FrameType::Ack)?; // loader accepted the header (next = 0)

        let chunk = (ready.max_chunk as usize).clamp(1, MAX_PAYLOAD - DataFrame::HEADER);
        let mut buf = [0u8; MAX_PAYLOAD];
        let mut offset = 0usize;
        while offset < image.bytes.len() {
            let end = (offset + chunk).min(image.bytes.len());
            let frame = DataFrame {
                offset: offset as u32,
                chunk: &image.bytes[offset..end],
            };
            let n = frame
                .encode(&mut buf)
                .map_err(|e| format!("encoding DATA: {e}"))?;
            self.send_data(&buf[..n], end as u32)?;
            offset = end;
            print_progress(offset, image.bytes.len());
        }
        eprintln!();
        Ok(())
    }

    /// Sends a single DATA frame and waits for the acknowledgement, resending on
    /// error/timeout or a mismatched offset up to [`MAX_RETRIES`] times.
    fn send_data(&mut self, payload: &[u8], expected_next: u32) -> Result<()> {
        let mut last_err: Option<String> = None;
        for _ in 0..=MAX_RETRIES {
            self.write_frame(FrameType::Data, payload)?;
            match self.expect(FrameType::Ack) {
                Ok(ack_bytes) => {
                    let ack =
                        Ack::from_bytes(&ack_bytes).map_err(|e| format!("malformed ACK: {e}"))?;
                    if ack.next_offset == expected_next {
                        return Ok(());
                    }
                    last_err = Some(format!(
                        "loader expected offset {}, we are at {expected_next}",
                        ack.next_offset
                    ));
                }
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        Err(format!(
            "giving up on chunk ending at {expected_next} after {MAX_RETRIES} retries: {}",
            last_err.unwrap_or_else(|| "unknown error".into())
        )
        .into())
    }

    /// Sends BOOT and waits for the loader's acknowledgement before it jumps.
    pub(crate) fn boot(&mut self) -> Result<()> {
        self.write_frame(FrameType::Boot, &[])?;
        self.expect(FrameType::Ack)?;
        Ok(())
    }

    /// Attaches as a plain serial console: serial → stdout, stdin → serial,
    /// until end-of-input or an error. Consumes the session.
    pub(crate) fn console(self) -> Result<()> {
        let mut reader = self.port;
        let mut writer = reader
            .try_clone()
            .map_err(|e| format!("cannot split port: {e}"))?;

        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 256];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || writer.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        });

        let mut stdout = std::io::stdout();
        let mut buf = [0u8; 256];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    stdout.write_all(&buf[..n])?;
                    stdout.flush()?;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Encodes and writes one frame, flushing the port.
    fn write_frame(&mut self, ty: FrameType, payload: &[u8]) -> Result<()> {
        let mut buf = [0u8; MAX_FRAME];
        let frame =
            encode_frame(ty, payload, &mut buf).map_err(|e| format!("encoding {ty:?}: {e}"))?;
        self.port.write_all(frame)?;
        self.port.flush()?;
        Ok(())
    }

    /// Reads frames until one arrives (or timeout), requiring type `want`.
    /// An ERROR frame is turned into a descriptive error.
    fn expect(&mut self, want: FrameType) -> Result<Vec<u8>> {
        let (ty, payload) = self.read_frame(REPLY_TIMEOUT)?;
        if ty == want {
            return Ok(payload);
        }
        if ty == FrameType::Error {
            let msg =
                ErrorMsg::from_bytes(&payload).map_err(|e| format!("malformed ERROR: {e}"))?;
            let code = msg
                .known_code()
                .map_or_else(|| format!("code {}", msg.code), |c| c.to_string());
            return Err(format!("loader reported error: {code} (detail {:#x})", msg.detail).into());
        }
        Err(format!("unexpected {ty:?} frame while awaiting {want:?}").into())
    }

    /// Reads and decodes one frame, giving up after `timeout`.
    fn read_frame(&mut self, timeout: Duration) -> Result<(FrameType, Vec<u8>)> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 64];
        while Instant::now() < deadline {
            let n = match self.port.read(&mut buf) {
                Ok(n) => n,
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
                Err(e) => return Err(e.into()),
            };
            for &b in &buf[..n] {
                match self.decoder.push(b) {
                    Decoded::Frame(ty) => return Ok((ty, self.decoder.payload().to_vec())),
                    // A host-side decode error is line noise; keep scanning.
                    Decoded::Error(_) | Decoded::None => {}
                }
            }
        }
        Err("timed out waiting for a reply from the loader".into())
    }
}

/// Sanity-checks the image against the loader's advertised limits before we
/// start, so a doomed transfer fails fast with a clear message.
fn preflight(ready: &Ready, image: &Image, len: u32) -> Result<()> {
    if len > ready.max_image_len {
        return Err(format!(
            "image is {len} bytes but the loader accepts at most {}",
            ready.max_image_len
        )
        .into());
    }
    if ready.alignment != 0 && image.load_addr % u64::from(ready.alignment) != 0 {
        return Err(format!(
            "load address {:#x} is not aligned to {:#x}",
            image.load_addr, ready.alignment
        )
        .into());
    }
    let end = image.load_addr + u64::from(len);
    if image.load_addr < ready.load_addr_min || end > ready.load_addr_max {
        return Err(format!(
            "image [{:#x}, {end:#x}) is outside the loader's window [{:#x}, {:#x})",
            image.load_addr, ready.load_addr_min, ready.load_addr_max
        )
        .into());
    }
    Ok(())
}

/// Prints an in-place transfer progress line to stderr.
fn print_progress(done: usize, total: usize) {
    let pct = (done * 100).checked_div(total).unwrap_or(100);
    eprint!("\r  transferring {done}/{total} bytes ({pct}%)");
}
