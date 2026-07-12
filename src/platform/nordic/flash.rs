// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Nordic flash programming and verification.
//!
//! Handles flash controller enablement (RRAMC on nRF54, NVMC on nRF52),
//! streaming word writes and byte-accurate read-back verification.

use anyhow::{anyhow, bail, Context, Result};

use super::{poll_until, NordicHandler};
use crate::dap::io::DapIo;
use crate::hex::Firmware;
use crate::platform::{ProgressFn, VerifyOutcome};
use crate::targets::definition::{FlashController, FlashControllerKind};

/// Word chunk size for streaming flash writes / verify reads (4 KiB).
const WORD_CHUNK: usize = 1024;

// NVMC (nRF52): CONFIG at base+0x504 (1 = write enable), READY at base+0x400.
const NVMC_CONFIG_OFFSET: u64 = 0x504;
const NVMC_READY_OFFSET: u64 = 0x400;
const NVMC_CONFIG_WEN: u32 = 1;

impl NordicHandler {
    /// Enable the flash controller for programming based on its declared type.
    fn init_flash_controller(&self, io: &mut dyn DapIo) -> Result<()> {
        let fc = self.flash_controller()?.clone();
        match fc.kind {
            FlashControllerKind::Rramc => self.init_rramc(io, &fc),
            FlashControllerKind::Nvmc => self.init_nvmc(io, &fc),
            FlashControllerKind::Unknown => bail!(
                "Unsupported flash controller type for target '{}'",
                self.cfg.id
            ),
        }
    }

    /// Enable RRAMC write mode (nRF54) and wait until it reports ready.
    fn init_rramc(&self, io: &mut dyn DapIo, fc: &FlashController) -> Result<()> {
        let regs = fc
            .registers
            .as_ref()
            .ok_or_else(|| anyhow!("RRAMC requires a registers definition"))?;
        let config_addr = fc.base + regs.config.offset;
        let config_value = regs.config.enable_value;
        let ready_addr = fc.base + regs.ready.offset;

        tracing::info!("Configuring RRAMC for flash programming...");
        if let Ok(current) = io.read_word_32(config_addr) {
            tracing::info!("Current RRAMC CONFIG: 0x{current:08X}");
        }
        io.write_word_32(config_addr, config_value)
            .context("Failed to write RRAMC CONFIG")?;

        match io.read_word_32(config_addr) {
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

        self.wait_flash_ready(io, ready_addr, "RRAMC")
    }

    /// Enable NVMC write mode (nRF52) and wait until it reports ready.
    fn init_nvmc(&self, io: &mut dyn DapIo, fc: &FlashController) -> Result<()> {
        let config_addr = fc.base + NVMC_CONFIG_OFFSET;
        let ready_addr = fc.base + NVMC_READY_OFFSET;

        tracing::info!("Configuring NVMC for flash programming...");
        io.write_word_32(config_addr, NVMC_CONFIG_WEN)
            .context("Failed to write NVMC CONFIG")?;

        self.wait_flash_ready(io, ready_addr, "NVMC")
    }

    /// Poll a flash controller READY register until bit 0 is set, failing on
    /// timeout so programming never proceeds against a wedged controller.
    fn wait_flash_ready(&self, io: &mut dyn DapIo, ready_addr: u64, name: &str) -> Result<()> {
        let ready = poll_until(
            self.timing.ready_timeout,
            self.timing.ready_interval,
            |_| {},
            || match io.read_word_32(ready_addr) {
                Ok(v) if v & 0x1 != 0 => Some(()),
                _ => None,
            },
        );
        match ready {
            Some(()) => {
                tracing::info!("{name} is ready for programming");
                Ok(())
            }
            None => bail!("{name} did not report ready within the timeout"),
        }
    }

    /// Enable the flash controller, then stream the image to flash as
    /// little-endian 32-bit words, finishing with a read-back that flushes the
    /// controller's write buffer so the final word is committed.
    pub(super) fn flash_impl(
        &self,
        io: &mut dyn DapIo,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<()> {
        tracing::info!(
            "Flashing {} bytes starting at 0x{:08X}...",
            firmware.data.len(),
            firmware.start_address
        );

        self.init_flash_controller(io)?;

        // Convert chunk-by-chunk to little-endian words, padding the final
        // partial word with 0xFF (the erased value), avoiding a full copy of
        // the image.
        let total_words = firmware.data.len().div_ceil(4);
        tracing::info!("Writing {total_words} words...");

        let mut words = Vec::with_capacity(WORD_CHUNK);
        let mut written = 0usize;
        for chunk in firmware.data.chunks(WORD_CHUNK * 4) {
            words.clear();
            for bytes in chunk.chunks(4) {
                let mut word = [0xFFu8; 4];
                word[..bytes.len()].copy_from_slice(bytes);
                words.push(u32::from_le_bytes(word));
            }

            let addr = firmware.start_address + (written as u64) * 4;
            io.write_32(addr, &words)
                .with_context(|| format!("Flash write failed at 0x{addr:08X}"))?;
            written += words.len();
            progress((written as f64 / total_words as f64) * 100.0);
        }

        // Flush the flash controller's write buffer. The nRF54L RRAM holds the
        // most recently written word(s) in a small buffer and only commits them
        // to non-volatile storage on a coherency event; a MEM-AP read of the
        // RRAM forces that commit, whereas the peripheral-register writes used to
        // program it do not. Reading the tail of the freshly written region back
        // guarantees the final word is committed before the device is reset.
        // Without it the last word is silently lost (a `--verify` pass hides the
        // bug because reading the whole image back performs the same flush).
        let flush_words = total_words.min(WORD_CHUNK);
        if flush_words > 0 {
            let tail_addr = firmware.start_address + ((total_words - flush_words) as u64) * 4;
            let mut scratch = vec![0u32; flush_words];
            if let Err(err) = io.read_32(tail_addr, &mut scratch) {
                tracing::warn!("Flash flush read-back failed at 0x{tail_addr:08X}: {err}");
            }
        }

        tracing::info!("Firmware write completed!");
        Ok(())
    }

    /// Read the whole image back from flash and count byte mismatches against
    /// the expected firmware.
    pub(super) fn verify_impl(
        &self,
        io: &mut dyn DapIo,
        firmware: &Firmware,
        progress: &mut ProgressFn,
    ) -> Result<VerifyOutcome> {
        tracing::info!("Verifying firmware (reading back entire image)...");

        let size = firmware.data.len();
        let total_words = size.div_ceil(4);
        let mut mismatches = 0usize;

        let mut buf = vec![0u32; WORD_CHUNK];
        let mut word_idx = 0usize;
        while word_idx < total_words {
            let n = WORD_CHUNK.min(total_words - word_idx);
            let addr = firmware.start_address + (word_idx as u64) * 4;
            io.read_32(addr, &mut buf[..n])
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
        } else {
            tracing::info!("Verification passed: all {size} bytes match");
        }
        Ok(VerifyOutcome {
            success: mismatches == 0,
            mismatches,
        })
    }
}
