//! `cargo pi` — load AArch64 bare-metal images onto a Raspberry Pi over UART.
//!
//! Installed as the `cargo-pi` binary, Cargo exposes it as the `cargo pi`
//! subcommand. When Cargo invokes a subcommand it passes the subcommand name as
//! the first argument, so `cargo pi load` arrives as `["cargo-pi", "pi",
//! "load", ..]`; this entry point tolerates both that and a direct
//! `cargo-pi load` invocation.
//!
//! # Status
//!
//! Argument dispatch and help are wired up. The transport — serial-device
//! discovery, the loader handshake, framed transfer with retries, and the
//! post-load console — is the `cargo-pi` track in `../PLANNED.md`. Each
//! subcommand currently reports that it is not yet implemented rather than
//! pretending to talk to hardware.

use std::process::ExitCode;

const USAGE: &str = "\
cargo pi — load AArch64 bare-metal images onto a Raspberry Pi over UART

USAGE:
    cargo pi <COMMAND>

COMMANDS:
    load       Build (if needed), then transfer the image to the Pi and boot it
    console    Attach to the Pi's serial port as a plain UART console
    help       Show this message

Run `cargo pi <COMMAND> --help` for command-specific options once implemented.
See ../PLANNED.md for the transport roadmap.";

fn main() -> ExitCode {
    // Drop the leading `pi` that Cargo inserts for `cargo pi ...`, so the rest
    // of the code sees a uniform argument list either way.
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("pi") {
        args.next();
    }
    let rest: Vec<String> = args.collect();

    match rest.first().map(String::as_str) {
        Some("load") => not_yet("load"),
        Some("console") => not_yet("console"),
        None | Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("cargo-pi: unknown command `{other}`\n");
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn not_yet(command: &str) -> ExitCode {
    eprintln!(
        "cargo-pi: `{command}` is not implemented yet.\n\
         The serial transport is planned; see ../PLANNED.md.\n\
         Protocol version in use: {} (chainloader-protocol {}).",
        chainloader_protocol::PROTOCOL_VERSION,
        chainloader_protocol::version(),
    );
    ExitCode::FAILURE
}
