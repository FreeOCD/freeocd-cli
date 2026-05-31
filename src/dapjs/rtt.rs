// SPDX-License-Identifier: MIT
//
// Copyright (C) 2021 Ciro Cattuto <ciro.cattuto@gmail.com>
// Copyright (c) 2026 FreeOCD
//
// This file is a Rust port of dapjs `examples/rtt/rtt.js` (MIT License). See the
// LICENSE file in this directory for the full MIT license text.

//! SEGGER RTT (Real-Time Transfer) support.
//!
//! Locates the RTT control block in target RAM by scanning for the
//! `"SEGGER RTT"` signature, then performs up-buffer (target -> host) reads and
//! down-buffer (host -> target) writes through probe-rs's MEM-AP memory
//! interface. RTT uses background memory access, so the target keeps running.

use anyhow::{bail, Result};
use probe_rs::architecture::arm::memory::ArmMemoryInterface;

/// RTT control-block scan configuration.
#[derive(Debug, Clone, Copy)]
pub struct RttConfig {
    /// First address to scan.
    pub scan_start: u64,
    /// Number of bytes to scan from `scan_start`.
    pub scan_range: u64,
    /// Window size for each block read.
    pub scan_block: u64,
    /// Stride between scan windows.
    pub scan_stride: u64,
}

impl Default for RttConfig {
    /// Defaults targeting the common Cortex-M SRAM window (0x2000_0000, 64 KiB).
    fn default() -> Self {
        Self {
            scan_start: 0x2000_0000,
            scan_range: 0x1_0000, // 64 KiB
            scan_block: 0x1000,   // 4 KiB
            scan_stride: 0x0800,  // 2 KiB
        }
    }
}

/// `"SEGGER RTT"` control-block signature.
const RTT_SIGNATURE: &[u8] = b"SEGGER RTT";
/// Defensive upper bound on the buffer counts read from the control block.
const MAX_BUFFERS: u32 = 64;

/// A single RTT ring buffer descriptor.
#[derive(Debug, Clone, Copy)]
struct RttBuffer {
    /// Address of this descriptor inside the control block.
    descriptor_addr: u64,
    /// Address of the ring buffer storage in target memory.
    buffer_ptr: u32,
    /// Ring buffer size in bytes.
    size: u32,
}

/// An attached RTT session.
pub struct Rtt {
    up: Vec<RttBuffer>,
    down: Vec<RttBuffer>,
}

impl Rtt {
    /// Scan for and attach to the RTT control block.
    pub fn attach(mem: &mut dyn ArmMemoryInterface, config: &RttConfig) -> Result<Rtt> {
        let ctrl_addr = Self::find_control_block(mem, config)?;
        tracing::info!("Found RTT control block at 0x{ctrl_addr:08X}");

        // Control block header: char acID[16]; i32 MaxNumUpBuffers; i32 MaxNumDownBuffers.
        let mut header = [0u8; 24];
        mem.read_8(ctrl_addr, &mut header)?;
        let num_up = u32::from_le_bytes(header[16..20].try_into().unwrap());
        let num_down = u32::from_le_bytes(header[20..24].try_into().unwrap());

        if num_up > MAX_BUFFERS || num_down > MAX_BUFFERS {
            bail!("Implausible RTT buffer counts (up={num_up}, down={num_down}); aborting");
        }
        tracing::info!("RTT: {num_up} up buffer(s), {num_down} down buffer(s)");

        // Each ring buffer descriptor is 24 bytes: const char* sName; char* pBuffer;
        // u32 SizeOfBuffer; u32 WrOff; u32 RdOff; u32 Flags.
        let total = (num_up + num_down) as usize;
        let mut descriptors = vec![0u8; total * 24];
        if total > 0 {
            mem.read_8(ctrl_addr + 24, &mut descriptors)?;
        }

        let parse = |index: usize| -> RttBuffer {
            let base = index * 24;
            RttBuffer {
                descriptor_addr: ctrl_addr + 24 + base as u64,
                buffer_ptr: u32::from_le_bytes(descriptors[base + 4..base + 8].try_into().unwrap()),
                size: u32::from_le_bytes(descriptors[base + 8..base + 12].try_into().unwrap()),
            }
        };

        let up = (0..num_up as usize).map(parse).collect();
        let down = (num_up as usize..total).map(parse).collect();

        Ok(Rtt { up, down })
    }

    /// Number of (up, down) channels.
    pub fn channel_counts(&self) -> (usize, usize) {
        (self.up.len(), self.down.len())
    }

    /// Read any pending bytes from an up-buffer (target -> host).
    pub fn read_up(&self, mem: &mut dyn ArmMemoryInterface, channel: usize) -> Result<Vec<u8>> {
        let Some(buf) = self.up.get(channel) else {
            bail!("RTT up-buffer {channel} does not exist");
        };

        let write_off = mem.read_word_32(buf.descriptor_addr + 12)?;
        let read_off = mem.read_word_32(buf.descriptor_addr + 16)?;

        let buffer_ptr = u64::from(buf.buffer_ptr);
        let data = if write_off > read_off {
            let mut out = vec![0u8; (write_off - read_off) as usize];
            mem.read_8(buffer_ptr + u64::from(read_off), &mut out)?;
            out
        } else if write_off < read_off {
            // Wrapped: read tail then head.
            let mut tail = vec![0u8; (buf.size - read_off) as usize];
            mem.read_8(buffer_ptr + u64::from(read_off), &mut tail)?;
            let mut head = vec![0u8; write_off as usize];
            mem.read_8(buffer_ptr, &mut head)?;
            tail.extend_from_slice(&head);
            tail
        } else {
            return Ok(Vec::new());
        };

        // Advance the read offset to the write offset.
        mem.write_word_32(buf.descriptor_addr + 16, write_off)?;
        Ok(data)
    }

    /// Write bytes to a down-buffer (host -> target).
    ///
    /// Returns the number of bytes written, or `0` if the buffer cannot hold
    /// the whole payload right now.
    pub fn write_down(
        &self,
        mem: &mut dyn ArmMemoryInterface,
        channel: usize,
        data: &[u8],
    ) -> Result<usize> {
        let Some(buf) = self.down.get(channel) else {
            bail!("RTT down-buffer {channel} does not exist");
        };

        let read_off = mem.read_word_32(buf.descriptor_addr + 16)?;
        let mut write_off = mem.read_word_32(buf.descriptor_addr + 12)?;

        let available = ring_free_space(buf.size, write_off, read_off);
        if (available as usize) < data.len() {
            return Ok(0);
        }

        let buffer_ptr = u64::from(buf.buffer_ptr);
        for &byte in data {
            mem.write_8(buffer_ptr + u64::from(write_off), &[byte])?;
            write_off += 1;
            if write_off == buf.size {
                write_off = 0;
            }
        }
        mem.write_word_32(buf.descriptor_addr + 12, write_off)?;
        Ok(data.len())
    }

    /// Scan target memory for the RTT control block signature.
    fn find_control_block(mem: &mut dyn ArmMemoryInterface, config: &RttConfig) -> Result<u64> {
        let mut offset = 0u64;
        while offset < config.scan_range {
            let addr = config.scan_start + offset;
            let mut block = vec![0u8; config.scan_block as usize];
            match mem.read_8(addr, &mut block) {
                Ok(()) => {
                    if let Some(index) = find_subslice(&block, RTT_SIGNATURE) {
                        return Ok(addr + index as u64);
                    }
                }
                Err(err) => {
                    tracing::debug!("RTT scan read error at 0x{addr:08X}: {err}");
                }
            }
            offset += config.scan_stride;
        }
        bail!("RTT control block not found in scan range");
    }
}

/// Find the first index of `needle` within `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Bytes that can be written to a SEGGER RTT ring buffer of `size` bytes given
/// the current write/read offsets.
///
/// One slot is always left unused so that a full buffer (`free == 0`) stays
/// distinguishable from an empty one (`write_off == read_off`). Writing more
/// than this would wrap `write_off` onto `read_off`, which the target reads as
/// an empty buffer, silently discarding the data.
fn ring_free_space(size: u32, write_off: u32, read_off: u32) -> u32 {
    if write_off >= read_off {
        size - 1 - (write_off - read_off)
    } else {
        read_off - write_off - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `find_subslice` locates a present needle and returns `None` when absent.
    #[test]
    fn finds_signature() {
        let mut data = vec![0u8; 100];
        data[40..50].copy_from_slice(b"SEGGER RTT");
        assert_eq!(find_subslice(&data, b"SEGGER RTT"), Some(40));
        assert_eq!(find_subslice(&data, b"NOPE"), None);
    }

    /// The ring buffer always reserves one slot, so a `size`-byte buffer holds
    /// at most `size - 1` bytes and an empty buffer never reports `size` free.
    #[test]
    fn ring_free_space_reserves_one_slot() {
        // Empty buffer (write_off == read_off): all but one slot is free.
        assert_eq!(ring_free_space(16, 0, 0), 15);
        assert_eq!(ring_free_space(16, 8, 8), 15);
        // Write ahead of read (no wrap).
        assert_eq!(ring_free_space(16, 10, 4), 9);
        // Read ahead of write (wrapped).
        assert_eq!(ring_free_space(16, 4, 10), 5);
        // One slot before the read offset stays reserved (buffer full).
        assert_eq!(ring_free_space(16, 7, 8), 0);
        assert_eq!(ring_free_space(16, 15, 0), 0);
    }
}
