// SPDX-License-Identifier: AGPL-3.0-only
//! An ordered JSON value and the one layout every file in `vectors/` uses: two-space
//! indentation, `": "` between key and value, keys in the order written, non-ASCII text left
//! as UTF-8, and a final newline (`vectors/README.md`).
//!
//! The layout is spelled out here, rather than taken from `serde_json`, because the files must
//! come out byte for byte the same as the ones already committed: object keys keep their
//! written order (`serde_json` sorts them without its `preserve_order` feature, which would
//! change every other crate's maps through feature unification), and floats are written in
//! their shortest round-trip form with the exponent style the committed files use
//! (`1e-05`, `1e+16`).

use std::fmt::Write as _;

/// A JSON value whose objects keep their keys in the order they were built.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    /// An integer: bucket indexes, lengths, timestamps.
    Integer(i64),
    /// A floating-point number: coordinates, distances, bearings.
    Float(f64),
    /// Text, including lower-case hex for bytes.
    Text(String),
    /// A list.
    Array(Vec<Json>),
    /// Key and value pairs, in order.
    Object(Vec<(String, Json)>),
}

impl From<i64> for Json {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<Json>> for Json {
    fn from(value: Vec<Json>) -> Self {
        Self::Array(value)
    }
}

/// `object! { "key": value, ... }`: a [`Json::Object`] with its keys in the order written.
/// Each value goes through `Json::from`, so integers, floats, text and nested objects mix.
macro_rules! object {
    ($($key:literal : $value:expr),* $(,)?) => {
        $crate::vectors::json::Json::Object(vec![
            $(($key.to_owned(), $crate::vectors::json::Json::from($value))),*
        ])
    };
}
pub(crate) use object;

impl Json {
    /// The value laid out as a whole file: indented, with a final newline.
    pub(crate) fn to_file_text(&self) -> String {
        let mut text = String::new();
        self.write(&mut text, 0);
        text.push('\n');
        text
    }

    fn write(&self, text: &mut String, depth: usize) {
        match self {
            Self::Integer(value) => {
                // Writing to a String cannot fail.
                let _infallible = write!(text, "{value}");
            }
            Self::Float(value) => text.push_str(&float_text(*value)),
            Self::Text(value) => write_string(text, value),
            Self::Array(items) => {
                let members = items
                    .iter()
                    .map(|item| {
                        let mut member = String::new();
                        item.write(&mut member, depth.saturating_add(1));
                        member
                    })
                    .collect();
                write_container(text, depth, ['[', ']'], members);
            }
            Self::Object(pairs) => {
                let members = pairs
                    .iter()
                    .map(|(key, value)| {
                        let mut member = String::new();
                        write_string(&mut member, key);
                        member.push_str(": ");
                        value.write(&mut member, depth.saturating_add(1));
                        member
                    })
                    .collect();
                write_container(text, depth, ['{', '}'], members);
            }
        }
    }
}

/// Members already laid out for `depth + 1`, one per line, indented one level deeper than the
/// brackets; `[]` or `{}` when there are none.
fn write_container(
    text: &mut String,
    depth: usize,
    [open, close]: [char; 2],
    members: Vec<String>,
) {
    text.push(open);
    if !members.is_empty() {
        let inner = depth.saturating_add(1);
        for (index, member) in members.into_iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push('\n');
            indent(text, inner);
            text.push_str(&member);
        }
        text.push('\n');
        indent(text, depth);
    }
    text.push(close);
}

fn indent(text: &mut String, depth: usize) {
    for _ in 0..depth {
        text.push_str("  ");
    }
}

/// A JSON string with only the escapes JSON requires: quote, backslash and control characters.
/// Everything else, non-ASCII included, is written as it is.
fn write_string(text: &mut String, value: &str) {
    text.push('"');
    for character in value.chars() {
        match character {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\u{8}' => text.push_str("\\b"),
            '\u{c}' => text.push_str("\\f"),
            control if u32::from(control) < 0x20 => {
                // Writing to a String cannot fail.
                let _infallible = write!(text, "\\u{:04x}", u32::from(control));
            }
            other => text.push(other),
        }
    }
    text.push('"');
}

/// The shortest text that reads back as the same float, always with a `.0` or an exponent so
/// it stays a float: positional between 1e-4 and 1e16, otherwise `<digits>e<sign><at least two
/// digits>`. Rust's `Debug` already picks the shortest digits and the same switch-over points;
/// only its exponent is spelled differently (`1e-5`, `1e16`).
pub(crate) fn float_text(value: f64) -> String {
    let debug = format!("{value:?}");
    let Some((mantissa, exponent)) = debug.split_once('e') else {
        return debug;
    };
    let (sign, digits) = exponent
        .strip_prefix('-')
        .map_or(('+', exponent), |digits| ('-', digits));
    format!("{mantissa}e{sign}{digits:0>2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_keep_a_fraction_or_an_exponent() {
        assert_eq!(float_text(0.0), "0.0");
        assert_eq!(float_text(-0.0), "-0.0");
        assert_eq!(float_text(90.0), "90.0");
        assert_eq!(float_text(51.5033), "51.5033");
        assert_eq!(float_text(0.0001), "0.0001");
        assert_eq!(float_text(1e-5), "1e-05");
        assert_eq!(float_text(1.5e-300), "1.5e-300");
        assert_eq!(float_text(1e15), "1000000000000000.0");
        assert_eq!(float_text(1e16), "1e+16");
        assert_eq!(float_text(2.5e123), "2.5e+123");
    }

    #[test]
    fn strings_escape_only_what_json_requires() {
        let mut text = String::new();
        write_string(&mut text, "a\"b\\c\nd\r\t\u{8}\u{c}\u{1}\u{7f}é☕/");
        assert_eq!(text, "\"a\\\"b\\\\c\\nd\\r\\t\\b\\f\\u0001\u{7f}é☕/\"");
    }

    #[test]
    fn layout_indents_two_spaces_and_keeps_key_order() {
        let value = object! {
            "z": 1_i64,
            "a": vec![Json::from("x"), object! {}],
            "empty": Vec::<Json>::new(),
            "float": 2.5,
        };
        assert_eq!(
            value.to_file_text(),
            "{\n  \"z\": 1,\n  \"a\": [\n    \"x\",\n    {}\n  ],\n  \"empty\": [],\n  \"float\": 2.5\n}\n"
        );
    }

    #[test]
    fn negative_integers_print_with_their_sign() {
        assert_eq!(Json::from(-338_688_000_i64).to_file_text(), "-338688000\n");
    }
}
