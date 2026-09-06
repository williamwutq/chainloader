//! Builds the payload and produces the flat image to transfer.
//!
//! The payload project is expected to build for a bare-metal AArch64 target (it
//! sets that in its own `.cargo/config.toml`, exactly as the loader does). This
//! module runs `cargo build`, finds the resulting executable, and — if it is an
//! ELF — flattens its loadable segments into the raw bytes the loader places in
//! RAM, deriving the load address and entry offset from the ELF itself. A file
//! that is already a flat binary is used as-is at the configured address.

use std::path::PathBuf;
use std::process::Command;

use crate::Result;
use crate::config::{Config, cargo_bin};

/// The image to hand to the loader.
pub(crate) struct Image {
    /// Physical address to load at.
    pub load_addr: u64,
    /// Offset from `load_addr` to the entry point.
    pub entry_off: u32,
    /// The raw bytes to write to RAM.
    pub bytes: Vec<u8>,
    /// CRC-32 of `bytes`, for the header.
    pub crc32: u32,
}

/// Builds the payload per `cfg` and returns the flat image.
pub(crate) fn build(cfg: &Config) -> Result<Image> {
    let exe = cargo_build(cfg)?;
    let data = std::fs::read(&exe).map_err(|e| format!("reading {}: {e}", exe.display()))?;

    let (load_addr, entry_off, bytes) = if is_elf(&data) {
        let (base, entry, image) = flatten_elf(&data)?;
        if cfg.load_addr != crate::config::DEFAULT_LOAD_ADDR && cfg.load_addr != base {
            eprintln!(
                "cargo-pi: warning: configured load-address {:#x} differs from the ELF's \
                 link address {base:#x}; using the ELF address (relocating a non-PIC image \
                 would break it).",
                cfg.load_addr
            );
        }
        (base, entry, image)
    } else {
        (cfg.load_addr, cfg.entry_off, data)
    };

    let crc32 = chainloader_protocol::crc32(&bytes);
    Ok(Image {
        load_addr,
        entry_off,
        bytes,
        crc32,
    })
}

/// Runs `cargo build` with JSON output and returns the built executable path.
fn cargo_build(cfg: &Config) -> Result<PathBuf> {
    let mut cmd = Command::new(cargo_bin());
    cmd.arg("build")
        .args(["--message-format", "json-render-diagnostics"]);
    if cfg.release {
        cmd.arg("--release");
    }
    if let Some(pkg) = &cfg.package {
        cmd.args(["--package", pkg]);
    }
    if let Some(bin) = &cfg.bin {
        cmd.args(["--bin", bin]);
    }

    let out = cmd
        .output()
        .map_err(|e| format!("failed to run cargo build: {e}"))?;
    if !out.status.success() {
        return Err("cargo build failed".into());
    }

    // Scan compiler-artifact messages for the produced executable, taking the
    // last match (the final linked binary), honoring a `--bin` selection.
    let mut executable: Option<PathBuf> = None;
    for line in out.stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let Some(exe) = msg.get("executable").and_then(|e| e.as_str()) else {
            continue;
        };
        if let Some(bin) = &cfg.bin {
            let name = msg.pointer("/target/name").and_then(|n| n.as_str());
            if name != Some(bin.as_str()) {
                continue;
            }
        }
        executable = Some(PathBuf::from(exe));
    }

    executable.ok_or_else(|| {
        "cargo build produced no executable (is this a binary crate for the Pi target?)".into()
    })
}

/// Returns whether `data` starts with the ELF magic.
fn is_elf(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == b"\x7FELF"
}

// ELF64 header/program-header field offsets (little-endian AArch64).
const E_ENTRY: usize = 24;
const E_PHOFF: usize = 32;
const E_PHENTSIZE: usize = 54;
const E_PHNUM: usize = 56;
const PT_LOAD: u32 = 1;
const PH_TYPE: usize = 0;
const PH_OFFSET: usize = 8;
const PH_PADDR: usize = 24;
const PH_FILESZ: usize = 32;

/// Flattens an ELF64 into `(link_base, entry_offset, bytes)` the way
/// `objcopy -O binary` does: `PT_LOAD` segments laid out by physical address,
/// with BSS (`memsz` beyond `filesz`) left implicit for the payload to clear.
fn flatten_elf(data: &[u8]) -> Result<(u64, u32, Vec<u8>)> {
    // e_ident[4] = EI_CLASS (2 = 64-bit), e_ident[5] = EI_DATA (1 = LE).
    if data.get(4) != Some(&2) || data.get(5) != Some(&1) {
        return Err("only 64-bit little-endian ELF images are supported".into());
    }
    let entry = rd_u64(data, E_ENTRY)?;
    let phoff = usize::try_from(rd_u64(data, E_PHOFF)?).map_err(|_| "bad e_phoff")?;
    let phentsize = usize::from(rd_u16(data, E_PHENTSIZE)?);
    let phnum = usize::from(rd_u16(data, E_PHNUM)?);

    let mut segments = Vec::new();
    let mut base = u64::MAX;
    let mut top = 0u64;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if rd_u32(data, ph + PH_TYPE)? != PT_LOAD {
            continue;
        }
        let offset = usize::try_from(rd_u64(data, ph + PH_OFFSET)?).map_err(|_| "bad p_offset")?;
        let paddr = rd_u64(data, ph + PH_PADDR)?;
        let filesz = usize::try_from(rd_u64(data, ph + PH_FILESZ)?).map_err(|_| "bad p_filesz")?;
        if filesz == 0 {
            continue;
        }
        let src = data
            .get(offset..offset + filesz)
            .ok_or("ELF segment extends past end of file")?;
        base = base.min(paddr);
        top = top.max(paddr + filesz as u64);
        segments.push((paddr, src));
    }

    if segments.is_empty() {
        return Err("ELF has no loadable data".into());
    }

    let size = usize::try_from(top - base).map_err(|_| "image too large")?;
    let mut image = vec![0u8; size];
    for (paddr, src) in segments {
        let start = (paddr - base) as usize;
        image[start..start + src.len()].copy_from_slice(src);
    }

    let entry_off = u32::try_from(entry - base).map_err(|_| "entry offset out of range")?;
    Ok((base, entry_off, image))
}

fn rd_u16(d: &[u8], off: usize) -> Result<u16> {
    let b = d.get(off..off + 2).ok_or("truncated ELF")?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}
fn rd_u32(d: &[u8], off: usize) -> Result<u32> {
    let b = d.get(off..off + 4).ok_or("truncated ELF")?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn rd_u64(d: &[u8], off: usize) -> Result<u64> {
    let b = d.get(off..off + 8).ok_or("truncated ELF")?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `PT_LOAD` segment: `(paddr, bytes)`.
    struct Seg(u64, &'static [u8]);

    /// Builds a minimal ELF64 (LE) with `e_entry` and the given `PT_LOAD`
    /// segments laid out contiguously in the file after the program headers.
    fn build_elf(entry: u64, segs: &[Seg]) -> Vec<u8> {
        const EH: usize = 64;
        const PH: usize = 56;
        let ph_off = EH;
        let mut data_off = EH + PH * segs.len();

        let mut out = vec![0u8; data_off + segs.iter().map(|s| s.1.len()).sum::<usize>()];
        out[..4].copy_from_slice(b"\x7FELF");
        out[4] = 2; // 64-bit
        out[5] = 1; // little-endian
        out[6] = 1; // version
        out[16..18].copy_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
        out[18..20].copy_from_slice(&0xB7u16.to_le_bytes()); // e_machine = AArch64
        out[24..32].copy_from_slice(&entry.to_le_bytes());
        out[32..40].copy_from_slice(&(ph_off as u64).to_le_bytes());
        out[54..56].copy_from_slice(&(PH as u16).to_le_bytes());
        out[56..58].copy_from_slice(&(segs.len() as u16).to_le_bytes());

        for (i, seg) in segs.iter().enumerate() {
            let ph = ph_off + i * PH;
            out[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
            out[ph + 8..ph + 16].copy_from_slice(&(data_off as u64).to_le_bytes()); // p_offset
            out[ph + 24..ph + 32].copy_from_slice(&seg.0.to_le_bytes()); // p_paddr
            out[ph + 32..ph + 40].copy_from_slice(&(seg.1.len() as u64).to_le_bytes()); // p_filesz
            out[ph + 40..ph + 48].copy_from_slice(&(seg.1.len() as u64).to_le_bytes()); // p_memsz
            out[data_off..data_off + seg.1.len()].copy_from_slice(seg.1);
            data_off += seg.1.len();
        }
        out
    }

    #[test]
    fn is_elf_recognizes_magic() {
        assert!(is_elf(b"\x7FELF and more"));
        assert!(!is_elf(b"\x7FELg"));
        assert!(!is_elf(b"raw"));
    }

    #[test]
    fn flattens_single_segment_with_entry_offset() {
        let elf = build_elf(0x0020_0002, &[Seg(0x0020_0000, &[0xDE, 0xAD, 0xBE, 0xEF])]);
        let (base, entry_off, bytes) = flatten_elf(&elf).unwrap();
        assert_eq!(base, 0x0020_0000);
        assert_eq!(entry_off, 2);
        assert_eq!(bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn flattens_two_segments_zero_filling_the_gap() {
        // Segments at 0x200000 (4 bytes) and 0x200008 (2 bytes): the 4-byte gap
        // between them must be zero-filled, matching `objcopy -O binary`.
        let elf = build_elf(
            0x0020_0000,
            &[Seg(0x0020_0000, &[1, 2, 3, 4]), Seg(0x0020_0008, &[5, 6])],
        );
        let (base, entry_off, bytes) = flatten_elf(&elf).unwrap();
        assert_eq!(base, 0x0020_0000);
        assert_eq!(entry_off, 0);
        assert_eq!(bytes, vec![1, 2, 3, 4, 0, 0, 0, 0, 5, 6]);
    }

    #[test]
    fn rejects_non_64bit_le() {
        let mut elf = build_elf(0x1000, &[Seg(0x1000, &[0])]);
        elf[4] = 1; // 32-bit
        assert!(flatten_elf(&elf).is_err());
    }

    #[test]
    fn rejects_elf_with_no_loadable_data() {
        let elf = build_elf(0x1000, &[]);
        assert!(flatten_elf(&elf).is_err());
    }
}
