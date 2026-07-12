// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Intel HEX firmware parsing.
//!
//! Parses an Intel HEX file into a single contiguous binary image plus its
//! start address. Gaps between records are filled with `0xFF` (the erased
//! flash value). The address span is bounded so a malformed or extremely
//! sparse file fails with a clear error instead of exhausting memory.

use anyhow::{bail, Context, Result};
use ihex::Record;

/// Maximum contiguous image span accepted from a HEX file. Generously above
/// any Cortex-M flash size; primarily guards against sparse files whose gap
/// fill would allocate gigabytes.
const MAX_IMAGE_SIZE: u64 = 256 * 1024 * 1024;

/// A parsed firmware image: a contiguous byte buffer and where it starts.
#[derive(Debug, Clone)]
pub struct Firmware {
    /// Contiguous firmware bytes (gaps filled with `0xFF`).
    pub data: Vec<u8>,
    /// Absolute start address of `data[0]`.
    pub start_address: u64,
}

/// A contiguous run of bytes from one data record.
struct Segment {
    address: u64,
    bytes: Vec<u8>,
}

/// Parse an Intel HEX string into a [`Firmware`] image.
pub fn parse_intel_hex(hex: &str) -> Result<Firmware> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut extended_address: u64 = 0;

    for record in ihex::Reader::new(hex) {
        let record = record.context("Failed to parse Intel HEX record")?;
        match record {
            Record::Data { offset, value } => {
                if !value.is_empty() {
                    segments.push(Segment {
                        address: extended_address + u64::from(offset),
                        bytes: value,
                    });
                }
            }
            // Extended Segment Address: segment base, shifted left by 4.
            Record::ExtendedSegmentAddress(segment) => {
                extended_address = u64::from(segment) << 4;
            }
            // Extended Linear Address: upper 16 bits of a 32-bit address.
            Record::ExtendedLinearAddress(high) => {
                extended_address = u64::from(high) << 16;
            }
            // Start address records carry no flash data.
            Record::EndOfFile
            | Record::StartSegmentAddress { .. }
            | Record::StartLinearAddress(_) => {}
        }
    }

    if segments.is_empty() {
        bail!("No data found in HEX file");
    }

    let min_address = segments.iter().map(|s| s.address).min().expect("non-empty");
    let max_address = segments
        .iter()
        .map(|s| s.address + s.bytes.len() as u64)
        .max()
        .expect("non-empty");

    let size = max_address - min_address;
    if size > MAX_IMAGE_SIZE {
        bail!(
            "HEX file spans 0x{min_address:08X}..0x{max_address:08X} ({size} bytes), \
             exceeding the {MAX_IMAGE_SIZE} byte limit; the file is likely sparse or corrupt"
        );
    }

    warn_on_overlaps(&segments);

    let mut data = vec![0xFFu8; size as usize];
    for segment in &segments {
        let start = (segment.address - min_address) as usize;
        data[start..start + segment.bytes.len()].copy_from_slice(&segment.bytes);
    }

    Ok(Firmware {
        data,
        start_address: min_address,
    })
}

/// Warn (once) if any data records overlap; later records win.
fn warn_on_overlaps(segments: &[Segment]) {
    let mut ranges: Vec<(u64, u64)> = segments
        .iter()
        .map(|s| (s.address, s.address + s.bytes.len() as u64))
        .collect();
    ranges.sort_unstable();

    let mut prev_end = 0u64;
    for (i, &(start, end)) in ranges.iter().enumerate() {
        if i > 0 && start < prev_end {
            tracing::warn!(
                "HEX file contains overlapping data records around 0x{start:08X}; \
                 later records take precedence"
            );
            return;
        }
        prev_end = prev_end.max(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Classic Intel HEX sample: two 16-byte data records at 0x0100 then EOF.
    const SAMPLE: &str = ":10010000214601360121470136007EFE09D2190140\n\
         :100110002146017E17C20001FF5F16002148011928\n\
         :00000001FF\n";

    /// Two adjacent data records parse into one 32-byte image at 0x0100.
    #[test]
    fn parses_contiguous_image() {
        let fw = parse_intel_hex(SAMPLE).expect("valid hex");
        assert_eq!(fw.start_address, 0x0100);
        assert_eq!(fw.data.len(), 32);
        assert_eq!(fw.data[0], 0x21);
        assert_eq!(fw.data[16], 0x21);
    }

    /// A one-byte hole between records is filled with the erased value `0xFF`.
    #[test]
    fn fills_gaps_with_ff() {
        // Two data records with a one-byte gap between them at 0x00..0x02.
        let hex = ":0100000000FF\n:01000200 AA53\n:00000001FF\n".replace(' ', "");
        let fw = parse_intel_hex(&hex).expect("valid hex");
        assert_eq!(fw.start_address, 0x0000);
        assert_eq!(fw.data, vec![0x00, 0xFF, 0xAA]);
    }

    /// A HEX file with no data records is rejected.
    #[test]
    fn rejects_empty() {
        assert!(parse_intel_hex(":00000001FF\n").is_err());
    }

    /// A sparse file whose span exceeds the image size limit is rejected
    /// instead of triggering a huge allocation.
    #[test]
    fn rejects_oversized_span() {
        // One byte at 0x0000_0000 and one byte at 0xF000_0000 (via an
        // Extended Linear Address record with upper bits 0xF000).
        let hex = ":0100000000FF\n:02000004F0000A\n:01000000AA55\n:00000001FF\n";
        let err = parse_intel_hex(hex).expect_err("span too large");
        assert!(err.to_string().contains("exceeding"));
    }

    /// Overlapping records parse (later record wins) rather than failing.
    #[test]
    fn overlapping_records_later_wins() {
        let hex = ":02000000AAAAAA\n:0100010055A9\n:00000001FF\n";
        let fw = parse_intel_hex(hex).expect("valid hex");
        assert_eq!(fw.data, vec![0xAA, 0x55]);
    }
}
