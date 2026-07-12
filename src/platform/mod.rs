// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Platform-specific debug operations.
//!
//! Each platform (Nordic, future: STM32, RP2040, ...) implements
//! [`PlatformHandler`] to provide recover / flash / verify / reset on top of
//! the narrow [`DapIo`] debug I/O abstraction, which keeps the handlers
//! transport-agnostic and unit-testable.

pub mod nordic;

use anyhow::{bail, Result};

use crate::dap::io::DapIo;
use crate::hex::Firmware;
use crate::targets::TargetConfig;

/// Progress callback reporting completion as a percentage in `0.0..=100.0`.
pub type ProgressFn<'a> = dyn FnMut(f64) + 'a;

/// Result of a verify operation.
#[derive(Debug, Clone, Copy)]
pub struct VerifyOutcome {
    pub success: bool,
    pub mismatches: usize,
}

/// Contract implemented by every platform handler.
pub trait PlatformHandler {
    /// Recover (unlock / mass erase) the device into a known, accessible state.
    fn recover(&self, io: &mut dyn DapIo, progress: &mut ProgressFn) -> Result<()>;

    /// Write firmware to the device's flash.
    fn flash(
        &self,
        io: &mut dyn DapIo,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<()>;

    /// Read back and compare firmware against the expected image.
    fn verify(
        &self,
        io: &mut dyn DapIo,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<VerifyOutcome>;

    /// Reset the target device (best effort; errors are propagated so the
    /// caller can decide whether they are fatal).
    fn reset(&self, io: &mut dyn DapIo) -> Result<()>;
}

/// Instantiate the platform handler for a target definition.
pub fn handler_for(target: &TargetConfig) -> Result<Box<dyn PlatformHandler>> {
    match target.platform.as_str() {
        "nordic" => Ok(Box::new(nordic::NordicHandler::new(target.clone()))),
        other => bail!("No platform handler registered for platform: {other}"),
    }
}
