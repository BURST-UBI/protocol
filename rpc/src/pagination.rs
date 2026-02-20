//! Cursor-based pagination utilities for list endpoints.

use serde::{Deserialize, Serialize};

/// Default page size when `count` is not specified.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// Maximum allowed page size.
pub const MAX_PAGE_SIZE: u32 = 1000;

/// Common pagination parameters accepted by list endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct PaginationParams {
    /// Opaque cursor from a previous response (base64-encoded offset).
    pub cursor: Option<String>,
    /// Number of items per page (default 100, max 1000).
    pub count: Option<u32>,
}

impl PaginationParams {
    /// Resolve effective page size, clamped to [1, MAX_PAGE_SIZE].
    pub fn effective_count(&self) -> u32 {
        self.count
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE)
    }

    /// Decode the cursor to a numeric offset. Returns 0 if cursor is absent or
    /// invalid. The cursor is a base64-encoded decimal string of the offset.
    pub fn decode_offset(&self) -> u64 {
        self.cursor
            .as_deref()
            .and_then(|c| decode_cursor(c))
            .unwrap_or(0)
    }
}

/// Pagination metadata included in list responses.
#[derive(Debug, Clone, Serialize)]
pub struct PaginationMeta {
    /// Cursor to pass for the next page, or `None` if this is the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Encode a numeric offset into an opaque cursor string (base64).
pub fn encode_cursor(offset: u64) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    write!(buf, "{offset}").unwrap();
    base64_encode(&buf)
}

/// Decode a cursor string back to a numeric offset.
pub fn decode_cursor(cursor: &str) -> Option<u64> {
    let bytes = base64_decode(cursor)?;
    let s = std::str::from_utf8(&bytes).ok()?;
    s.parse::<u64>().ok()
}

/// Compute the next-page cursor given the current offset and the number of
/// items returned. Returns `None` when fewer items than `count` were returned
/// (meaning we've reached the end).
pub fn next_cursor(current_offset: u64, returned: usize, page_size: u32) -> Option<String> {
    if (returned as u32) < page_size {
        None
    } else {
        Some(encode_cursor(current_offset + returned as u64))
    }
}

// Minimal base64 helpers (no extra dependency needed).

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut accum: u32 = 0;
        let mut bits = 0;
        for &b in chunk {
            accum = (accum << 6) | val(b)?;
            bits += 6;
        }
        // shift left so the meaningful bits are at the top of a 24-bit window
        accum <<= 24 - bits;
        out.push((accum >> 16) as u8);
        if chunk.len() > 2 {
            out.push((accum >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(accum as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        for offset in [0u64, 1, 42, 100, 999, 123456789] {
            let encoded = encode_cursor(offset);
            let decoded = decode_cursor(&encoded);
            assert_eq!(decoded, Some(offset), "roundtrip failed for {offset}");
        }
    }

    #[test]
    fn next_cursor_returns_none_at_end() {
        assert!(next_cursor(0, 50, 100).is_none());
    }

    #[test]
    fn next_cursor_returns_some_when_full_page() {
        let c = next_cursor(0, 100, 100);
        assert!(c.is_some());
        assert_eq!(decode_cursor(c.as_deref().unwrap()), Some(100));
    }

    #[test]
    fn effective_count_defaults() {
        let p = PaginationParams {
            cursor: None,
            count: None,
        };
        assert_eq!(p.effective_count(), 100);
    }

    #[test]
    fn effective_count_clamps() {
        let p = PaginationParams {
            cursor: None,
            count: Some(5000),
        };
        assert_eq!(p.effective_count(), 1000);
    }
}
