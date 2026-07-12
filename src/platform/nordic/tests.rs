// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Mock-based unit tests for the Nordic handler's recover / flash / verify /
//! reset state machines.

use std::collections::{HashMap, VecDeque};

use anyhow::{anyhow, Result};

use super::{
    NordicHandler, CTRL_AP_ERASEALL, CTRL_AP_ERASEALLSTATUS, CTRL_AP_ERASEPROTECTSTATUS,
    CTRL_AP_IDR, CTRL_AP_RESET,
};
use crate::dap::io::DapIo;
use crate::hex::Firmware;
use crate::platform::PlatformHandler;
use crate::targets::definition::TargetConfig;

/// ERASEALLSTATUS codes used by the test target (matching nrf54l15.json).
const STATUS_BUSY: u32 = 2;
const STATUS_READY_TO_RESET: u32 = 1;
const STATUS_ERROR: u32 = 3;

const AP: u8 = 2;
const IDR: u32 = 0x32880000;
const RRAMC_BASE: u64 = 0x5004B000;
const RRAMC_CONFIG: u64 = RRAMC_BASE + 0x500;
const RRAMC_READY: u64 = RRAMC_BASE + 0x408;

/// Scriptable in-memory [`DapIo`] implementation.
#[derive(Default)]
struct MockIo {
    /// Scripted AP read values, consumed in order per (ap, reg).
    ap_read_seq: HashMap<(u8, u64), VecDeque<u32>>,
    /// Fallback AP read values once a script is exhausted.
    ap_read_default: HashMap<(u8, u64), u32>,
    /// Log of all AP writes.
    ap_writes: Vec<(u8, u64, u32)>,
    /// Word-addressed target memory.
    mem: HashMap<u64, u32>,
    /// Log of all bulk memory writes.
    mem_writes: Vec<(u64, Vec<u32>)>,
    /// Number of reinitialize() calls.
    reinit_count: usize,
    /// Force all AP writes to fail.
    fail_ap_writes: bool,
}

impl MockIo {
    fn script_ap_read(&mut self, reg: u64, values: &[u32]) {
        self.ap_read_seq
            .insert((AP, reg), values.iter().copied().collect());
    }

    fn default_ap_read(&mut self, reg: u64, value: u32) {
        self.ap_read_default.insert((AP, reg), value);
    }
}

impl DapIo for MockIo {
    fn read_ap(&mut self, ap: u8, reg: u64) -> Result<u32> {
        if let Some(seq) = self.ap_read_seq.get_mut(&(ap, reg)) {
            if let Some(v) = seq.pop_front() {
                return Ok(v);
            }
        }
        self.ap_read_default
            .get(&(ap, reg))
            .copied()
            .ok_or_else(|| anyhow!("unscripted AP read: ap={ap} reg=0x{reg:03X}"))
    }

    fn write_ap(&mut self, ap: u8, reg: u64, value: u32) -> Result<()> {
        if self.fail_ap_writes {
            return Err(anyhow!("scripted AP write failure"));
        }
        self.ap_writes.push((ap, reg, value));
        Ok(())
    }

    fn reinitialize(&mut self) -> Result<()> {
        self.reinit_count += 1;
        Ok(())
    }

    fn read_word_32(&mut self, addr: u64) -> Result<u32> {
        self.mem
            .get(&addr)
            .copied()
            .ok_or_else(|| anyhow!("unmapped memory read at 0x{addr:08X}"))
    }

    fn write_word_32(&mut self, addr: u64, value: u32) -> Result<()> {
        self.mem.insert(addr, value);
        Ok(())
    }

    fn read_32(&mut self, addr: u64, buf: &mut [u32]) -> Result<()> {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = self.read_word_32(addr + (i as u64) * 4)?;
        }
        Ok(())
    }

    fn write_32(&mut self, addr: u64, words: &[u32]) -> Result<()> {
        for (i, &word) in words.iter().enumerate() {
            self.mem.insert(addr + (i as u64) * 4, word);
        }
        self.mem_writes.push((addr, words.to_vec()));
        Ok(())
    }
}

/// An nRF54L15-like target definition.
fn test_target() -> TargetConfig {
    serde_json::from_str(
        r#"{
            "id": "test/nrf54l15",
            "name": "nRF54L15",
            "platform": "nordic",
            "ctrlAp": { "num": "2", "idr": "0x32880000" },
            "eraseAllStatus": { "ready": "0", "readyToReset": "1", "busy": "2", "error": "3" },
            "flashController": {
                "type": "rramc",
                "base": "0x5004B000",
                "registers": {
                    "config": { "offset": "0x500", "enableValue": "0x1" },
                    "ready": { "offset": "0x408" }
                }
            },
            "flash": { "address": "0x0", "size": "0x17D000" },
            "sram": { "address": "0x20000000" },
            "capabilities": ["recover", "flash", "verify", "rtt"]
        }"#,
    )
    .expect("valid test target JSON")
}

fn handler() -> NordicHandler {
    NordicHandler::new_for_test(test_target())
}

fn no_progress() -> impl FnMut(f64) {
    |_| {}
}

/// A clean erase (BUSY then READYTORESET) recovers, resets and verifies unlock.
#[test]
fn recover_succeeds_on_clean_erase() {
    let mut io = MockIo::default();
    io.default_ap_read(CTRL_AP_IDR, IDR);
    io.script_ap_read(
        CTRL_AP_ERASEALLSTATUS,
        &[STATUS_BUSY, STATUS_READY_TO_RESET],
    );
    io.default_ap_read(CTRL_AP_ERASEPROTECTSTATUS, 1);

    let mut progress = no_progress();
    handler()
        .recover(&mut io, &mut progress)
        .expect("recover succeeds");

    // ERASEALL was cleared then triggered, and the device was reset afterwards.
    assert!(io.ap_writes.contains(&(AP, CTRL_AP_ERASEALL, 0)));
    assert!(io.ap_writes.contains(&(AP, CTRL_AP_ERASEALL, 1)));
    assert!(io.ap_writes.contains(&(AP, CTRL_AP_RESET, 2)));
    assert!(io.ap_writes.contains(&(AP, CTRL_AP_RESET, 0)));
    assert!(io.reinit_count >= 1);
}

/// A first erase attempt that reports ERROR triggers a reinitialize and a
/// retry, which can then succeed.
#[test]
fn recover_retries_after_erase_error() {
    let mut io = MockIo::default();
    io.default_ap_read(CTRL_AP_IDR, IDR);
    io.script_ap_read(
        CTRL_AP_ERASEALLSTATUS,
        &[STATUS_ERROR, STATUS_BUSY, STATUS_READY_TO_RESET],
    );
    io.default_ap_read(CTRL_AP_ERASEPROTECTSTATUS, 1);

    let mut progress = no_progress();
    handler()
        .recover(&mut io, &mut progress)
        .expect("retry succeeds");

    // Two full erase attempts: ERASEALL triggered twice.
    let triggers = io
        .ap_writes
        .iter()
        .filter(|w| **w == (AP, CTRL_AP_ERASEALL, 1))
        .count();
    assert_eq!(triggers, 2);
    assert!(io.reinit_count >= 1);
}

/// When both erase attempts fail the operation errors out.
#[test]
fn recover_fails_when_both_attempts_fail() {
    let mut io = MockIo::default();
    io.default_ap_read(CTRL_AP_IDR, IDR);
    io.default_ap_read(CTRL_AP_ERASEALLSTATUS, STATUS_ERROR);
    io.default_ap_read(CTRL_AP_ERASEPROTECTSTATUS, 1);

    let mut progress = no_progress();
    let err = handler()
        .recover(&mut io, &mut progress)
        .expect_err("recover fails");
    assert!(err.to_string().contains("fallback erase failed"));
}

/// A device that still reports itself locked after the erase fails recovery.
#[test]
fn recover_fails_when_still_locked() {
    let mut io = MockIo::default();
    io.default_ap_read(CTRL_AP_IDR, IDR);
    io.script_ap_read(
        CTRL_AP_ERASEALLSTATUS,
        &[STATUS_BUSY, STATUS_READY_TO_RESET],
    );
    io.default_ap_read(CTRL_AP_ERASEPROTECTSTATUS, 0);

    let mut progress = no_progress();
    let err = handler()
        .recover(&mut io, &mut progress)
        .expect_err("locked device fails recovery");
    assert!(err.to_string().contains("locked"));
}

/// Prepare RRAMC memory state: ready flag set, CONFIG initially zero.
fn arm_rramc(io: &mut MockIo) {
    io.mem.insert(RRAMC_READY, 1);
    io.mem.insert(RRAMC_CONFIG, 0);
}

/// Flashing a non-word-multiple image enables the RRAMC, pads the final word
/// with 0xFF and reads the tail back to flush the write buffer.
#[test]
fn flash_pads_final_word_and_flushes() {
    let mut io = MockIo::default();
    arm_rramc(&mut io);

    let firmware = Firmware {
        data: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        start_address: 0x1000,
    };
    let mut progress = no_progress();
    handler()
        .flash(&mut io, &firmware, &mut progress)
        .expect("flash succeeds");

    // RRAMC write mode was enabled.
    assert_eq!(io.mem.get(&RRAMC_CONFIG), Some(&0x1));
    // Words are little-endian with the partial tail padded by 0xFF.
    assert_eq!(io.mem_writes, vec![(0x1000, vec![0x04030201, 0xFFFFFF05])]);
}

/// An RRAMC that never reports ready aborts the flash instead of programming
/// against a wedged controller.
#[test]
fn flash_fails_when_rramc_not_ready() {
    let mut io = MockIo::default();
    io.mem.insert(RRAMC_READY, 0);
    io.mem.insert(RRAMC_CONFIG, 0);

    let firmware = Firmware {
        data: vec![0x01, 0x02, 0x03, 0x04],
        start_address: 0x1000,
    };
    let mut progress = no_progress();
    let err = handler()
        .flash(&mut io, &firmware, &mut progress)
        .expect_err("not-ready controller fails");
    assert!(err.to_string().contains("RRAMC"));
}

/// A target whose flash controller type is unrecognized cannot be flashed.
#[test]
fn flash_fails_on_unknown_controller_kind() {
    let mut cfg = test_target();
    cfg.flash_controller =
        serde_json::from_str(r#"{ "type": "mystery", "base": "0x5004B000" }"#).expect("valid JSON");

    let mut io = MockIo::default();
    let firmware = Firmware {
        data: vec![0x01, 0x02, 0x03, 0x04],
        start_address: 0x1000,
    };
    let mut progress = no_progress();
    let err = NordicHandler::new_for_test(cfg)
        .flash(&mut io, &firmware, &mut progress)
        .expect_err("unknown controller fails");
    assert!(err.to_string().contains("Unsupported flash controller"));
}

/// Verify counts byte-level mismatches and reports success only on a match.
#[test]
fn verify_counts_byte_mismatches() {
    let mut io = MockIo::default();
    // Flash contents: 01 02 03 04 | 05 FF FF FF at 0x1000.
    io.mem.insert(0x1000, 0x04030201);
    io.mem.insert(0x1004, 0xFFFFFF05);

    let matching = Firmware {
        data: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        start_address: 0x1000,
    };
    let mut progress = no_progress();
    let outcome = handler()
        .verify(&mut io, &matching, &mut progress)
        .expect("verify runs");
    assert!(outcome.success);
    assert_eq!(outcome.mismatches, 0);

    let mismatching = Firmware {
        data: vec![0x01, 0xAA, 0x03, 0x04, 0xBB],
        start_address: 0x1000,
    };
    let outcome = handler()
        .verify(&mut io, &mismatching, &mut progress)
        .expect("verify runs");
    assert!(!outcome.success);
    assert_eq!(outcome.mismatches, 2);
}

/// Reset pulses the CTRL-AP RESET register (assert then release).
#[test]
fn reset_pulses_ctrl_ap() {
    let mut io = MockIo::default();
    handler().reset(&mut io).expect("reset succeeds");
    assert_eq!(
        io.ap_writes,
        vec![(AP, CTRL_AP_RESET, 2), (AP, CTRL_AP_RESET, 0)]
    );
}

/// A failed CTRL-AP write now propagates instead of being reported as success.
#[test]
fn reset_propagates_write_failure() {
    let mut io = MockIo {
        fail_ap_writes: true,
        ..MockIo::default()
    };
    assert!(handler().reset(&mut io).is_err());
}
