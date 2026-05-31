// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Logging setup based on the `tracing` ecosystem.
//!
//! Console logging where the default level is `info` (or `debug` with
//! `--verbose`); the `RUST_LOG` environment variable always takes precedence.

use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber.
///
/// When `verbose` is set the default level is `debug`, otherwise `info`.
/// The `RUST_LOG` environment variable, if present, always takes precedence.
pub fn init(verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_level(true)
        .compact()
        .init();
}
