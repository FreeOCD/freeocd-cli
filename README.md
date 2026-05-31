# FreeOCD-CLI

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/FreeOCD/freeocd-cli)
[![CI](https://github.com/FreeOCD/freeocd-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/FreeOCD/freeocd-cli/actions/workflows/ci.yml)
[![Release](https://github.com/FreeOCD/freeocd-cli/actions/workflows/release.yml/badge.svg)](https://github.com/FreeOCD/freeocd-cli/actions/workflows/release.yml)

A cross-platform command-line debugger for ARM Cortex-M microcontrollers over
CMSIS-DAP, built on [probe-rs](https://probe.rs). It is a native Rust port of
[freeocd-web](https://github.com/FreeOCD/freeocd-web) and **reuses the exact same
target definition JSON files**, which are embedded into the binary at build time
so the tool is fully self-contained — a single binary with no runtime
dependencies and no network access.

## Design Philosophy

A debugger is a tool that developers place their trust in during the most
critical moments of development. We hold ourselves to that standard:

- **Reliability** — Every flash and recover operation either completes correctly
  or fails explicitly with clear, actionable guidance.
- **Stability** — Bounded operations, explicit error propagation, and graceful
  Ctrl-C handling ensure the tool never hangs or leaves a device in an unknown
  state.
- **Security** — All inputs are validated; target definitions are embedded at
  build time and the tool makes no network requests at runtime.
- **Portability** — A single self-contained binary that runs on Linux, macOS,
  and Windows, with a modular architecture that welcomes new targets and
  transports.
- **Performance** — Lightweight native Rust with responsive progress reporting
  that keeps you informed during long operations.

## Features

- **list** — enumerate connected debug probes and available targets
- **flash** — mass-erase, program firmware (Intel HEX), optionally verify, then reset
- **recover** — mass-erase / unlock a locked device via the Nordic CTRL-AP, then reset
- **verify** — read back flash and compare against a firmware file
- **reset** — reset the target device
- **rtt** — open a bidirectional SEGGER RTT terminal

## Supported targets

| Target id | Device | Flash | Capabilities |
| --- | --- | --- | --- |
| `nordic/nrf54/nrf54l15` | Nordic nRF54L15 (Cortex-M33) | RRAMC | recover, flash, verify, rtt |

Targets are defined by JSON files shared with `freeocd-web` under
`vendor/freeocd-web/public/targets`. Adding a new device is, in most cases, a
matter of adding its JSON definition and listing it in `index.json` — see
[Adding a new target](#adding-a-new-target).

## Requirements

- [Rust](https://rustup.rs) 1.87 or newer (2021 edition).
- A CMSIS-DAP debug probe (v1/HID or v2/bulk). Most modern boards such as the
  Seeed XIAO nRF54L15 expose an on-board CMSIS-DAP probe.

### Linux USB permissions

On Linux, install the probe-rs udev rules so non-root users can access probes:

```sh
sudo curl -fsSL https://probe.rs/files/69-probe-rs.rules \
  -o /etc/udev/rules.d/69-probe-rs.rules
sudo udevadm control --reload && sudo udevadm trigger
```

## Building

The shared target definitions live in a git submodule, so check it out first:

```sh
git clone https://github.com/FreeOCD/freeocd-cli
cd freeocd-cli
git submodule update --init --recursive
cargo build --release
```

The resulting binary is `target/release/freeocd`. The target JSON files are
embedded at build time, so this single binary is all you need to ship.

> **Note:** if the submodule is not checked out, the build fails fast with a
> message pointing you to `git submodule update --init --recursive`.

## Usage

```sh
# List connected probes and available targets
freeocd list

# Flash firmware and verify it (recovers the device first for a clean state)
freeocd flash --target nordic/nrf54/nrf54l15 --file firmware.hex --verify

# Unlock / mass-erase a locked device
freeocd recover --target nordic/nrf54/nrf54l15

# Verify a previously flashed image
freeocd verify --target nordic/nrf54/nrf54l15 --file firmware.hex

# Reset the target
freeocd reset --target nordic/nrf54/nrf54l15

# Open an RTT terminal (optionally reset first so the firmware starts fresh)
freeocd rtt --target nordic/nrf54/nrf54l15 --reset
```

### Selecting a probe

When more than one probe is connected, choose one explicitly:

```sh
freeocd flash -t nordic/nrf54/nrf54l15 -f firmware.hex \
  --probe 2886:000c           # VID:PID (hex)
  # or --probe 2886:000c:ABC123   # VID:PID:Serial
```

### RTT options

The `rtt` subcommand scans target memory for the SEGGER RTT control block:

- `--scan-addr <addr>` — scan start address (defaults to the target SRAM base, or `0x20000000`)
- `--scan-range <bytes>` — scan range in bytes (default `0x10000`)
- `--poll-ms <ms>` — polling interval in milliseconds (default `100`)
- `--reset` — reset the target before attaching so the firmware starts fresh

Input is line-buffered and forwarded on each newline; press `Ctrl-C` to exit.

### Global options

`--speed <kHz>` sets the SWD clock, and `-v/--verbose` enables debug-level
logging (you can also set `RUST_LOG`).

## Architecture

The codebase mirrors the layered design of `freeocd-web`, replacing the WebUSB /
DAP.js transport with probe-rs:

```
cli.rs / main.rs        Command-line interface (clap) + entry point
   |
ops.rs                  Orchestration of each subcommand + progress reporting
   |
platform/               PlatformHandler trait + nordic.rs (recover/flash/verify/reset)
   |
dap/                    probe-rs probe access; CTRL-AP / MEM-AP (arm.rs) helpers
   |
dapjs/rtt.rs            SEGGER RTT control-block scan and buffer I/O (MIT, dapjs-derived)

targets/                Embedded target JSON + probe filters (build.rs copy + include_dir)
hex.rs                  Intel HEX parser
logging.rs              tracing-subscriber setup
```

### Key components

- **`ops.rs`** — orchestrates each subcommand end to end and drives the
  percentage progress bars.
- **`platform/`** — `PlatformHandler` is the abstract contract
  (`recover`/`flash`/`verify`/`reset`); `nordic.rs` implements it for the Nordic
  CTRL-AP and RRAMC/NVMC flash controllers.
- **`dap/`** — the only module that knows how a probe is enumerated, selected and
  opened. Everything above it works against probe-rs's `ArmDebugInterface`, so a
  new transport can be added here without touching the upper layers.
- **`dapjs/rtt.rs`** — SEGGER RTT control-block scanning and up/down buffer I/O,
  ported from the MIT-licensed dapjs RTT example.
- **`targets/`** — loads the embedded JSON target definitions and the central
  CMSIS-DAP probe filter list.

### Operation flow

**`flash`**

1. Load the target definition and confirm it advertises the `flash` capability.
2. Parse the Intel HEX firmware file.
3. Open the probe and bring up the ARM debug interface (SWD).
4. `recover()` — mass-erase to reach a clean, accessible state.
5. Reinitialize the debug port after the recovery reset.
6. `flash()` — write the firmware.
7. If `--verify` is set, read it back and compare byte-by-byte.
8. `reset()` — restart the device.

**`rtt`**

1. Load the target and confirm the `rtt` capability.
2. Open the probe and ARM debug interface (optionally `--reset` first).
3. Scan target SRAM for the RTT control-block signature.
4. Attach and report the up / down channel counts.
5. Poll the up-buffer to stdout and forward stdin to the down-buffer until
   `Ctrl-C`.

### Future transport extensibility

The tool intentionally keeps the transport concern isolated in the `dap` module.
probe-rs exposes a `DebugProbe` trait and `Probe::from_specific_probe`, so a
future non-USB transport (for example a Bluetooth LE CMSIS-DAP probe) can be
implemented as a custom `DebugProbe` and plugged in without touching the
platform, flash, or RTT layers.

## Target definitions

Target definitions are the **same JSON files** used by `freeocd-web`, embedded
into the binary at build time. Each lives at
`vendor/freeocd-web/public/targets/<platform>/<family>/<mcu>.json` and describes
the CPU, CTRL-AP, flash controller, memory map and capabilities. CMSIS-DAP probe
USB vendor IDs are maintained centrally in `…/targets/probe-filters.json`, not
per target. See `freeocd-web`'s README for the full schema and an annotated
example.

### Adding a new target

1. Add a JSON definition under
   `vendor/freeocd-web/public/targets/<platform>/<family>/<mcu>.json`.
2. Append its id to the `targets` array in `index.json`.
3. If the platform is new, implement `PlatformHandler` in `src/platform/` and
   register it in `handler_for()` (`src/platform/mod.rs`).
4. Rebuild — `build.rs` re-embeds the refreshed definitions automatically.

## Project structure

```
src/
├── main.rs           # Entry point: arg parsing, logging, dispatch
├── cli.rs            # clap command / argument definitions
├── ops.rs            # Subcommand orchestration + progress bars
├── hex.rs            # Intel HEX parser
├── logging.rs        # tracing-subscriber setup
├── dap/              # probe-rs probe access
│   ├── mod.rs        #   enumeration / selection / open
│   └── arm.rs        #   CTRL-AP / MEM-AP helpers
├── dapjs/            # SEGGER RTT (MIT, dapjs-derived)
│   ├── mod.rs
│   ├── rtt.rs        #   control-block scan + buffer I/O
│   └── LICENSE       #   bundled dapjs MIT license
├── platform/         # Platform handlers
│   ├── mod.rs        #   PlatformHandler trait + registry
│   └── nordic.rs     #   Nordic CTRL-AP + RRAMC implementation
└── targets/          # Embedded target definitions
    ├── mod.rs        #   include_dir! asset loading
    ├── definition.rs #   TargetConfig schema
    └── probe_filters.rs  # CMSIS-DAP probe VID list
build.rs              # Copies freeocd-web target JSON into OUT_DIR for embedding
vendor/freeocd-web/   # Shared web project (git submodule; source of target JSON)
```

## License

This project is licensed under the BSD 3-Clause License — see [LICENSE](LICENSE).
Copyright (c) 2026, FreeOCD.

### Third-party attribution

The relevant source files carry per-file attribution notices for the following
third-party sources:

- **[dapjs](https://github.com/ARMmbed/dapjs)** (`examples/rtt/rtt.js`) — MIT
  License, Copyright (C) 2021 Ciro Cattuto. The SEGGER RTT control-block handling
  in `src/dapjs/rtt.rs` is based on this implementation; the corresponding MIT
  license text is bundled at `src/dapjs/LICENSE`.
- **Nordic Semiconductor nRF54L15 Product Specification** — source of the
  hardware register definitions (RRAMC, CTRL-AP, flash memory map).
- **[platform-seeedboards](https://github.com/Seeed-Studio/platform-seeedboards/)**
  (Apache License 2.0) — its OpenOCD `nrf54l.cfg` was used as a cross-reference
  for CTRL-AP register offsets, IDR values and RRAMC programming procedures.
- **[OpenOCD](https://openocd.org/)** (GPL-2.0) — its `nrf52.cfg` was used as a
  cross-reference for the Nordic CTRL-AP recovery procedure patterns.
  