// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Command-line interface definition (clap).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::dap::ProbeOptions;

/// FreeOCD CLI: flash, recover, verify and RTT-debug ARM Cortex-M MCUs via CMSIS-DAP.
#[derive(Parser, Debug)]
#[command(name = "freeocd", version, about, long_about = None)]
pub struct Cli {
    /// Enable verbose (debug-level) logging.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Subcommand to run. When omitted, the help text is printed.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// List connected debug probes and available targets.
    List,
    /// Mass-erase, flash firmware, optionally verify, then reset.
    Flash(FlashArgs),
    /// Mass-erase (unlock) the device, then reset.
    Recover(TargetArgs),
    /// Read back flash and compare against a firmware file.
    Verify(VerifyArgs),
    /// Reset the target device.
    Reset(TargetArgs),
    /// Open a SEGGER RTT terminal.
    Rtt(RttArgs),
}

/// Common probe selection options.
#[derive(Args, Debug, Clone)]
pub struct ProbeArgs {
    /// Probe selector "VID:PID" or "VID:PID:Serial" (hex). Defaults to the first probe found.
    #[arg(long)]
    pub probe: Option<String>,

    /// SWD clock speed in kHz.
    #[arg(long)]
    pub speed: Option<u32>,
}

impl ProbeArgs {
    /// Convert CLI probe args into [`ProbeOptions`].
    pub fn to_options(&self) -> ProbeOptions {
        ProbeOptions {
            selector: self.probe.clone(),
            speed_khz: self.speed,
        }
    }
}

/// Arguments for commands that only need a target and a probe.
#[derive(Args, Debug)]
pub struct TargetArgs {
    /// Target id, e.g. "nordic/nrf54/nrf54l15".
    #[arg(short, long)]
    pub target: String,

    #[command(flatten)]
    pub probe: ProbeArgs,
}

/// Arguments for the `flash` command.
#[derive(Args, Debug)]
pub struct FlashArgs {
    /// Target id, e.g. "nordic/nrf54/nrf54l15".
    #[arg(short, long)]
    pub target: String,

    /// Path to the firmware `.hex` file.
    #[arg(short, long)]
    pub file: PathBuf,

    /// Verify the firmware after flashing.
    #[arg(long)]
    pub verify: bool,

    #[command(flatten)]
    pub probe: ProbeArgs,
}

/// Arguments for the `verify` command.
#[derive(Args, Debug)]
pub struct VerifyArgs {
    /// Target id, e.g. "nordic/nrf54/nrf54l15".
    #[arg(short, long)]
    pub target: String,

    /// Path to the firmware `.hex` file.
    #[arg(short, long)]
    pub file: PathBuf,

    #[command(flatten)]
    pub probe: ProbeArgs,
}

/// Arguments for the `rtt` command.
#[derive(Args, Debug)]
pub struct RttArgs {
    /// Target id, e.g. "nordic/nrf54/nrf54l15".
    #[arg(short, long)]
    pub target: String,

    /// RTT scan start address (default: target SRAM base or 0x20000000).
    #[arg(long, value_parser = parse_u64_maybe_hex)]
    pub scan_addr: Option<u64>,

    /// RTT scan range in bytes (default: 0x10000).
    #[arg(long, value_parser = parse_u64_maybe_hex)]
    pub scan_range: Option<u64>,

    /// Polling interval in milliseconds.
    #[arg(long, default_value_t = 100)]
    pub poll_ms: u64,

    /// Reset the target before attaching RTT.
    #[arg(long)]
    pub reset: bool,

    #[command(flatten)]
    pub probe: ProbeArgs,
}

/// Parse a `u64` from a decimal or `0x`-prefixed hex string.
fn parse_u64_maybe_hex(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u64>().map_err(|e| e.to_string())
    }
}
