// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Narrow debug I/O abstraction used by the platform handlers.
//!
//! [`DapIo`] captures the handful of operations a platform handler needs
//! (Access Port register access, MEM-AP word transfers and a debug-port
//! reinitialize), decoupling the handlers from probe-rs's full
//! `ArmDebugInterface`. This keeps the transport swappable and lets the
//! handler logic be unit-tested against a mock implementation.

use anyhow::{anyhow, Result};
use probe_rs::architecture::arm::{
    memory::ArmMemoryInterface, ArmDebugInterface, FullyQualifiedApAddress,
};

use super::arm;

/// Debug I/O operations required by platform handlers.
pub trait DapIo {
    /// Read an Access Port register.
    fn read_ap(&mut self, ap: u8, reg: u64) -> Result<u32>;
    /// Write an Access Port register.
    fn write_ap(&mut self, ap: u8, reg: u64, value: u32) -> Result<()>;
    /// Re-establish the debug port connection (e.g. after a device reset).
    fn reinitialize(&mut self) -> Result<()>;
    /// Read a single 32-bit word from target memory via the MEM-AP.
    fn read_word_32(&mut self, addr: u64) -> Result<u32>;
    /// Write a single 32-bit word to target memory via the MEM-AP.
    fn write_word_32(&mut self, addr: u64, value: u32) -> Result<()>;
    /// Read a block of 32-bit words from target memory via the MEM-AP.
    fn read_32(&mut self, addr: u64, buf: &mut [u32]) -> Result<()>;
    /// Write a block of 32-bit words to target memory via the MEM-AP.
    fn write_32(&mut self, addr: u64, words: &[u32]) -> Result<()>;
}

/// Best-effort AP register read returning `None` on failure (used in polling
/// loops where a missing value should be retried rather than treated fatally).
pub fn try_read_ap(io: &mut dyn DapIo, ap: u8, reg: u64) -> Option<u32> {
    io.read_ap(ap, reg).ok()
}

/// [`DapIo`] implementation backed by a probe-rs `ArmDebugInterface`.
pub struct ProbeRsIo {
    iface: Box<dyn ArmDebugInterface>,
}

impl ProbeRsIo {
    /// Wrap an initialized ARM debug interface.
    pub fn new(iface: Box<dyn ArmDebugInterface>) -> Self {
        Self { iface }
    }

    /// Borrow the default MEM-AP memory interface (used by RTT).
    pub fn memory(&mut self) -> Result<Box<dyn ArmMemoryInterface + '_>> {
        Ok(self.iface.memory_interface(&mem_ap())?)
    }
}

/// The default MEM-AP (AP #0) address on the default debug port.
fn mem_ap() -> FullyQualifiedApAddress {
    FullyQualifiedApAddress::v1_with_default_dp(0)
}

impl DapIo for ProbeRsIo {
    fn read_ap(&mut self, ap: u8, reg: u64) -> Result<u32> {
        Ok(arm::read_ap(self.iface.as_mut(), ap, reg)?)
    }

    fn write_ap(&mut self, ap: u8, reg: u64, value: u32) -> Result<()> {
        Ok(arm::write_ap(self.iface.as_mut(), ap, reg, value)?)
    }

    fn reinitialize(&mut self) -> Result<()> {
        self.iface
            .reinitialize()
            .map_err(|err| anyhow!("Failed to reinitialize the debug port: {err}"))
    }

    fn read_word_32(&mut self, addr: u64) -> Result<u32> {
        Ok(self.memory()?.read_word_32(addr)?)
    }

    fn write_word_32(&mut self, addr: u64, value: u32) -> Result<()> {
        Ok(self.memory()?.write_word_32(addr, value)?)
    }

    fn read_32(&mut self, addr: u64, buf: &mut [u32]) -> Result<()> {
        Ok(self.memory()?.read_32(addr, buf)?)
    }

    fn write_32(&mut self, addr: u64, words: &[u32]) -> Result<()> {
        Ok(self.memory()?.write_32(addr, words)?)
    }
}
