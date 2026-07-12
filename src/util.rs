// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Small shared parsing helpers.

/// Parse a `u64` from a decimal or `0x`-prefixed hex string.
pub fn parse_u64_maybe_hex(s: &str) -> Result<u64, String> {
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

    /// Decimal, lower/upper hex prefixes and surrounding whitespace all parse;
    /// malformed input is rejected.
    #[test]
    fn parses_decimal_and_hex() {
        assert_eq!(parse_u64_maybe_hex("42"), Ok(42));
        assert_eq!(parse_u64_maybe_hex("0x2A"), Ok(42));
        assert_eq!(parse_u64_maybe_hex("0X2a"), Ok(42));
        assert_eq!(parse_u64_maybe_hex(" 0x10 "), Ok(16));
        assert!(parse_u64_maybe_hex("nope").is_err());
        assert!(parse_u64_maybe_hex("0xZZ").is_err());
    }
}
