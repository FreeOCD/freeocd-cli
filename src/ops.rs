// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Operation orchestration: ties together targets, probe access, platform
//! handlers and RTT for each CLI subcommand.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::{FlashArgs, RttArgs, TargetArgs, VerifyArgs};
use crate::dap::{self, arm as dap_arm};
use crate::dapjs::rtt::{Rtt, RttConfig};
use crate::hex::{self, Firmware};
use crate::platform;
use crate::targets::{self, TargetConfig};

/// `list`: show connected probes and available targets.
pub fn run_list() -> Result<()> {
    let filters = targets::load_probe_filters();
    let probes = dap::list_probes();

    println!("Connected probes:");
    if probes.is_empty() {
        println!("  (none found)");
    } else {
        for (i, p) in probes.iter().enumerate() {
            let note = filters
                .iter()
                .find(|f| f.vid == p.vendor_id)
                .map(|f| match &f.comment {
                    // The comment is formatted "Vendor — Products"; show the vendor part.
                    Some(c) => format!("  [{}]", c.split('—').next().unwrap_or(c).trim()),
                    None => "  [known CMSIS-DAP vendor]".to_string(),
                })
                .unwrap_or_default();
            let serial = p.serial_number.as_deref().unwrap_or("-");
            println!(
                "  {i}: {} ({:04x}:{:04x}) serial={serial}{note}",
                p.identifier, p.vendor_id, p.product_id
            );
        }
    }

    println!("\nAvailable targets:");
    for t in targets::list_targets()? {
        println!("  {} — {} [{}]", t.id, t.name, t.capabilities.join(", "));
        if !t.description.is_empty() {
            println!("       {}", t.description);
        }
    }
    Ok(())
}

/// `flash`: recover, flash, optionally verify, then reset.
pub fn run_flash(args: &FlashArgs) -> Result<()> {
    let target = targets::load_target(&args.target)?;
    ensure_capability(&target, "flash")?;
    let firmware = read_firmware(&args.file)?;
    let handler = platform::handler_for(&target)?;
    let mut iface = dap::open_interface(&args.probe.to_options())?;

    run_with_bar("recover", |cb| handler.recover(iface.as_mut(), cb))?;
    // Re-establish the debug port after the reset triggered by recovery.
    iface.reinitialize().ok();

    run_with_bar("flash", |cb| handler.flash(iface.as_mut(), &firmware, cb))?;

    if args.verify {
        ensure_capability(&target, "verify")?;
        let outcome = run_with_bar("verify", |cb| handler.verify(iface.as_mut(), &firmware, cb))?;
        if !outcome.success {
            bail!(
                "Verification failed: {} byte mismatch(es)",
                outcome.mismatches
            );
        }
    }

    handler.reset(iface.as_mut())?;
    tracing::info!("Flash completed successfully");
    Ok(())
}

/// `recover`: mass-erase (unlock), then reset.
pub fn run_recover(args: &TargetArgs) -> Result<()> {
    let target = targets::load_target(&args.target)?;
    ensure_capability(&target, "recover")?;
    let handler = platform::handler_for(&target)?;
    let mut iface = dap::open_interface(&args.probe.to_options())?;

    run_with_bar("recover", |cb| handler.recover(iface.as_mut(), cb))?;
    handler.reset(iface.as_mut())?;
    tracing::info!("Recover completed successfully");
    Ok(())
}

/// `verify`: read back flash and compare against a firmware file.
pub fn run_verify(args: &VerifyArgs) -> Result<()> {
    let target = targets::load_target(&args.target)?;
    ensure_capability(&target, "verify")?;
    let firmware = read_firmware(&args.file)?;
    let handler = platform::handler_for(&target)?;
    let mut iface = dap::open_interface(&args.probe.to_options())?;

    let outcome = run_with_bar("verify", |cb| handler.verify(iface.as_mut(), &firmware, cb))?;
    if !outcome.success {
        bail!(
            "Verification failed: {} byte mismatch(es)",
            outcome.mismatches
        );
    }
    tracing::info!("Verification passed");
    Ok(())
}

/// `reset`: reset the target device.
pub fn run_reset(args: &TargetArgs) -> Result<()> {
    let target = targets::load_target(&args.target)?;
    let handler = platform::handler_for(&target)?;
    let mut iface = dap::open_interface(&args.probe.to_options())?;
    handler.reset(iface.as_mut())?;
    Ok(())
}

/// `rtt`: open a bidirectional SEGGER RTT terminal.
pub fn run_rtt(args: &RttArgs) -> Result<()> {
    let target = targets::load_target(&args.target)?;
    ensure_capability(&target, "rtt")?;
    let mut iface = dap::open_interface(&args.probe.to_options())?;

    if args.reset {
        let handler = platform::handler_for(&target)?;
        handler.reset(iface.as_mut())?;
        iface.reinitialize().ok();
    }

    let mut config = RttConfig::default();
    if let Some(addr) = args.scan_addr {
        config.scan_start = addr;
    } else if let Some(sram) = &target.sram {
        config.scan_start = sram.address;
    }
    if let Some(range) = args.scan_range {
        config.scan_range = range;
    }

    let mut mem = iface.memory_interface(&dap_arm::mem_ap())?;
    let rtt = Rtt::attach(mem.as_mut(), &config)?;
    let (up, down) = rtt.channel_counts();
    tracing::info!(
        "RTT attached: {up} up channel(s), {down} down channel(s). Press Ctrl-C to exit."
    );

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        let _ = ctrlc::set_handler(move || running.store(false, Ordering::SeqCst));
    }

    // Read stdin on a background thread and forward to the polling loop.
    // Note: stdin is line-buffered, so input is sent after each newline.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    {
        let running = running.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 256];
            while running.load(Ordering::SeqCst) {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let mut stdout = std::io::stdout();
    let poll = Duration::from_millis(args.poll_ms);
    while running.load(Ordering::SeqCst) {
        if up > 0 {
            let data = rtt.read_up(mem.as_mut(), 0)?;
            if !data.is_empty() {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
        }
        if down > 0 {
            while let Ok(chunk) = rx.try_recv() {
                if rtt.write_down(mem.as_mut(), 0, &chunk)? == 0 {
                    tracing::warn!("RTT down-buffer full; input dropped");
                }
            }
        }
        std::thread::sleep(poll);
    }

    tracing::info!("RTT terminal closed");
    Ok(())
}

/// Run an operation with a percentage progress bar.
fn run_with_bar<T>(
    label: &str,
    op: impl FnOnce(&mut platform::ProgressFn) -> Result<T>,
) -> Result<T> {
    let bar = ProgressBar::new(100);
    bar.set_style(
        ProgressStyle::with_template("{prefix:>8} [{bar:40}] {pos:>3}%")
            .expect("valid template")
            .progress_chars("=>-"),
    );
    bar.set_prefix(label.to_string());

    let mut cb = |p: f64| bar.set_position(p.round() as u64);
    let result = op(&mut cb);
    bar.finish_and_clear();
    result
}

/// Read and parse an Intel HEX firmware file.
fn read_firmware(path: &Path) -> Result<Firmware> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read firmware file {}", path.display()))?;
    let firmware = hex::parse_intel_hex(&content)?;
    tracing::info!(
        "Loaded {} bytes from {} (start 0x{:08X})",
        firmware.size,
        path.display(),
        firmware.start_address
    );
    Ok(firmware)
}

/// Ensure the target advertises a capability (no-op if it lists none).
fn ensure_capability(target: &TargetConfig, capability: &str) -> Result<()> {
    if !target.capabilities.is_empty() && !target.has_capability(capability) {
        bail!(
            "Target '{}' does not support '{}' (capabilities: {})",
            target.id,
            capability,
            target.capabilities.join(", ")
        );
    }
    Ok(())
}
