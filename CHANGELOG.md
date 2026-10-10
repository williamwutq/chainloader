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
  core frequency in `x9` and the board's peripheral (MMIO) base in `x10` (so a
  payload can locate the UART/GPIO/mailbox/timer without hardcoding it); scrubbed
  GPRs are `x11`–`x30` (was `x9`–`x30`). A payload built against generation 1 must
  not read `x9`/`x10`.

### Deprecated

### Removed

### Fixed

- Reload (`HVC #0`) could leave a secondary core's dirty L1 data cache lines in
  place: set/way maintenance is per-core, so the boot core's full data-cache
  flush cannot reach a secondary's own L1, and lines left from running the
  previous payload with caches on could survive the reload — serving the next
  payload stale data through coherency, or being evicted over the freshly
  loaded image and corrupting it. Each secondary now cleans+invalidates its L1
  data cache (into the shared L2, which the boot core's flush then reaches)
  before re-parking.

### Security
