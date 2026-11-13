// SPDX-License-Identifier: AGPL-3.0-only
//! `Range` header parsing (RFC 9110 §14), single byte ranges only.

/// An inclusive byte range within a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte.
    pub start: u64,
    /// Last byte, inclusive; at least `start` and less than the file length.
    pub end: u64,
}

impl ByteRange {
    /// The number of bytes in the range.
    #[must_use]
    pub const fn len(self) -> u64 {
        // `end >= start` by construction, so this cannot fail; saturate anyway.
        self.end.saturating_sub(self.start).saturating_add(1)
    }

    /// Always false: a range holds at least one byte.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }
}

/// What to answer for a `Range` header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeAnswer {
    /// Send the whole file with 200: no header, a header that does not parse, a unit other than
    /// bytes, or several ranges (which the host does not split into multipart answers).
    Full,
    /// Send this part with 206.
    Partial(ByteRange),
    /// Answer 416 with `Content-Range: bytes */<len>`.
    Unsatisfiable,
}

/// Interprets a `Range` header value for a file of `len` bytes.
#[must_use]
pub fn parse_range(value: &str, len: u64) -> RangeAnswer {
    let value = value.trim();
    let Some((unit, spec)) = value.split_once('=') else {
        return RangeAnswer::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return RangeAnswer::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeAnswer::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    let Some(final_byte) = len.checked_sub(1) else {
        // Nothing in an empty file can be selected, but a malformed header is still ignored.
        return if (first.is_empty() && last.is_empty())
            || (!first.is_empty() && digits(first).is_none())
            || (!last.is_empty() && digits(last).is_none())
        {
            RangeAnswer::Full
        } else {
            RangeAnswer::Unsatisfiable
        };
    };
    match (first.is_empty(), last.is_empty()) {
        (true, true) => RangeAnswer::Full,
        (true, false) => match digits(last) {
            None => RangeAnswer::Full,
            Some(0) => RangeAnswer::Unsatisfiable,
            Some(suffix) => RangeAnswer::Partial(ByteRange {
                start: len.saturating_sub(suffix),
                end: final_byte,
            }),
        },
        (false, true) => match digits(first) {
            None => RangeAnswer::Full,
            Some(start) if start > final_byte => RangeAnswer::Unsatisfiable,
            Some(start) => RangeAnswer::Partial(ByteRange {
                start,
                end: final_byte,
            }),
        },
        (false, false) => match (digits(first), digits(last)) {
            (Some(start), Some(end)) if start <= end => {
                if start > final_byte {
                    RangeAnswer::Unsatisfiable
                } else {
                    RangeAnswer::Partial(ByteRange {
                        start,
                        end: end.min(final_byte),
                    })
                }
            }
            _ => RangeAnswer::Full,
        },
    }
}

/// A non-empty run of ASCII digits as a number (`u64::from_str` would also take a `+`).
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(start: u64, end: u64) -> RangeAnswer {
        RangeAnswer::Partial(ByteRange { start, end })
    }

    #[test]
    fn closed_ranges() {
        assert_eq!(parse_range("bytes=0-0", 10), part(0, 0));
        assert_eq!(parse_range("bytes=2-5", 10), part(2, 5));
        assert_eq!(parse_range("bytes=2-500", 10), part(2, 9));
        assert_eq!(parse_range(" Bytes = 2 - 5 ", 10), part(2, 5));
        assert_eq!(parse_range("bytes=10-20", 10), RangeAnswer::Unsatisfiable);
        assert_eq!(parse_range("bytes=5-2", 10), RangeAnswer::Full);
    }

    #[test]
    fn open_ranges() {
        assert_eq!(parse_range("bytes=3-", 10), part(3, 9));
        assert_eq!(parse_range("bytes=9-", 10), part(9, 9));
        assert_eq!(parse_range("bytes=10-", 10), RangeAnswer::Unsatisfiable);
    }

    #[test]
    fn suffix_ranges() {
        assert_eq!(parse_range("bytes=-3", 10), part(7, 9));
        assert_eq!(parse_range("bytes=-30", 10), part(0, 9));
        assert_eq!(parse_range("bytes=-0", 10), RangeAnswer::Unsatisfiable);
    }

    #[test]
    fn empty_files_have_nothing_to_select() {
        assert_eq!(parse_range("bytes=0-", 0), RangeAnswer::Unsatisfiable);
        assert_eq!(parse_range("bytes=-5", 0), RangeAnswer::Unsatisfiable);
        assert_eq!(parse_range("bytes=x-", 0), RangeAnswer::Full);
    }

    #[test]
    fn other_headers_get_the_whole_file() {
        for value in [
            "",
            "bytes",
            "bytes=",
            "bytes=-",
            "bytes=a-b",
            "bytes=+1-2",
            "bytes=0-1,4-5",
            "items=0-1",
            "bytes=99999999999999999999999-",
        ] {
            assert_eq!(parse_range(value, 10), RangeAnswer::Full, "{value}");
        }
    }

    #[test]
    fn lengths_count_both_ends() {
        assert_eq!(ByteRange { start: 2, end: 5 }.len(), 4);
        assert_eq!(ByteRange { start: 0, end: 0 }.len(), 1);
    }
}
