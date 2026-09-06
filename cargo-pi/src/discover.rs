//! Serial device selection.
//!
//! An explicit `--port`/config value is used verbatim. Otherwise the USB serial
//! adapters are enumerated: on macOS the callout (`/dev/cu.*`) node is preferred
//! over its dial-in (`/dev/tty.*`) twin. Exactly one candidate is used
//! automatically; zero or many is an error that lists what was found.

use serialport::SerialPortType;

use crate::Result;

/// Resolves the serial device to open.
pub(crate) fn resolve_port(requested: Option<&str>) -> Result<String> {
    if let Some(p) = requested {
        return Ok(p.to_string());
    }

    let ports = serialport::available_ports()
        .map_err(|e| format!("could not enumerate serial ports: {e}"))?;

    let mut candidates: Vec<String> = ports
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .map(|p| p.port_name)
        .collect();

    // On macOS, drop the /dev/tty.* dial-in duplicate when the /dev/cu.*
    // callout node for the same device is present.
    if candidates.iter().any(|n| n.starts_with("/dev/cu.")) {
        candidates.retain(|n| !n.starts_with("/dev/tty."));
    }

    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(
            "no USB serial device found; pass --port <dev> (e.g. /dev/cu.usbserial-XXXX)".into(),
        ),
        _ => Err(format!(
            "multiple USB serial devices found: {}; pass --port to choose one",
            candidates.join(", ")
        )
        .into()),
    }
}
