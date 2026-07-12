// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Nordic Semiconductor platform handler.
//!
//! Implements CTRL-AP mass-erase recovery, RRAMC/NVMC flash programming,
//! verification and reset for Nordic nRF series microcontrollers, driven by the
//! shared target definition JSON. All hardware access goes through the narrow
//! [`DapIo`] abstraction (CTRL-AP register transfers and MEM-AP word
//! transfers), which keeps this logic unit-testable against a mock.

mod flash;
#[cfg(test)]
mod tests;

use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use super::{PlatformHandler, ProgressFn, VerifyOutcome};
use crate::dap::io::{try_read_ap, DapIo};
use crate::hex::Firmware;
use crate::targets::definition::{CtrlAp, EraseAllStatus, FlashController, TargetConfig};

// CTRL-AP register offsets (common across the Nordic nRF series).
const CTRL_AP_RESET: u64 = 0x000;
const CTRL_AP_ERASEALL: u64 = 0x004;
const CTRL_AP_ERASEALLSTATUS: u64 = 0x008;
const CTRL_AP_ERASEPROTECTSTATUS: u64 = 0x00C;
const CTRL_AP_IDR: u64 = 0x0FC;

/// Delays and timeouts used by the handler. Tests substitute near-zero values
/// so the state-machine logic can be exercised without real waits.
#[derive(Debug, Clone)]
struct Timing {
    /// Timeout for each ERASEALL wait phase (start / complete).
    erase_timeout: Duration,
    /// Polling interval while waiting on ERASEALLSTATUS.
    erase_interval: Duration,
    /// Timeout while waiting for the flash controller READY flag.
    ready_timeout: Duration,
    /// Polling interval while waiting on the READY flag.
    ready_interval: Duration,
    /// Short settle delay between CTRL-AP register writes.
    settle_short: Duration,
    /// Stabilization delay after the post-erase reset.
    settle_reset: Duration,
    /// Delay after a debug-port reinitialize.
    reconnect: Duration,
    /// Settle delay after a plain device reset.
    post_reset: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            erase_timeout: Duration::from_secs(30),
            erase_interval: Duration::from_millis(100),
            ready_timeout: Duration::from_secs(1),
            ready_interval: Duration::from_millis(10),
            settle_short: Duration::from_millis(10),
            settle_reset: Duration::from_millis(500),
            reconnect: Duration::from_millis(200),
            post_reset: Duration::from_millis(100),
        }
    }
}

impl Timing {
    /// All-zero timing for tests: polls run exactly one check and no sleeps.
    #[cfg(test)]
    fn instant() -> Self {
        Self {
            erase_timeout: Duration::ZERO,
            erase_interval: Duration::ZERO,
            ready_timeout: Duration::ZERO,
            ready_interval: Duration::ZERO,
            settle_short: Duration::ZERO,
            settle_reset: Duration::ZERO,
            reconnect: Duration::ZERO,
            post_reset: Duration::ZERO,
        }
    }
}

/// Poll `check` every `interval` until it yields a value or `timeout` elapses.
/// `on_tick` receives the elapsed fraction (`0.0..1.0`) for progress reporting.
fn poll_until<T>(
    timeout: Duration,
    interval: Duration,
    mut on_tick: impl FnMut(f64),
    mut check: impl FnMut() -> Option<T>,
) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return None;
        }
        on_tick((elapsed.as_secs_f64() / timeout.as_secs_f64()).min(1.0));
        sleep(interval);
    }
}

/// Outcome of one ERASEALLSTATUS wait phase.
enum EraseWait {
    Busy,
    Done,
    Error,
}

/// Nordic platform handler.
pub struct NordicHandler {
    cfg: TargetConfig,
    timing: Timing,
}

impl NordicHandler {
    /// Create a handler bound to a parsed target definition.
    pub fn new(cfg: TargetConfig) -> Self {
        Self {
            cfg,
            timing: Timing::default(),
        }
    }

    /// Create a handler with near-zero delays for tests.
    #[cfg(test)]
    fn new_for_test(cfg: TargetConfig) -> Self {
        Self {
            cfg,
            timing: Timing::instant(),
        }
    }

    /// Borrow the target's CTRL-AP definition, or error if it is missing.
    fn ctrl_ap(&self) -> Result<&CtrlAp> {
        self.cfg
            .ctrl_ap
            .as_ref()
            .ok_or_else(|| anyhow!("target '{}' has no ctrlAp definition", self.cfg.id))
    }

    /// Borrow the target's ERASEALLSTATUS code mapping, or error if missing.
    fn erase_status(&self) -> Result<&EraseAllStatus> {
        self.cfg
            .erase_all_status
            .as_ref()
            .ok_or_else(|| anyhow!("target '{}' has no eraseAllStatus definition", self.cfg.id))
    }

    /// Borrow the target's flash controller definition, or error if missing.
    fn flash_controller(&self) -> Result<&FlashController> {
        self.cfg
            .flash_controller
            .as_ref()
            .ok_or_else(|| anyhow!("target '{}' has no flashController definition", self.cfg.id))
    }

    /// Wait for ERASEALLSTATUS to reach one of the known states, mapping the
    /// progress into `progress_from..progress_to` percent. When `busy_is_wait`
    /// is set, a BUSY status keeps polling instead of being reported.
    fn wait_erase_status(
        &self,
        io: &mut dyn DapIo,
        progress: &mut ProgressFn,
        (progress_from, progress_to): (f64, f64),
        busy_is_wait: bool,
    ) -> Option<EraseWait> {
        let ap = self.ctrl_ap().ok()?.num;
        let status = self.erase_status().ok()?.clone();
        poll_until(
            self.timing.erase_timeout,
            self.timing.erase_interval,
            |f| progress(progress_from + f * (progress_to - progress_from)),
            || match try_read_ap(io, ap, CTRL_AP_ERASEALLSTATUS) {
                Some(v) if v == status.busy && busy_is_wait => None,
                Some(v) if v == status.busy => Some(EraseWait::Busy),
                Some(v) if v == status.error => Some(EraseWait::Error),
                Some(v) if v == status.ready_to_reset => Some(EraseWait::Done),
                _ => None,
            },
        )
    }

    /// Trigger ERASEALL and wait for completion. Returns `false` (rather than
    /// an error) on a recoverable failure so the caller can retry.
    fn attempt_erase_all(
        &self,
        io: &mut dyn DapIo,
        progress: &mut ProgressFn,
        is_retry: bool,
    ) -> Result<bool> {
        let ap = self.ctrl_ap()?.num;
        // Validate the status mapping up front so a missing definition surfaces
        // as a configuration error rather than a polling timeout.
        self.erase_status()?;
        let prefix = if is_retry { "[Retry] " } else { "" };

        tracing::info!("{prefix}Resetting ERASEALL task...");
        io.write_ap(ap, CTRL_AP_ERASEALL, 0)?;
        sleep(self.timing.settle_short);

        tracing::info!("{prefix}Triggering mass erase (ERASEALL)...");
        io.write_ap(ap, CTRL_AP_ERASEALL, 1)?;

        // Phase 1: wait for the BUSY state to appear.
        tracing::info!("{prefix}Waiting for erase to start...");
        match self.wait_erase_status(io, progress, (0.0, 30.0), false) {
            Some(EraseWait::Done) => {
                tracing::info!("{prefix}Device already erased (READYTORESET)");
                return Ok(true);
            }
            Some(EraseWait::Error) => {
                tracing::error!("{prefix}Erase failed with ERROR status");
                return Ok(false);
            }
            Some(EraseWait::Busy) => {
                tracing::info!("{prefix}Erase in progress (BUSY)...");
            }
            None => {
                tracing::error!("{prefix}Timeout waiting for erase to start");
                return Ok(false);
            }
        }

        // Phase 2: wait for a terminal state (READYTORESET or ERROR).
        tracing::info!("{prefix}Waiting for erase to complete...");
        match self.wait_erase_status(io, progress, (30.0, 80.0), true) {
            Some(EraseWait::Done) => {
                tracing::info!("{prefix}Erase completed successfully (READYTORESET)");
                Ok(true)
            }
            Some(EraseWait::Error) => {
                tracing::error!("{prefix}Erase failed with ERROR status");
                Ok(false)
            }
            Some(EraseWait::Busy) => unreachable!("busy_is_wait suppresses BUSY"),
            None => {
                tracing::error!("{prefix}Timeout waiting for erase to complete");
                Ok(false)
            }
        }
    }

    /// Confirm the device is accessible and unlocked after recovery. Fails if
    /// the device still reports itself locked; a warning (not an error) is
    /// issued when the status cannot be read at all.
    fn verify_recovery(&self, io: &mut dyn DapIo) -> Result<()> {
        let ap = self.ctrl_ap()?.num;
        tracing::info!("Verifying device accessibility...");

        if let Some(idr) = try_read_ap(io, ap, CTRL_AP_IDR) {
            tracing::info!("Post-erase CTRL-AP IDR: 0x{idr:08X}");
        }

        let Some(protect) = try_read_ap(io, ap, CTRL_AP_ERASEPROTECTSTATUS) else {
            tracing::warn!("Could not read ERASEPROTECTSTATUS; unable to confirm unlock");
            return Ok(());
        };

        tracing::info!("ERASEPROTECTSTATUS: {protect}");
        if protect >= 1 {
            tracing::info!("Device is unlocked");
            return Ok(());
        }

        tracing::warn!("Device may still be locked; retrying...");
        sleep(self.timing.settle_reset);
        if let Err(err) = io.reinitialize() {
            tracing::warn!("Reinitialize during unlock retry failed: {err}");
        }
        sleep(self.timing.reconnect);
        match try_read_ap(io, ap, CTRL_AP_ERASEPROTECTSTATUS) {
            Some(v) if v >= 1 => {
                tracing::info!("Device is now unlocked after retry");
                Ok(())
            }
            _ => bail!("Device still appears locked after mass erase"),
        }
    }
}

impl PlatformHandler for NordicHandler {
    /// Unlock the device with a CTRL-AP mass erase (with one reinit+retry
    /// fallback), then reset and confirm the device is accessible.
    fn recover(&self, io: &mut dyn DapIo, progress: &mut ProgressFn) -> Result<()> {
        let ap = self.ctrl_ap()?.num;
        let expected_idr = self.ctrl_ap()?.idr;

        tracing::info!("Initializing DAP connection for recovery...");
        match try_read_ap(io, ap, CTRL_AP_IDR) {
            Some(idr) => {
                tracing::info!("CTRL-AP IDR: 0x{idr:08X}");
                if idr != expected_idr {
                    tracing::warn!("Unexpected CTRL-AP IDR (expected 0x{expected_idr:08X})");
                }
            }
            None => tracing::warn!("Could not read CTRL-AP IDR; attempting mass erase anyway"),
        }

        let mut erased = self.attempt_erase_all(io, progress, false)?;
        if !erased {
            tracing::warn!("Mass erase failed; attempting fallback (reinit + retry)...");
            io.reinitialize()?;
            sleep(self.timing.reconnect);
            erased = self.attempt_erase_all(io, progress, true)?;
            if !erased {
                bail!("Both mass erase and fallback erase failed");
            }
        }

        progress(80.0);

        // Reset the device after erase.
        sleep(self.timing.settle_short);
        tracing::info!("Resetting device...");
        io.write_ap(ap, CTRL_AP_RESET, 2)?;
        sleep(self.timing.settle_short);
        io.write_ap(ap, CTRL_AP_RESET, 0)?;
        io.write_ap(ap, CTRL_AP_ERASEALL, 0)?;

        tracing::info!("Waiting for device to stabilize...");
        sleep(self.timing.settle_reset);
        progress(85.0);

        tracing::info!("Reconnecting to verify recovery...");
        if let Err(err) = io.reinitialize() {
            tracing::warn!("Reconnect warning: {err}");
        }
        sleep(self.timing.reconnect);
        progress(90.0);

        self.verify_recovery(io)?;
        progress(100.0);
        tracing::info!("Mass erase completed successfully!");
        Ok(())
    }

    fn flash(
        &self,
        io: &mut dyn DapIo,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<()> {
        self.flash_impl(io, firmware, progress)
    }

    fn verify(
        &self,
        io: &mut dyn DapIo,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<VerifyOutcome> {
        self.verify_impl(io, firmware, progress)
    }

    /// Pulse the CTRL-AP RESET register to reset the device.
    fn reset(&self, io: &mut dyn DapIo) -> Result<()> {
        let ap = self.ctrl_ap()?.num;
        tracing::info!("Resetting device via CTRL-AP...");

        io.write_ap(ap, CTRL_AP_RESET, 2)?;
        sleep(self.timing.settle_short);
        io.write_ap(ap, CTRL_AP_RESET, 0)?;
        sleep(self.timing.post_reset);
        tracing::info!("Device reset completed");
        Ok(())
    }
}
