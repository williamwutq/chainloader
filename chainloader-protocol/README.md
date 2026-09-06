# chainloader-protocol

The wire protocol shared by the Raspberry Pi UART chainloader and the host
`cargo-pi` tool: a small, versioned, endian-explicit framing over a raw UART
byte stream.

`no_std` and allocation-free, so the identical encode/decode code runs on the
bare-metal loader and on the host. There is one wire format, defined once, and
it is fully testable off-target.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](../LICENSE)

## Frame layout

```text
offset  size  field
  0      2    magic    = 0x50AE, little-endian
  2      1    version  = PROTOCOL_VERSION
  3      1    type     = FrameType
  4      2    length   = payload byte count, little-endian
  6      N    payload
 6+N     4    crc32    = CRC-32 over version..payload, little-endian
```

The magic is only a resync marker; the CRC covers everything after it. All
multi-byte fields are little-endian, matching the AArch64 default.

See [`../docs/PROTOCOL.md`](../docs/PROTOCOL.md) for the full handshake and
[`../docs/ENTRY_CONTRACT.md`](../docs/ENTRY_CONTRACT.md) for the jump contract.

## Status

Implemented: [`crc`] (CRC-32/ISO-HDLC, one-shot and streaming) and [`frame`]
(`FrameType`, `encode_frame`, size helpers). Next up (`../PLANNED.md`): the
streaming resync decoder and typed payload structs.

## License

MIT — see [LICENSE](../LICENSE).
