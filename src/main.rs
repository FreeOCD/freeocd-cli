// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! FreeOCD CLI entry point.
//!
//! A command-line debugger for ARM Cortex-M microcontrollers over CMSIS-DAP,
//! built on probe-rs. It reuses the freeocd-web target definition JSON (embedded
//! at build time) to drive flash / recover / verify / reset / RTT operations.

mod cli;
mod dap;
mod dapjs;
mod hex;
mod logging;
mod ops;
mod platform;
mod targets;

use clap::{CommandFactory, Parser};

use crate::cli::{Cli, Command};

/// Parse CLI arguments, initialize logging, dispatch the chosen subcommand and
/// exit with a non-zero status if the operation returns an error.
fn main() {
    let cli = Cli::parse();
    logging::init(cli.verbose);

    // No subcommand: print help to stdout and exit successfully.
    let Some(command) = &cli.command else {
        Cli::command()
            .print_help()
            .expect("failed to write help to stdout");
        println!();
        return;
    };

    let result = match command {
        Command::List => ops::run_list(),
        Command::Flash(args) => ops::run_flash(args),
        Command::Recover(args) => ops::run_recover(args),
        Command::Verify(args) => ops::run_verify(args),
        Command::Reset(args) => ops::run_reset(args),
        Command::Rtt(args) => ops::run_rtt(args),
    };

    if let Err(err) = result {
        tracing::error!("{err:#}");
        std::process::exit(1);
    }
}
