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
use crate::dap::{
    self,
    io::{DapIo, ProbeRsIo},
};
use crate::dapjs::rtt::{Rtt, RttConfig};
use crate::hex::{self, Firmware};
use crate::platform::{self, PlatformHandler};
use crate::targets::{self, TargetConfig};

/// A target-directed operation session: parsed target definition, its platform
/// handler and an opened probe connection.
struct Session {
    target: TargetConfig,
    handler: Box<dyn PlatformHandler>,
    io: ProbeRsIo,
}

/// Load the target definition, instantiate its handler and open the probe.
fn open_session(args: &TargetArgs) -> Result<Session> {
    let target = targets::load_target(&args.id)?;
    let handler = platform::handler_for(&target)?;
    let io = dap::open_io(&args.probe.to_options())?;
    Ok(Session {
        target,
        handler,
        io,
    })
}

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
    let mut s = open_session(&args.target)?;
    ensure_capability(&s.target, "flash")?;
    if args.verify {
        ensure_capability(&s.target, "verify")?;
    }
    let firmware = read_firmware(&args.file)?;

    run_with_bar("recover", |cb| s.handler.recover(&mut s.io, cb))?;
    // Re-establish the debug port after the reset triggered by recovery;
    // flashing cannot proceed over a dead connection.
    s.io.reinitialize()
        .context("Reconnect after recovery failed")?;

    run_with_bar("flash", |cb| s.handler.flash(&mut s.io, &firmware, cb))?;

    if args.verify {
        let outcome = run_with_bar("verify", |cb| s.handler.verify(&mut s.io, &firmware, cb))?;
        if !outcome.success {
            bail!(
                "Verification failed: {} byte mismatch(es)",
                outcome.mismatches
            );
        }
    }

    s.handler.reset(&mut s.io)?;
    tracing::info!("Flash completed successfully");
    Ok(())
}

/// `recover`: mass-erase (unlock), then reset.
pub fn run_recover(args: &TargetArgs) -> Result<()> {
    let mut s = open_session(args)?;
    ensure_capability(&s.target, "recover")?;

    run_with_bar("recover", |cb| s.handler.recover(&mut s.io, cb))?;
    s.handler.reset(&mut s.io)?;
    tracing::info!("Recover completed successfully");
    Ok(())
}

/// `verify`: read back flash and compare against a firmware file.
pub fn run_verify(args: &VerifyArgs) -> Result<()> {
    let mut s = open_session(&args.target)?;
    ensure_capability(&s.target, "verify")?;
    let firmware = read_firmware(&args.file)?;

    let outcome = run_with_bar("verify", |cb| s.handler.verify(&mut s.io, &firmware, cb))?;
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
    let mut s = open_session(args)?;
    s.handler.reset(&mut s.io)?;
    Ok(())
}

/// `rtt`: open a bidirectional SEGGER RTT terminal.
pub fn run_rtt(args: &RttArgs) -> Result<()> {
    let mut s = open_session(&args.target)?;
    ensure_capability(&s.target, "rtt")?;

    if args.reset {
        s.handler.reset(&mut s.io)?;
        // Re-establish the debug port after the reset; RTT cannot attach over
        // a dead connection.
        s.io.reinitialize()
            .context("Reconnect after reset failed")?;
    }

    let mut config = RttConfig::default();
    if let Some(addr) = args.scan_addr {
        config.scan_start = addr;
    } else if let Some(sram) = &s.target.sram {
        config.scan_start = sram.address;
    }
    if let Some(range) = args.scan_range {
        config.scan_range = range;
    }

    let mut mem = s.io.memory()?;
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
    let stdin_rx = spawn_stdin_reader(running.clone());

    let mut stdout = std::io::stdout();
    let poll = Duration::from_millis(args.poll_ms);
    let result = (|| -> Result<()> {
        while running.load(Ordering::SeqCst) {
            if up > 0 {
                let data = rtt.read_up(mem.as_mut(), 0)?;
                if !data.is_empty() {
                    stdout.write_all(&data)?;
                    stdout.flush()?;
                }
            }
            if down > 0 {
                while let Ok(chunk) = stdin_rx.try_recv() {
                    if rtt.write_down(mem.as_mut(), 0, &chunk)? == 0 {
                        tracing::warn!("RTT down-buffer full; input dropped");
                    }
                }
            }
            std::thread::sleep(poll);
        }
        Ok(())
    })();

    // Signal the stdin thread to stop regardless of how the loop exited. (It
    // may still be blocked in read() until the next line of input arrives, but
    // it will not touch the channel after this.)
    running.store(false, Ordering::SeqCst);

    result?;
    tracing::info!("RTT terminal closed");
    Ok(())
}

/// Read stdin on a background thread, forwarding chunks over a channel.
/// Note: stdin is line-buffered, so input is sent after each newline.
fn spawn_stdin_reader(running: Arc<AtomicBool>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
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
    rx
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
        firmware.data.len(),
        path.display(),
        firmware.start_address
    );
    Ok(firmware)
}

/// Ensure the target advertises a capability.
///
/// Targets that list no capabilities at all are treated as allowing every
/// operation; this keeps minimal / experimental target definitions usable.
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
