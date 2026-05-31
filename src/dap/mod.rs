// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Debug probe access via probe-rs.
//!
//! This module is the only place that knows how a probe is enumerated, selected
//! and opened. Everything above it operates on probe-rs's `ArmDebugInterface`
//! abstraction, so a future non-USB transport (e.g. a Bluetooth LE CMSIS-DAP
//! probe implemented as a custom `probe_rs::probe::DebugProbe`) can be slotted
//! in here without changing the platform/flash/RTT layers.

pub mod arm;

use anyhow::{anyhow, bail, Context, Result};
use probe_rs::architecture::arm::{sequences::DefaultArmSequence, ArmDebugInterface};
use probe_rs::probe::{list::Lister, DebugProbeInfo, DebugProbeSelector, WireProtocol};

/// Options controlling probe selection and configuration.
#[derive(Debug, Default, Clone)]
pub struct ProbeOptions {
    /// Optional probe selector string, e.g. `"2886:000c"` or `"vid:pid:serial"`.
    pub selector: Option<String>,
    /// Optional SWD clock speed in kHz.
    pub speed_khz: Option<u32>,
}

/// List all connected debug probes.
pub fn list_probes() -> Vec<DebugProbeInfo> {
    Lister::new().list_all()
}

/// Open a probe and initialize the ARM debug interface (SWD).
///
/// Uses [`DefaultArmSequence`], which brings up the debug port without needing
/// a chip-specific target definition. This is sufficient to reach the Nordic
/// CTRL-AP (for recovery) and the MEM-AP (for flashing / RTT).
pub fn open_interface(opts: &ProbeOptions) -> Result<Box<dyn ArmDebugInterface>> {
    let lister = Lister::new();

    let selector: DebugProbeSelector = match &opts.selector {
        Some(s) => s
            .parse()
            .with_context(|| format!("Invalid --probe selector: {s}"))?,
        None => {
            let probes = lister.list_all();
            match probes.len() {
                0 => bail!("No debug probes found. Connect a CMSIS-DAP probe and try again."),
                1 => {}
                n => tracing::warn!("{n} probes found; using the first. Use --probe to choose."),
            }
            selector_string(&probes[0])
                .parse()
                .expect("a selector built from DebugProbeInfo is always valid")
        }
    };

    let mut probe = lister
        .open(selector)
        .context("Failed to open the selected debug probe")?;
    tracing::info!("Opened probe: {}", probe.get_name());

    if let Err(err) = probe.select_protocol(WireProtocol::Swd) {
        tracing::warn!("Could not select SWD protocol: {err}");
    }
    if let Some(khz) = opts.speed_khz {
        match probe.set_speed(khz) {
            Ok(actual) => tracing::info!("Probe speed set to {actual} kHz"),
            Err(err) => tracing::warn!("Could not set speed to {khz} kHz: {err}"),
        }
    }

    probe
        .attach_to_unspecified()
        .context("Failed to attach to the debug probe")?;

    let interface = probe
        .try_into_arm_debug_interface(DefaultArmSequence::create())
        .map_err(|(_probe, err)| anyhow!("Failed to initialize ARM debug interface: {err}"))?;

    Ok(interface)
}

/// Build a probe-rs selector string (`vid:pid` or `vid:pid:serial`, hex) from
/// probe info.
fn selector_string(info: &DebugProbeInfo) -> String {
    match &info.serial_number {
        Some(serial) => format!("{:04x}:{:04x}:{}", info.vendor_id, info.product_id, serial),
        None => format!("{:04x}:{:04x}", info.vendor_id, info.product_id),
    }
}
