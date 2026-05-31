// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Loader for the central CMSIS-DAP probe vendor ID list.
//!
//! Parses `probe-filters.json`: each entry of the `vendorIds` array is either a
//! bare hex string (`"0x03EB"`) or an object with a `vid` hex string and an
//! optional `$comment` describing the vendor / products.

use anyhow::{Context, Result};
use serde_json::Value;

/// A known CMSIS-DAP probe vendor.
#[derive(Debug, Clone)]
pub struct ProbeFilter {
    /// USB vendor id.
    pub vid: u16,
    /// Optional human-readable description of the vendor / products.
    pub comment: Option<String>,
}

/// Parse `probe-filters.json` content into a list of [`ProbeFilter`]s.
///
/// Invalid individual entries are skipped (with a warning) rather than failing
/// the whole parse, matching the resilient behaviour of the web loader.
pub fn parse_probe_filters(json: &str) -> Result<Vec<ProbeFilter>> {
    let root: Value = serde_json::from_str(json).context("probe-filters.json is not valid JSON")?;

    let Some(vendor_ids) = root.get("vendorIds").and_then(Value::as_array) else {
        tracing::warn!("probe-filters.json is missing a `vendorIds` array; ignoring");
        return Ok(Vec::new());
    };

    let mut filters = Vec::with_capacity(vendor_ids.len());
    for entry in vendor_ids {
        let (vid_str, comment) = match entry {
            Value::String(s) => (s.as_str(), None),
            Value::Object(_) => {
                let vid = entry.get("vid").and_then(Value::as_str);
                let comment = entry
                    .get("$comment")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                match vid {
                    Some(v) => (v, comment),
                    None => {
                        tracing::warn!("Skipping probe-filters entry without `vid`: {entry}");
                        continue;
                    }
                }
            }
            other => {
                tracing::warn!("Skipping invalid probe-filters entry: {other}");
                continue;
            }
        };

        match parse_vid(vid_str) {
            Some(vid) => filters.push(ProbeFilter { vid, comment }),
            None => tracing::warn!("Skipping invalid vendor id in probe-filters.json: {vid_str}"),
        }
    }

    Ok(filters)
}

/// Parse a vendor id from a `0x`-prefixed hex string, returning `None` if it is
/// malformed or out of range.
fn parse_vid(s: &str) -> Option<u16> {
    let s = s.trim();
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u16::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid bare-string and object entries are kept; entries without a usable
    /// `vid` are skipped rather than failing the whole parse.
    #[test]
    fn parses_mixed_entries() {
        let json = r#"{
            "vendorIds": [
                "0x0D28",
                { "vid": "0x2886", "$comment": "SeeedStudio — XIAO" },
                { "$comment": "missing vid" },
                "not-a-vid"
            ]
        }"#;
        let filters = parse_probe_filters(json).unwrap();
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].vid, 0x0D28);
        assert_eq!(filters[1].vid, 0x2886);
        assert!(filters[1].comment.as_deref().unwrap().contains("XIAO"));
    }
}
