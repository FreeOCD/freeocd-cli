// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Low-level ARM Debug Access Port helpers on top of probe-rs.
//!
//! Provides retrying Access Port register read/write wrappers (used for Nordic
//! CTRL-AP access) and the default MEM-AP address, delegating the actual
//! transfers to probe-rs's `DapAccess` implementation.

use std::{thread::sleep, time::Duration};

use probe_rs::architecture::arm::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};

/// Number of attempts for a single AP register transfer.
const RETRY_COUNT: usize = 3;
/// Delay between retry attempts.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// The default MEM-AP (AP #0) address on the default debug port.
pub fn mem_ap() -> FullyQualifiedApAddress {
    FullyQualifiedApAddress::v1_with_default_dp(0)
}

/// Read an Access Port register, retrying on transient transfer errors.
pub fn read_ap(iface: &mut dyn ArmDebugInterface, ap: u8, reg: u64) -> Result<u32, ArmError> {
    let addr = FullyQualifiedApAddress::v1_with_default_dp(ap);
    let mut last_err = None;
    for attempt in 0..RETRY_COUNT {
        match iface.read_raw_ap_register(&addr, reg) {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_err = Some(err);
                if attempt + 1 < RETRY_COUNT {
                    sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(last_err.expect("RETRY_COUNT is non-zero"))
}

/// Write an Access Port register, retrying on transient transfer errors.
pub fn write_ap(
    iface: &mut dyn ArmDebugInterface,
    ap: u8,
    reg: u64,
    value: u32,
) -> Result<(), ArmError> {
    let addr = FullyQualifiedApAddress::v1_with_default_dp(ap);
    let mut last_err = None;
    for attempt in 0..RETRY_COUNT {
        match iface.write_raw_ap_register(&addr, reg, value) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_err = Some(err);
                if attempt + 1 < RETRY_COUNT {
                    sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(last_err.expect("RETRY_COUNT is non-zero"))
}

/// Best-effort AP register read returning `None` on failure (used for polling
/// loops where a missing value should be retried rather than treated fatally).
pub fn try_read_ap(iface: &mut dyn ArmDebugInterface, ap: u8, reg: u64) -> Option<u32> {
    read_ap(iface, ap, reg).ok()
}
