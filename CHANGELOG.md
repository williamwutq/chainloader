# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Entry-contract clock guarantees. The loader validates `CNTFRQ_EL0` at boot
  (nonzero and cross-checked against the 1 MHz system timer) and **refuses to
  boot a payload** if it is invalid, so a payload never inherits a broken
  generic-timer timebase. It also pins the ARM core clock to 1 GHz (the Zero 2
  W's rated maximum, clamped by firmware so nothing is overclocked) via the
  VideoCore mailbox and reports the achieved rate to the payload in `x9`
  (re-pinned on every `HVC #0` reload). See `docs/ENTRY_CONTRACT.md`.

### Changed

- Entry ABI generation 2 (`x6`): the register handoff now carries the pinned ARM
  core frequency in `x9`; scrubbed GPRs are `x10`–`x30` (was `x9`–`x30`). A
  payload built against generation 1 must not read `x9`.

### Deprecated

### Removed

### Fixed

### Security
