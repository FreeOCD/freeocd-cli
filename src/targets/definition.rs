// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Serde data model for the shared target definition JSON.
//!
//! The schema mirrors `public/targets/<platform>/<family>/<mcu>.json`
//! (e.g. `nordic/nrf54/nrf54l15.json`). Numeric fields are stored as integers;
//! the JSON encodes most addresses/values as hex strings such as
//! `"0x5004B000"`, which are decoded by [`de_int`].
//!
//! Some fields mirror the JSON schema for completeness and are not consumed by
//! every code path yet, so dead-code analysis is relaxed for this data model.
#![allow(dead_code)]

use serde::{Deserialize, Deserializer};

/// Top-level target MCU definition.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetConfig {
    /// Canonical target id, e.g. `"nordic/nrf54/nrf54l15"`.
    pub id: String,
    /// Human-readable name, e.g. `"nRF54L15"`.
    pub name: String,
    /// Platform handler key, e.g. `"nordic"`.
    pub platform: String,
    /// CPU core identifier, e.g. `"cortex-m33"`.
    #[serde(default)]
    pub cpu: Option<String>,
    /// Expected CPU TAP id (informational).
    #[serde(default)]
    pub cputapid: Option<String>,
    /// Control Access Port configuration (Nordic).
    #[serde(default)]
    pub ctrl_ap: Option<CtrlAp>,
    /// ERASEALLSTATUS status code mapping (Nordic).
    #[serde(default)]
    pub erase_all_status: Option<EraseAllStatus>,
    /// Flash controller configuration.
    #[serde(default)]
    pub flash_controller: Option<FlashController>,
    /// Flash memory region.
    #[serde(default)]
    pub flash: Option<MemoryRegion>,
    /// SRAM region (used as the default RTT scan area).
    #[serde(default)]
    pub sram: Option<SramRegion>,
    /// Supported capabilities, e.g. `["recover", "flash", "verify", "rtt"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Free-form description.
    #[serde(default)]
    pub description: Option<String>,
}

impl TargetConfig {
    /// Returns true if the target advertises the given capability.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// Nordic Control Access Port configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CtrlAp {
    /// AP index of the CTRL-AP.
    #[serde(deserialize_with = "de_int")]
    pub num: u8,
    /// Expected CTRL-AP IDR value.
    #[serde(deserialize_with = "de_int")]
    pub idr: u32,
}

/// Mapping of ERASEALLSTATUS register values to their meaning.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EraseAllStatus {
    #[serde(deserialize_with = "de_int")]
    pub ready: u32,
    #[serde(deserialize_with = "de_int")]
    pub ready_to_reset: u32,
    #[serde(deserialize_with = "de_int")]
    pub busy: u32,
    #[serde(deserialize_with = "de_int")]
    pub error: u32,
}

/// Flash controller description (RRAMC for nRF54, NVMC for nRF52).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashController {
    /// Controller type: `"rramc"` or `"nvmc"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Controller register base address.
    #[serde(deserialize_with = "de_int")]
    pub base: u64,
    /// Named register offsets.
    #[serde(default)]
    pub registers: Option<FlashRegisters>,
}

/// RRAMC register set.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashRegisters {
    pub config: ConfigRegister,
    pub ready: OffsetRegister,
    #[serde(default)]
    pub ready_next: Option<OffsetRegister>,
}

/// A configuration register with an enable value.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigRegister {
    #[serde(deserialize_with = "de_int")]
    pub offset: u64,
    #[serde(deserialize_with = "de_int")]
    pub enable_value: u32,
}

/// A register addressed solely by its offset.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OffsetRegister {
    #[serde(deserialize_with = "de_int")]
    pub offset: u64,
}

/// A flash memory region.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRegion {
    #[serde(deserialize_with = "de_int")]
    pub address: u64,
    #[serde(deserialize_with = "de_int")]
    pub size: u64,
}

/// An SRAM region with an optional debugger work area size.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SramRegion {
    #[serde(deserialize_with = "de_int")]
    pub address: u64,
    #[serde(default, deserialize_with = "de_int_opt")]
    pub work_area_size: Option<u64>,
}

/// Deserialize an integer that may be encoded as a JSON number or a decimal /
/// `0x`-prefixed hex string. Generic over any integer that is `TryFrom<u64>`.
pub fn de_int<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<u64>,
    <T as TryFrom<u64>>::Error: std::fmt::Display,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    let n = value_to_u64(&value).map_err(Error::custom)?;
    T::try_from(n).map_err(|e| Error::custom(format!("value {n} out of range: {e}")))
}

/// Like [`de_int`] but for `Option<T>` fields.
pub fn de_int_opt<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<u64>,
    <T as TryFrom<u64>>::Error: std::fmt::Display,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    let n = value_to_u64(&value).map_err(Error::custom)?;
    T::try_from(n)
        .map(Some)
        .map_err(|e| Error::custom(format!("value {n} out of range: {e}")))
}

/// Coerce a JSON value into a `u64`, accepting either a JSON number or a
/// decimal / hex string.
fn value_to_u64(value: &serde_json::Value) -> Result<u64, String> {
    match value {
        serde_json::Value::String(s) => parse_int_str(s),
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| format!("expected an unsigned integer, got {n}")),
        other => Err(format!("expected string or number, got {other}")),
    }
}

/// Parse an integer from a decimal or `0x`-prefixed hex string.
fn parse_int_str(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex '{s}': {e}"))
    } else {
        s.parse::<u64>()
            .map_err(|e| format!("invalid integer '{s}': {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full nRF54L15 definition deserializes, decoding hex-string fields into
    /// integers across the nested CTRL-AP / flash-controller / SRAM blocks.
    #[test]
    fn parses_nrf54l15_definition() {
        let json = r#"{
            "id": "nordic/nrf54/nrf54l15",
            "name": "nRF54L15",
            "platform": "nordic",
            "cpu": "cortex-m33",
            "cputapid": "0x6ba02477",
            "ctrlAp": { "num": 2, "idr": "0x32880000" },
            "eraseAllStatus": { "ready": 0, "readyToReset": 1, "busy": 2, "error": 3 },
            "flashController": {
                "type": "rramc",
                "base": "0x5004B000",
                "registers": {
                    "config": { "offset": "0x500", "enableValue": "0x101" },
                    "ready": { "offset": "0x400" },
                    "readyNext": { "offset": "0x404" }
                }
            },
            "flash": { "address": "0x00000000", "size": "0x0017D000" },
            "sram": { "address": "0x20000000", "workAreaSize": "0x4000" },
            "capabilities": ["recover", "flash", "verify", "rtt"],
            "description": "Nordic nRF54L15 (Cortex-M33, RRAMC)"
        }"#;

        let cfg: TargetConfig = serde_json::from_str(json).expect("valid definition");
        let ctrl = cfg.ctrl_ap.as_ref().unwrap();
        assert_eq!(ctrl.num, 2);
        assert_eq!(ctrl.idr, 0x3288_0000);

        let fc = cfg.flash_controller.as_ref().unwrap();
        assert_eq!(fc.kind, "rramc");
        assert_eq!(fc.base, 0x5004_B000);
        let regs = fc.registers.as_ref().unwrap();
        assert_eq!(regs.config.offset, 0x500);
        assert_eq!(regs.config.enable_value, 0x101);

        assert_eq!(cfg.sram.as_ref().unwrap().address, 0x2000_0000);
        assert!(cfg.has_capability("rtt"));
    }
}
