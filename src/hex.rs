// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Intel HEX firmware parsing.
//!
//! Parses an Intel HEX file into a single contiguous binary image plus its
//! start address. Gaps between records are filled with `0xFF` (the erased
//! flash value).

use anyhow::{bail, Context, Result};
use ihex::Record;

/// A parsed firmware image: a contiguous byte buffer and where it starts.
#[derive(Debug, Clone)]
pub struct Firmware {
    /// Contiguous firmware bytes (gaps filled with `0xFF`).
    pub data: Vec<u8>,
    /// Absolute start address of `data[0]`.
    pub start_address: u64,
    /// Length of `data` in bytes.
    pub size: u64,
}

/// Parse an Intel HEX string into a [`Firmware`] image.
pub fn parse_intel_hex(hex: &str) -> Result<Firmware> {
    let mut cells: Vec<(u64, u8)> = Vec::new();
    let mut extended_address: u64 = 0;
    let mut min_address: u64 = u64::MAX;
    let mut max_address: u64 = 0;

    for record in ihex::Reader::new(hex) {
        let record = record.context("Failed to parse Intel HEX record")?;
        match record {
            Record::Data { offset, value } => {
                let full = extended_address + u64::from(offset);
                for (i, byte) in value.iter().enumerate() {
                    cells.push((full + i as u64, *byte));
                }
                min_address = min_address.min(full);
                max_address = max_address.max(full + value.len() as u64);
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

    if cells.is_empty() {
        bail!("No data found in HEX file");
    }

    let size = max_address - min_address;
    let mut data = vec![0xFFu8; size as usize];
    for (address, value) in cells {
        data[(address - min_address) as usize] = value;
    }

    Ok(Firmware {
        data,
        start_address: min_address,
        size,
    })
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
        assert_eq!(fw.size, 32);
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
        assert_eq!(fw.size, 3);
        assert_eq!(fw.data, vec![0x00, 0xFF, 0xAA]);
    }

    /// A HEX file with no data records is rejected.
    #[test]
    fn rejects_empty() {
        assert!(parse_intel_hex(":00000001FF\n").is_err());
    }
}
