//! Resolved configuration, merging three sources in increasing precedence:
//! built-in Pi 2 defaults, `[package.metadata.pi]` in the payload's
//! `Cargo.toml`, and command-line flags.

use serde::Deserialize;

use crate::Result;

/// Default serial baud rate.
pub(crate) const DEFAULT_BAUD: u32 = 115_200;
/// Default load address, matching the loader's writable window.
pub(crate) const DEFAULT_LOAD_ADDR: u64 = 0x0020_0000;

/// Fully resolved settings for a `load`/`console` run.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Serial device path, or `None` to auto-discover.
    pub port: Option<String>,
    /// Baud rate.
    pub baud: u32,
    /// Load address for a raw (non-ELF) image; an ELF's own address wins.
    pub load_addr: u64,
    /// Entry offset for a raw image; an ELF's `e_entry` wins.
    pub entry_off: u32,
    /// Cargo package to build (`-p`), or `None` for the default.
    pub package: Option<String>,
    /// Cargo binary target (`--bin`), or `None` for the default.
    pub bin: Option<String>,
    /// Build the release profile.
    pub release: bool,
    /// Stay attached as a console after a successful load.
    pub console_after: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: None,
            baud: DEFAULT_BAUD,
            load_addr: DEFAULT_LOAD_ADDR,
            entry_off: 0,
            package: None,
            bin: None,
            // Release is the sensible default for bare-metal: debug images are
            // far larger, and the documented loop is `cargo build --release`.
            release: true,
            console_after: false,
        }
    }
}

/// Command-line overrides collected by the argument parser. Every field is
/// optional so that unset flags fall through to metadata/defaults.
#[derive(Debug, Default)]
pub(crate) struct CliOverrides {
    pub port: Option<String>,
    pub baud: Option<u32>,
    pub load_addr: Option<u64>,
    pub package: Option<String>,
    pub bin: Option<String>,
    pub release: Option<bool>,
    pub console_after: Option<bool>,
}

/// A `load-address` value in `[package.metadata.pi]`, accepted as either a
/// number or a (possibly `0x`-prefixed) string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Address {
    Num(u64),
    Str(String),
}

impl Address {
    fn to_u64(&self) -> Result<u64> {
        match self {
            Self::Num(n) => Ok(*n),
            Self::Str(s) => parse_u64(s),
        }
    }
}

/// The `[package.metadata.pi]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct MetadataPi {
    port: Option<String>,
    baud: Option<u32>,
    load_address: Option<Address>,
    entry_offset: Option<u32>,
    package: Option<String>,
    bin: Option<String>,
    release: Option<bool>,
    console: Option<bool>,
}

/// Parses a `u64` written in decimal or `0x`-prefixed hex.
pub(crate) fn parse_u64(s: &str) -> Result<u64> {
    let t = s.trim();
    let parsed = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .map_or_else(|| t.parse::<u64>(), |hex| u64::from_str_radix(hex, 16));
    parsed.map_err(|_| format!("invalid integer: {s:?}").into())
}

impl Config {
    /// Resolves the effective configuration for the current directory.
    ///
    /// Reads `[package.metadata.pi]` via `cargo metadata` (best effort — a
    /// missing table or a non-Cargo directory is not an error), then applies
    /// `cli` on top.
    pub(crate) fn resolve(cli: CliOverrides) -> Result<Self> {
        let mut cfg = Self::default();

        if let Some(meta) = read_metadata_pi()? {
            if meta.port.is_some() {
                cfg.port = meta.port;
            }
            if let Some(b) = meta.baud {
                cfg.baud = b;
            }
            if let Some(a) = &meta.load_address {
                cfg.load_addr = a.to_u64()?;
            }
            if let Some(e) = meta.entry_offset {
                cfg.entry_off = e;
            }
            if meta.package.is_some() {
                cfg.package = meta.package;
            }
            if meta.bin.is_some() {
                cfg.bin = meta.bin;
            }
            if let Some(r) = meta.release {
                cfg.release = r;
            }
            if let Some(c) = meta.console {
                cfg.console_after = c;
            }
        }

        if cli.port.is_some() {
            cfg.port = cli.port;
        }
        if let Some(b) = cli.baud {
            cfg.baud = b;
        }
        if let Some(a) = cli.load_addr {
            cfg.load_addr = a;
        }
        if cli.package.is_some() {
            cfg.package = cli.package;
        }
        if cli.bin.is_some() {
            cfg.bin = cli.bin;
        }
        if let Some(r) = cli.release {
            cfg.release = r;
        }
        if let Some(c) = cli.console_after {
            cfg.console_after = c;
        }

        Ok(cfg)
    }
}

/// Runs `cargo metadata` and returns the first `[package.metadata.pi]` found,
/// or `None`. Any failure to invoke or parse Cargo is treated as "no config".
fn read_metadata_pi() -> Result<Option<MetadataPi>> {
    use std::process::Command;

    let out = Command::new(cargo_bin())
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Ok(None),
    };

    let root: serde_json::Value = match serde_json::from_slice(&out) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let packages = root.get("packages").and_then(|p| p.as_array());
    let Some(packages) = packages else {
        return Ok(None);
    };
    for pkg in packages {
        if let Some(pi) = pkg.pointer("/metadata/pi") {
            let meta: MetadataPi = serde_json::from_value(pi.clone())
                .map_err(|e| format!("invalid [package.metadata.pi]: {e}"))?;
            return Ok(Some(meta));
        }
    }
    Ok(None)
}

/// The cargo executable to invoke, honoring the `CARGO` env var Cargo sets for
/// its subcommands.
pub(crate) fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}
