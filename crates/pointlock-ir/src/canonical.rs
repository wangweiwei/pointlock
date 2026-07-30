//! JCS-style canonical JSON serialization — the byte form all IR hashes are
//! computed over (02 §12.1).
//!
//! Of the five canonical-form rules of 02 §12.1, this module implements
//! rule 1 (serialization): object members are sorted by ascending UTF-16
//! code units (RFC 8785 §3.2.3) and no insignificant whitespace is emitted.
//! String escaping follows RFC 8785 §3.2.2.2 — shorthand escapes for the
//! two-character sequences (`\"` `\\` `\b` `\t` `\n` `\f` `\r`), lowercase
//! `\u00xx` for the remaining control characters, everything else verbatim
//! UTF-8 — which is exactly the form serde_json's escaper produces.
//!
//! ## Documented divergence: number rendering
//!
//! RFC 8785 renders numbers in the ES `Number::toString` shortest form.
//! This module instead uses serde_json's default number formatting (the
//! decided simplification for this milestone):
//!
//! - integers print exactly as parsed (`1`, never `1.0`);
//! - floats go through ryu shortest-form printing, which matches ES for the
//!   finite doubles the IR admits (coordinates, backoff), except that
//!   integral doubles keep a trailing `.0` (`1.0` where JCS prints `1`);
//! - u64/i64 beyond 2^53 print exactly (JCS forbids them; the IR's integer
//!   domains are counts/budgets/line numbers far below that bound).
//!
//! This is deterministic for all IR produced and consumed through
//! serde_json — the only path in Pointlock (spine R12): a JSON number keeps
//! its `serde_json::Number` representation from parse to hash, so the same
//! sealed artifact always hashes identically. Byte-level interoperability
//! with third-party RFC 8785 implementations is explicitly out of scope for
//! v0.1. `NaN`/`Infinity` are not JSON and are unrepresentable here.
//!
//! The remaining rules are properties of sealed IR that this module assumes
//! rather than enforces: rule 2 (Unicode NFC) is the compiler's duty before
//! `seal`; rules 3–5 (materialized defaults, absence-by-omission / no null
//! for absence, semantic array order) are guaranteed by the DTO layer's
//! serde discipline.

use std::cmp::Ordering;

use serde_json::Value;

/// Serializes `value` to its canonical JSON form (02 §12.1 rule 1): object
/// members sorted by ascending UTF-16 code units, no whitespace, RFC 8785
/// string escaping, serde_json default number formatting (see the module
/// docs for the documented divergence from full JCS number rendering).
pub fn to_canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
    }
}

/// RFC 8785 §3.2.3 member ordering: keys compare as sequences of UTF-16
/// code units. This differs from Rust's `str` ordering (UTF-8 bytes, i.e.
/// code points) whenever supplementary-plane characters mix with
/// U+E000..=U+FFFF, so it must not be replaced by `str::cmp`.
fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// JSON string escaping per RFC 8785 §3.2.2.2. serde_json's escaper emits
/// exactly the canonical form (shorthand escapes, lowercase `\u00xx` for the
/// remaining control characters, nothing else escaped), so it is reused
/// verbatim.
fn write_json_string(s: &str, out: &mut String) {
    out.push_str(&serde_json::to_string(s).expect("serializing a string to JSON cannot fail"));
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::to_canonical_json;

    #[test]
    fn sorts_keys_and_strips_whitespace() {
        let v: Value = serde_json::from_str(r#"{ "b" : 2, "a" : [1, {"y": 0, "x": 1}] }"#)
            .expect("valid JSON");
        assert_eq!(to_canonical_json(&v), r#"{"a":[1,{"x":1,"y":0}],"b":2}"#);
    }

    #[test]
    fn sorts_keys_by_utf16_code_units_not_code_points() {
        // U+1F600 encodes as the surrogate pair D83D DE00, which sorts BEFORE
        // U+FF61 in UTF-16 even though its code point (and UTF-8 bytes) are
        // larger. BTreeMap iteration order (UTF-8) yields U+FF61 first, so
        // this asserts the canonicalizer re-sorts.
        let mut map = serde_json::Map::new();
        map.insert("\u{ff61}".to_owned(), json!(1));
        map.insert("\u{1f600}".to_owned(), json!(2));
        assert_eq!(
            to_canonical_json(&Value::Object(map)),
            "{\"\u{1f600}\":2,\"\u{ff61}\":1}"
        );
    }

    #[test]
    fn escapes_strings_canonically() {
        // Shorthand escapes, lowercase \u00xx for other control characters,
        // non-ASCII verbatim (RFC 8785 §3.2.2.2).
        let v = json!({ "k": "a\"b\\c\n\u{0007}€" });
        assert_eq!(to_canonical_json(&v), "{\"k\":\"a\\\"b\\\\c\\n\\u0007€\"}");
    }

    #[test]
    fn numbers_use_serde_json_default_formatting() {
        assert_eq!(to_canonical_json(&json!([1, -3, 1.5])), "[1,-3,1.5]");
        // Documented divergence from RFC 8785: an integral double keeps its
        // ".0" (JCS would print "1"). Deterministic within serde_json.
        let v: Value = serde_json::from_str("1.0").expect("valid JSON");
        assert_eq!(to_canonical_json(&v), "1.0");
    }
}
