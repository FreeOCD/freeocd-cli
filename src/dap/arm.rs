// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Low-level ARM Debug Access Port helpers on top of probe-rs.
//!
//! Provides retrying Access Port register read/write wrappers (used for Nordic
//! CTRL-AP access), delegating the actual transfers to probe-rs's `DapAccess`
//! implementation. Only transient transfer errors are retried; deterministic
//! failures (bad address, wrong AP type, ...) surface immediately.

use std::{thread::sleep, time::Duration};

use probe_rs::architecture::arm::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};

/// Number of attempts for a single AP register transfer.
const RETRY_COUNT: usize = 3;
/// Delay between retry attempts.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// Whether an [`ArmError`] is plausibly transient (a flaky wire transfer that
/// may succeed on retry) as opposed to a deterministic failure that retrying
/// cannot fix.
fn is_transient(err: &ArmError) -> bool {
    !matches!(
        err,
        ArmError::ArchitectureRequired(_)
            | ArmError::AddressOutOf32BitAddressSpace
            | ArmError::NoArmTarget
            | ArmError::ReAttachRequired
            | ArmError::MissingPermissions(_)
            | ArmError::MemoryNotAligned(_)
            | ArmError::OutOfBounds
            | ArmError::UnsupportedTransferWidth(_)
            | ArmError::ApDoesNotExist(_)
            | ArmError::WrongApVersion
            | ArmError::WrongApType
    )
}

/// Run an AP transfer with retries on transient errors.
fn with_retry<T>(mut op: impl FnMut() -> Result<T, ArmError>) -> Result<T, ArmError> {
    let mut last_err = None;
    for attempt in 0..RETRY_COUNT {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !is_transient(&err) {
                    return Err(err);
                }
                tracing::debug!(
                    "Transient AP transfer error (attempt {}): {err}",
                    attempt + 1
                );
                last_err = Some(err);
                if attempt + 1 < RETRY_COUNT {
                    sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(last_err.expect("RETRY_COUNT is non-zero"))
}

/// Read an Access Port register, retrying on transient transfer errors.
pub fn read_ap(iface: &mut dyn ArmDebugInterface, ap: u8, reg: u64) -> Result<u32, ArmError> {
    let addr = FullyQualifiedApAddress::v1_with_default_dp(ap);
    with_retry(|| iface.read_raw_ap_register(&addr, reg))
}

/// Write an Access Port register, retrying on transient transfer errors.
pub fn write_ap(
    iface: &mut dyn ArmDebugInterface,
    ap: u8,
    reg: u64,
    value: u32,
) -> Result<(), ArmError> {
    let addr = FullyQualifiedApAddress::v1_with_default_dp(ap);
    with_retry(|| iface.write_raw_ap_register(&addr, reg, value))
}
