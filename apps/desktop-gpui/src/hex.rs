//! Lowercase hex for digests.

use std::fmt::Write as _;

/// `bytes` as lowercase hex, two digits each.
pub fn lower_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
