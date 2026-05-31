// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Target definition management.
//!
//! Loads the shared target definitions and the central CMSIS-DAP probe filter
//! list. `build.rs` copies `vendor/freeocd-web/public/targets` into `OUT_DIR`,
//! and that copy is embedded into the binary via `include_dir!`, so the CLI is
//! fully self-contained.

pub mod definition;
pub mod probe_filters;

use anyhow::{anyhow, Context, Result};
use include_dir::{include_dir, Dir};
use serde::Deserialize;

pub use definition::TargetConfig;
pub use probe_filters::ProbeFilter;

/// The `vendor/freeocd-web/public/targets` directory, copied into `OUT_DIR` by
/// `build.rs` and embedded into the binary at compile time.
static TARGET_ASSETS: Dir<'static> = include_dir!("$OUT_DIR/targets");

/// Short metadata about a target, used by the `list` command.
#[derive(Debug, Clone)]
pub struct TargetSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub capabilities: Vec<String>,
}

#[derive(Deserialize)]
struct Index {
    #[serde(default)]
    targets: Vec<String>,
}

/// Read an embedded asset as UTF-8 text.
fn read_asset(path: &str) -> Result<String> {
    let file = TARGET_ASSETS
        .get_file(path)
        .ok_or_else(|| anyhow!("Embedded asset not found: {path}"))?;
    file.contents_utf8()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Embedded asset is not valid UTF-8: {path}"))
}

/// Load the ordered, de-duplicated list of target ids from `index.json`.
pub fn load_index() -> Result<Vec<String>> {
    let index: Index =
        serde_json::from_str(&read_asset("index.json")?).context("Failed to parse index.json")?;

    let mut seen = std::collections::HashSet::new();
    let ids = index
        .targets
        .into_iter()
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect();
    Ok(ids)
}

/// Load a single target definition by id (e.g. `"nordic/nrf54/nrf54l15"`).
pub fn load_target(id: &str) -> Result<TargetConfig> {
    let path = format!("{id}.json");
    let raw = read_asset(&path).with_context(|| format!("Unknown target: {id}"))?;
    serde_json::from_str(&raw).with_context(|| format!("Failed to parse target definition: {id}"))
}

/// Load the central CMSIS-DAP probe filter list.
///
/// Returns an empty list (no vendor filter) if the file is missing or invalid.
pub fn load_probe_filters() -> Vec<ProbeFilter> {
    match read_asset("probe-filters.json") {
        Ok(raw) => probe_filters::parse_probe_filters(&raw).unwrap_or_default(),
        Err(err) => {
            tracing::warn!("Could not load probe-filters.json: {err}");
            Vec::new()
        }
    }
}

/// Load summaries for all targets referenced by `index.json`.
///
/// Targets that fail to load are skipped with a warning so a single bad file
/// does not break the whole listing.
pub fn list_targets() -> Result<Vec<TargetSummary>> {
    let mut summaries = Vec::new();
    for id in load_index()? {
        match load_target(&id) {
            Ok(cfg) => summaries.push(TargetSummary {
                id: cfg.id,
                name: cfg.name,
                description: cfg.description.unwrap_or_default(),
                capabilities: cfg.capabilities,
            }),
            Err(err) => tracing::warn!("Skipping target '{id}': {err}"),
        }
    }
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded `index.json` includes the nRF54L15 target id.
    #[test]
    fn embedded_index_lists_nrf54l15() {
        let ids = load_index().expect("index loads");
        assert!(ids.iter().any(|id| id == "nordic/nrf54/nrf54l15"));
    }

    /// The nRF54L15 definition parses and exposes its Nordic CTRL-AP block.
    #[test]
    fn embedded_target_loads() {
        let cfg = load_target("nordic/nrf54/nrf54l15").expect("target loads");
        assert_eq!(cfg.platform, "nordic");
        assert!(cfg.ctrl_ap.is_some());
    }

    /// The embedded probe filter list includes the SeeedStudio vendor id.
    #[test]
    fn probe_filters_load() {
        let filters = load_probe_filters();
        assert!(filters.iter().any(|f| f.vid == 0x2886));
    }
}
