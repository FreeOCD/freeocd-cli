// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Nordic Semiconductor platform handler.
//!
//! Implements CTRL-AP mass-erase recovery, RRAMC/NVMC flash programming,
//! verification and reset for Nordic nRF series microcontrollers, driven by the
//! shared target definition JSON. The low-level transfers are performed through
//! probe-rs's `ArmDebugInterface` (CTRL-AP via `DapAccess`, flash/verify via the
//! MEM-AP `MemoryInterface`).

use std::{thread::sleep, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use probe_rs::architecture::arm::ArmDebugInterface;

use super::{PlatformHandler, ProgressFn, VerifyOutcome};
use crate::dap::arm as dap_arm;
use crate::hex::Firmware;
use crate::targets::definition::{CtrlAp, EraseAllStatus, FlashController, TargetConfig};

// CTRL-AP register offsets (common across the Nordic nRF series).
const CTRL_AP_RESET: u64 = 0x000;
const CTRL_AP_ERASEALL: u64 = 0x004;
const CTRL_AP_ERASEALLSTATUS: u64 = 0x008;
const CTRL_AP_ERASEPROTECTSTATUS: u64 = 0x00C;
const CTRL_AP_IDR: u64 = 0x0FC;

// Number of 100 ms polling iterations while waiting for ERASEALL transitions.
const ERASE_TIMEOUT: usize = 300;
// Word chunk size for streaming flash writes / verify reads.
const WORD_CHUNK: usize = 256;

/// Nordic platform handler.
pub struct NordicHandler {
    cfg: TargetConfig,
}

impl NordicHandler {
    /// Create a handler bound to a parsed target definition.
    pub fn new(cfg: TargetConfig) -> Self {
        Self { cfg }
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

    /// Trigger ERASEALL and wait for completion. Returns `false` (rather than an
    /// error) on a recoverable failure so the caller can retry.
    fn attempt_erase_all(
        &self,
        iface: &mut dyn ArmDebugInterface,
        progress: &mut ProgressFn,
        is_retry: bool,
    ) -> Result<bool> {
        let ap = self.ctrl_ap()?.num;
        let status = self.erase_status()?.clone();
        let prefix = if is_retry { "[Retry] " } else { "" };

        tracing::info!("{prefix}Resetting ERASEALL task...");
        dap_arm::write_ap(iface, ap, CTRL_AP_ERASEALL, 0)?;
        sleep(Duration::from_millis(10));

        tracing::info!("{prefix}Triggering mass erase (ERASEALL)...");
        dap_arm::write_ap(iface, ap, CTRL_AP_ERASEALL, 1)?;

        // Phase 1: wait for the BUSY state to appear.
        tracing::info!("{prefix}Waiting for erase to start...");
        let mut last: Option<u32> = None;
        for i in 0..ERASE_TIMEOUT {
            last = dap_arm::try_read_ap(iface, ap, CTRL_AP_ERASEALLSTATUS);
            match last {
                Some(v) if v == status.busy => {
                    tracing::info!("{prefix}Erase in progress (BUSY)...");
                    break;
                }
                Some(v) if v == status.error => {
                    tracing::error!("{prefix}Erase failed with ERROR status");
                    return Ok(false);
                }
                Some(v) if v == status.ready_to_reset => {
                    tracing::info!("{prefix}Device already erased (READYTORESET)");
                    return Ok(true);
                }
                _ => {
                    sleep(Duration::from_millis(100));
                    progress((i as f64 / ERASE_TIMEOUT as f64) * 30.0);
                }
            }
        }

        match last {
            Some(v) if v == status.busy => {}
            Some(v) if v == status.ready_to_reset => return Ok(true),
            _ => {
                tracing::error!("{prefix}Timeout waiting for erase to start");
                return Ok(false);
            }
        }

        // Phase 2: wait for READYTORESET.
        tracing::info!("{prefix}Waiting for erase to complete...");
        for i in 0..ERASE_TIMEOUT {
            match dap_arm::try_read_ap(iface, ap, CTRL_AP_ERASEALLSTATUS) {
                Some(v) if v == status.ready_to_reset => {
                    tracing::info!("{prefix}Erase completed successfully (READYTORESET)");
                    return Ok(true);
                }
                Some(v) if v == status.error => {
                    tracing::error!("{prefix}Erase failed with ERROR status");
                    return Ok(false);
                }
                _ => {
                    sleep(Duration::from_millis(100));
                    progress(30.0 + (i as f64 / ERASE_TIMEOUT as f64) * 50.0);
                }
            }
        }

        tracing::error!("{prefix}Timeout waiting for erase to complete");
        Ok(false)
    }

    /// Confirm the device is accessible and unlocked after recovery.
    fn verify_recovery(&self, iface: &mut dyn ArmDebugInterface) -> Result<()> {
        let ap = self.ctrl_ap()?.num;
        tracing::info!("Verifying device accessibility...");

        if let Some(idr) = dap_arm::try_read_ap(iface, ap, CTRL_AP_IDR) {
            tracing::info!("Post-erase CTRL-AP IDR: 0x{idr:08X}");
        }

        if let Some(protect) = dap_arm::try_read_ap(iface, ap, CTRL_AP_ERASEPROTECTSTATUS) {
            tracing::info!("ERASEPROTECTSTATUS: {protect}");
            if protect >= 1 {
                tracing::info!("Device is unlocked");
            } else {
                tracing::warn!("Device may still be locked; retrying...");
                sleep(Duration::from_millis(500));
                let _ = iface.reinitialize();
                sleep(Duration::from_millis(200));
                match dap_arm::try_read_ap(iface, ap, CTRL_AP_ERASEPROTECTSTATUS) {
                    Some(v) if v >= 1 => tracing::info!("Device is now unlocked after retry"),
                    _ => tracing::warn!("Device still appears locked after retry"),
                }
            }
        }
        Ok(())
    }

    /// Enable the flash controller for programming based on its declared type.
    fn init_flash_controller(&self, iface: &mut dyn ArmDebugInterface) -> Result<()> {
        let fc = self.flash_controller()?.clone();
        match fc.kind.as_str() {
            "rramc" => self.init_rramc(iface, &fc),
            "nvmc" => self.init_nvmc(iface, &fc),
            other => {
                tracing::warn!("Unknown flash controller type: {other}");
                Ok(())
            }
        }
    }

    /// Enable RRAMC write mode (nRF54) and wait until it reports ready.
    fn init_rramc(&self, iface: &mut dyn ArmDebugInterface, fc: &FlashController) -> Result<()> {
        let regs = fc
            .registers
            .as_ref()
            .ok_or_else(|| anyhow!("RRAMC requires a registers definition"))?;
        let config_addr = fc.base + regs.config.offset;
        let config_value = regs.config.enable_value;
        let ready_addr = fc.base + regs.ready.offset;

        tracing::info!("Configuring RRAMC for flash programming...");
        let mut mem = iface.memory_interface(&dap_arm::mem_ap())?;

        if let Ok(current) = mem.read_word_32(config_addr) {
            tracing::info!("Current RRAMC CONFIG: 0x{current:08X}");
        }
        mem.write_word_32(config_addr, config_value)
            .context("Failed to write RRAMC CONFIG")?;

        match mem.read_word_32(config_addr) {
            Ok(new) => {
                tracing::info!("New RRAMC CONFIG: 0x{new:08X}");
                if new & 0x1 != 1 {
                    tracing::warn!("RRAMC WEN bit not set");
                } else {
                    tracing::info!("RRAMC write mode enabled");
                }
            }
            Err(err) => tracing::warn!("Could not read back RRAMC CONFIG: {err}"),
        }

        for retries in 0.. {
            let ready = mem.read_word_32(ready_addr).unwrap_or(0);
            if ready & 0x1 != 0 {
                tracing::info!("RRAMC is ready for programming");
                break;
            }
            if retries >= 100 {
                tracing::warn!("RRAMC not ready after timeout");
                break;
            }
            sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    /// Enable NVMC write mode (nRF52) and wait until it reports ready.
    fn init_nvmc(&self, iface: &mut dyn ArmDebugInterface, fc: &FlashController) -> Result<()> {
        // NVMC (nRF52): CONFIG at base+0x504 (1 = write enable), READY at base+0x400.
        const NVMC_CONFIG_OFFSET: u64 = 0x504;
        const NVMC_READY_OFFSET: u64 = 0x400;
        const NVMC_CONFIG_WEN: u32 = 1;

        let config_addr = fc.base + NVMC_CONFIG_OFFSET;
        let ready_addr = fc.base + NVMC_READY_OFFSET;

        tracing::info!("Configuring NVMC for flash programming...");
        let mut mem = iface.memory_interface(&dap_arm::mem_ap())?;
        mem.write_word_32(config_addr, NVMC_CONFIG_WEN)
            .context("Failed to write NVMC CONFIG")?;

        for retries in 0.. {
            let ready = mem.read_word_32(ready_addr).unwrap_or(0);
            if ready & 0x1 == 1 {
                tracing::info!("NVMC write mode enabled");
                break;
            }
            if retries >= 100 {
                tracing::warn!("NVMC not ready after timeout");
                break;
            }
            sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

impl PlatformHandler for NordicHandler {
    /// Unlock the device with a CTRL-AP mass erase (with one reinit+retry
    /// fallback), then reset and confirm the device is accessible.
    fn recover(&self, iface: &mut dyn ArmDebugInterface, progress: &mut ProgressFn) -> Result<()> {
        let ap = self.ctrl_ap()?.num;
        let expected_idr = self.ctrl_ap()?.idr;

        tracing::info!("Initializing DAP connection for recovery...");
        match dap_arm::try_read_ap(iface, ap, CTRL_AP_IDR) {
            Some(idr) => {
                tracing::info!("CTRL-AP IDR: 0x{idr:08X}");
                if idr != expected_idr {
                    tracing::warn!("Unexpected CTRL-AP IDR (expected 0x{expected_idr:08X})");
                }
            }
            None => tracing::warn!("Could not read CTRL-AP IDR; attempting mass erase anyway"),
        }

        let mut erased = self.attempt_erase_all(iface, progress, false)?;
        if !erased {
            tracing::warn!("Mass erase failed; attempting fallback (reinit + retry)...");
            iface
                .reinitialize()
                .context("Reinitialize before fallback erase failed")?;
            sleep(Duration::from_millis(200));
            erased = self.attempt_erase_all(iface, progress, true)?;
            if !erased {
                bail!("Both mass erase and fallback erase failed");
            }
        }

        progress(80.0);

        // Reset the device after erase.
        sleep(Duration::from_millis(10));
        tracing::info!("Resetting device...");
        dap_arm::write_ap(iface, ap, CTRL_AP_RESET, 2)?;
        sleep(Duration::from_millis(10));
        dap_arm::write_ap(iface, ap, CTRL_AP_RESET, 0)?;
        dap_arm::write_ap(iface, ap, CTRL_AP_ERASEALL, 0)?;

        tracing::info!("Waiting for device to stabilize...");
        sleep(Duration::from_millis(500));
        progress(85.0);

        tracing::info!("Reconnecting to verify recovery...");
        if let Err(err) = iface.reinitialize() {
            tracing::warn!("Reconnect warning: {err}");
        }
        sleep(Duration::from_millis(200));
        progress(90.0);

        self.verify_recovery(iface)?;
        progress(100.0);
        tracing::info!("Mass erase completed successfully!");
        Ok(())
    }

    /// Enable the flash controller, then stream the image to flash as
    /// little-endian 32-bit words, finishing with a read-back that flushes the
    /// controller's write buffer so the final word is committed.
    fn flash(
        &self,
        iface: &mut dyn ArmDebugInterface,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<()> {
        tracing::info!(
            "Flashing {} bytes starting at 0x{:08X}...",
            firmware.data.len(),
            firmware.start_address
        );

        // Enable the flash controller (its own MEM-AP borrow is dropped here).
        self.init_flash_controller(iface)?;

        // Pad to a 32-bit word boundary and convert to little-endian words.
        let mut padded = firmware.data.clone();
        while !padded.len().is_multiple_of(4) {
            padded.push(0xFF);
        }
        let words: Vec<u32> = padded
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let total = words.len();

        tracing::info!("Writing {total} words...");
        let mut mem = iface.memory_interface(&dap_arm::mem_ap())?;
        let mut written = 0usize;
        for chunk in words.chunks(WORD_CHUNK) {
            let addr = firmware.start_address + (written as u64) * 4;
            mem.write_32(addr, chunk)
                .with_context(|| format!("Flash write failed at 0x{addr:08X}"))?;
            written += chunk.len();
            progress((written as f64 / total as f64) * 100.0);
        }

        // Flush the flash controller's write buffer. The nRF54L RRAM holds the
        // most recently written word(s) in a small buffer and only commits them
        // to non-volatile storage on a coherency event; a MEM-AP read of the
        // RRAM forces that commit, whereas the peripheral-register writes used to
        // program it do not. Reading the tail of the freshly written region back
        // guarantees the final word is committed before the device is reset.
        // Without it the last word is silently lost (a `--verify` pass hides the
        // bug because reading the whole image back performs the same flush).
        let flush_words = total.min(WORD_CHUNK);
        if flush_words > 0 {
            let tail_addr = firmware.start_address + ((total - flush_words) as u64) * 4;
            let mut scratch = vec![0u32; flush_words];
            if let Err(err) = mem.read_32(tail_addr, &mut scratch) {
                tracing::warn!("Flash flush read-back failed at 0x{tail_addr:08X}: {err}");
            }
        }

        tracing::info!("Firmware write completed!");
        Ok(())
    }

    /// Read the whole image back from flash and count byte mismatches against
    /// the expected firmware.
    fn verify(
        &self,
        iface: &mut dyn ArmDebugInterface,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<VerifyOutcome> {
        tracing::info!("Verifying firmware (reading back entire image)...");

        let size = firmware.data.len();
        let total_words = size.div_ceil(4);
        let mut mismatches = 0usize;

        let mut mem = iface.memory_interface(&dap_arm::mem_ap())?;
        let mut buf = vec![0u32; WORD_CHUNK];
        let mut word_idx = 0usize;
        while word_idx < total_words {
            let n = WORD_CHUNK.min(total_words - word_idx);
            let addr = firmware.start_address + (word_idx as u64) * 4;
            mem.read_32(addr, &mut buf[..n])
                .with_context(|| format!("Verify read failed at 0x{addr:08X}"))?;

            for (w, word) in buf[..n].iter().enumerate() {
                let bytes = word.to_le_bytes();
                for (b, &actual) in bytes.iter().enumerate() {
                    let byte_idx = (word_idx + w) * 4 + b;
                    if byte_idx >= size {
                        break;
                    }
                    let expected = firmware.data[byte_idx];
                    if actual != expected {
                        mismatches += 1;
                        if mismatches <= 5 {
                            tracing::warn!(
                                "Verify mismatch at 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                                firmware.start_address + byte_idx as u64,
                                expected,
                                actual
                            );
                        }
                    }
                }
            }

            word_idx += n;
            progress((word_idx as f64 / total_words as f64) * 100.0);
        }

        if mismatches > 0 {
            tracing::error!("Verification failed: {mismatches} byte mismatches in {size} bytes");
            Ok(VerifyOutcome {
                success: false,
                mismatches,
            })
        } else {
            tracing::info!("Verification passed: all {size} bytes match");
            Ok(VerifyOutcome {
                success: true,
                mismatches: 0,
            })
        }
    }

    /// Pulse the CTRL-AP RESET register to reset the device (best effort).
    fn reset(&self, iface: &mut dyn ArmDebugInterface) -> Result<()> {
        let ap = self.ctrl_ap()?.num;
        tracing::info!("Resetting device via CTRL-AP...");

        if let Err(err) = dap_arm::write_ap(iface, ap, CTRL_AP_RESET, 2) {
            tracing::warn!("CTRL-AP reset error: {err}");
            return Ok(());
        }
        sleep(Duration::from_millis(10));
        if let Err(err) = dap_arm::write_ap(iface, ap, CTRL_AP_RESET, 0) {
            tracing::warn!("CTRL-AP reset error: {err}");
            return Ok(());
        }
        sleep(Duration::from_millis(100));
        tracing::info!("Device reset completed");
        Ok(())
    }
}
