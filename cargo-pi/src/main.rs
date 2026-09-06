//! `cargo pi` — load AArch64 bare-metal images onto a Raspberry Pi over UART.
//!
//! Installed as the `cargo-pi` binary, Cargo exposes it as the `cargo pi`
//! subcommand. When Cargo invokes a subcommand it passes the subcommand name as
//! the first argument, so `cargo pi load` arrives as `["cargo-pi", "pi",
//! "load", ..]`; this entry point tolerates both that and a direct
//! `cargo-pi load` invocation.
//!
//! - `load` builds the payload, transfers it to the loader, and boots it.
//! - `console` attaches to the serial port as a plain UART console.
//!
//! Defaults suit the Pi 2 dev loop and can be overridden per project in
//! `[package.metadata.pi]` or per invocation with flags. See the protocol in
//! `../docs/PROTOCOL.md`.

mod artifact;
mod config;
mod discover;
mod session;

use std::process::ExitCode;

use config::{CliOverrides, Config, parse_u64};
use session::Session;

/// Crate-wide fallible result with boxed errors, so the modules stay dependency-light.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "\
cargo pi — load AArch64 bare-metal images onto a Raspberry Pi over UART

USAGE:
    cargo pi load [OPTIONS]
    cargo pi console [OPTIONS]

COMMANDS:
    load       Build the payload, transfer it to the loader, and boot it
    console    Attach to the loader's serial port as a plain UART console

OPTIONS (load and console):
    --port <DEV>          Serial device (default: sole /dev/cu.* USB adapter)
    --baud <RATE>         Baud rate (default: 115200)

OPTIONS (load only):
    --package <NAME>      Cargo package to build (-p)
    --bin <NAME>          Binary target to build
    --release / --debug   Build profile (default: release)
    --load-address <ADDR> Load address for a raw (non-ELF) image
    --console             Stay attached as a console after loading
    --no-console          Do not attach a console after loading

Configuration also reads [package.metadata.pi] from the payload's Cargo.toml.";

fn main() -> ExitCode {
    // Drop the leading `pi` that Cargo inserts for `cargo pi ...`.
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("pi") {
        args.next();
    }
    let rest: Vec<String> = args.collect();

    let result = match rest.first().map(String::as_str) {
        Some("load") => run_load(&rest[1..]),
        Some("console") => run_console(&rest[1..]),
        None | Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("cargo-pi: unknown command `{other}`\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cargo-pi: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `cargo pi load`: build, transfer, boot, and optionally console.
fn run_load(args: &[String]) -> Result<()> {
    let overrides = parse_overrides(args, true)?;
    let cfg = Config::resolve(overrides)?;

    eprintln!("Building payload...");
    let image = artifact::build(&cfg)?;
    eprintln!(
        "Image: {} bytes @ {:#x}, entry +{:#x}, crc32 {:#010x}",
        image.bytes.len(),
        image.load_addr,
        image.entry_off,
        image.crc32
    );

    let port = discover::resolve_port(cfg.port.as_deref())?;
    eprintln!("Opening {port} @ {} baud...", cfg.baud);
    let mut session = Session::open(&port, cfg.baud)?;

    let ready = session.handshake()?;
    eprintln!(
        "Loader ready (v{}, window [{:#x}, {:#x}), max chunk {}).",
        ready.loader_version, ready.load_addr_min, ready.load_addr_max, ready.max_chunk
    );

    session.transfer(&ready, &image)?;
    session.boot()?;
    eprintln!("Booted.");

    if cfg.console_after {
        eprintln!("Attaching console (Ctrl-C to exit)...");
        session.console()?;
    }
    Ok(())
}

/// `cargo pi console`: attach to the serial port.
fn run_console(args: &[String]) -> Result<()> {
    let overrides = parse_overrides(args, false)?;
    let cfg = Config::resolve(overrides)?;
    let port = discover::resolve_port(cfg.port.as_deref())?;
    eprintln!("Console on {port} @ {} baud (Ctrl-C to exit)...", cfg.baud);
    let session = Session::open(&port, cfg.baud)?;
    session.console()
}

/// Parses command-line flags into [`CliOverrides`]. `load_flags` enables the
/// options that only make sense for `load`.
fn parse_overrides(args: &[String], load_flags: bool) -> Result<CliOverrides> {
    let mut o = CliOverrides::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("missing value for {arg}"))
        };
        match arg.as_str() {
            "--port" => o.port = Some(value()?),
            "--baud" => o.baud = Some(value()?.parse().map_err(|_| "invalid --baud")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ if load_flags => match arg.as_str() {
                "--package" | "-p" => o.package = Some(value()?),
                "--bin" => o.bin = Some(value()?),
                "--release" => o.release = Some(true),
                "--debug" => o.release = Some(false),
                "--load-address" | "--load-addr" => o.load_addr = Some(parse_u64(&value()?)?),
                "--console" => o.console_after = Some(true),
                "--no-console" => o.console_after = Some(false),
                other => return Err(format!("unknown option: {other}").into()),
            },
            other => return Err(format!("unknown option: {other}").into()),
        }
    }
    Ok(o)
}
