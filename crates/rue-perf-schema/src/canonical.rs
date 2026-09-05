//! Key-sorted serialization, content addressing, and the reporting form.
//!
//! A run object's name is the SHA-256 digest of its canonical form. For that
//! name to be stable, serialization must be a function of the value alone:
//! object keys sorted, no insignificant whitespace, and no floating-point
//! numbers anywhere. The float rule is the reason raw records hold integer
//! nanoseconds and integer bytes — hashing must never depend on how some
//! writer chose to format `0.1`. Reports that are read rather than hashed —
//! the `--benchmark-json` envelope above all — keep the sorted-key shape but
//! may carry floats, and take [`sorted_json`] instead.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Why a value could not be canonically serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    /// The value contained a floating-point number.
    ///
    /// Raw records are integers by construction. A float here means a derived
    /// statistic leaked into storage, which is the thing this rule prevents.
    FloatingPoint {
        /// JSON pointer to the offending value, for example `/workloads/0/x`.
        path: String,
    },
    /// The value could not be represented as JSON at all.
    NotSerializable {
        /// The underlying serialization error, rendered.
        detail: String,
    },
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalError::FloatingPoint { path } => {
                write!(f, "floating-point value at {path} is not canonicalizable")
            }
            CanonicalError::NotSerializable { detail } => {
                write!(f, "value is not serializable as JSON: {detail}")
            }
        }
    }
}

impl std::error::Error for CanonicalError {}

/// Serialize a value to canonical JSON.
///
/// Object keys are sorted by their Unicode scalar sequence, no whitespace is
/// emitted, and any floating-point number is an error rather than a rounding
/// decision.
pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<String, CanonicalError> {
    let value = serde_json::to_value(value).map_err(|error| CanonicalError::NotSerializable {
        detail: error.to_string(),
    })?;
    let mut out = String::new();
    write_sorted(&value, "", &mut out, Floats::Rejected)?;
    Ok(out)
}

/// Serialize a value to key-sorted compact JSON, floating-point numbers
/// included.
///
/// This is the shape a published report takes on the wire: the same object
/// keys in the same order whatever a producer's field declaration order
/// happens to be, so a consumer diffing two reports sees only real changes.
/// Unlike [`canonical_json`] it is not a content-addressing form — a float's
/// text is a formatting decision, so a digest over these bytes would not be a
/// function of the value alone.
pub fn sorted_json<T: Serialize + ?Sized>(value: &T) -> Result<String, CanonicalError> {
    let value = serde_json::to_value(value).map_err(|error| CanonicalError::NotSerializable {
        detail: error.to_string(),
    })?;
    let mut out = String::new();
    write_sorted(&value, "", &mut out, Floats::Permitted)?;
    Ok(out)
}

/// The content address of a value: SHA-256 of its canonical JSON, in lowercase
/// hexadecimal.
///
/// This is the filename a run object is stored under. Equal addresses mean
/// byte-identical canonical forms, so a run object can be verified against its
/// own name without trusting whoever wrote it.
pub fn content_address<T: Serialize + ?Sized>(value: &T) -> Result<String, CanonicalError> {
    let canonical = canonical_json(value)?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Whether a writer may emit a floating-point number or must refuse one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Floats {
    /// Content addressing: a float is an error, located by JSON pointer.
    Rejected,
    /// Reporting: a float is written exactly as `serde_json` would write it.
    Permitted,
}

fn write_sorted(
    value: &serde_json::Value,
    path: &str,
    out: &mut String,
    floats: Floats,
) -> Result<(), CanonicalError> {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        serde_json::Value::Number(number) => {
            if let Some(unsigned) = number.as_u64() {
                out.push_str(&unsigned.to_string());
            } else if let Some(signed) = number.as_i64() {
                out.push_str(&signed.to_string());
            } else if floats == Floats::Permitted {
                // `Number`'s `Display` is the serializer's own float
                // formatting, so a permitted float reads exactly as
                // `serde_json` would have written it.
                out.push_str(&number.to_string());
            } else {
                return Err(CanonicalError::FloatingPoint {
                    path: if path.is_empty() {
                        "/".to_string()
                    } else {
                        path.to_string()
                    },
                });
            }
        }
        // Delegating string escaping to serde_json keeps this writer honest
        // about control characters, surrogate pairs, and quoting.
        serde_json::Value::String(text) => {
            let encoded =
                serde_json::to_string(text).map_err(|error| CanonicalError::NotSerializable {
                    detail: error.to_string(),
                })?;
            out.push_str(&encoded);
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_sorted(item, &format!("{path}/{index}"), out, floats)?;
            }
            out.push(']');
        }
        serde_json::Value::Object(entries) => {
            // serde_json's map is ordered by key under the default feature set,
            // but sorting explicitly means the canonical form does not depend
            // on which features a downstream build happens to enable.
            let mut keys: Vec<&String> = entries.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let encoded = serde_json::to_string(key).map_err(|error| {
                    CanonicalError::NotSerializable {
                        detail: error.to_string(),
                    }
                })?;
                out.push_str(&encoded);
                out.push(':');
                let child = entries.get(key).expect("key came from this map");
                write_sorted(child, &format!("{path}/{key}"), out, floats)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_keys_are_sorted_regardless_of_insertion_order() {
        let forward = canonical_json(&json!({"a": 1, "b": 2, "c": 3})).unwrap();
        let reverse = canonical_json(&json!({"c": 3, "b": 2, "a": 1})).unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(forward, r#"{"a":1,"b":2,"c":3}"#);
    }

    #[test]
    fn nested_objects_are_sorted_at_every_depth() {
        let canonical =
            canonical_json(&json!({"z": {"y": 1, "x": 2}, "a": [{"n": 1, "m": 2}]})).unwrap();
        assert_eq!(canonical, r#"{"a":[{"m":2,"n":1}],"z":{"x":2,"y":1}}"#);
    }

    #[test]
    fn no_insignificant_whitespace_is_emitted() {
        let canonical = canonical_json(&json!({"a": [1, 2], "b": {"c": "d"}})).unwrap();
        assert!(!canonical.contains(' '));
        assert!(!canonical.contains('\n'));
    }

    #[test]
    fn strings_are_escaped_like_json() {
        let canonical = canonical_json(&json!({"k": "a\"b\\c\nd"})).unwrap();
        assert_eq!(canonical, r#"{"k":"a\"b\\c\nd"}"#);
    }

    #[test]
    fn control_characters_round_trip_through_the_canonical_form() {
        // Round-tripping proves the escaping is real JSON, not an approximation.
        let awkward = "tab\there\u{1}\u{7f}\u{2028}end";
        let canonical = canonical_json(&json!({"k": awkward})).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(parsed["k"], awkward);
    }

    #[test]
    fn non_ascii_keys_sort_by_scalar_sequence() {
        let canonical = canonical_json(&json!({"é": 1, "e": 2, "z": 3})).unwrap();
        assert_eq!(canonical, "{\"e\":2,\"z\":3,\"\u{e9}\":1}");
    }

    #[test]
    fn the_reporting_form_sorts_keys_and_keeps_floats() {
        let report = sorted_json(&json!({"b": 0.5, "a": {"d": 1, "c": [2.25]}})).unwrap();
        assert_eq!(report, r#"{"a":{"c":[2.25],"d":1},"b":0.5}"#);
    }

    #[test]
    fn the_reporting_form_writes_floats_as_serde_json_does() {
        // The wire bytes must not depend on which writer produced them.
        let value = json!({"a": 0.1, "b": 1.0, "c": 1e300, "d": -0.0});
        assert_eq!(
            sorted_json(&value).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
    }

    #[test]
    fn floats_are_rejected_with_their_location() {
        let error = canonical_json(&json!({"outer": [{"inner": 0.5}]})).unwrap_err();
        assert_eq!(
            error,
            CanonicalError::FloatingPoint {
                path: "/outer/0/inner".to_string()
            }
        );
    }

    #[test]
    fn negative_and_large_integers_survive() {
        let canonical = canonical_json(&json!({"a": -7, "b": u64::MAX})).unwrap();
        assert_eq!(canonical, format!(r#"{{"a":-7,"b":{}}}"#, u64::MAX));
    }

    #[test]
    fn content_address_is_sha256_of_the_canonical_bytes() {
        let value = json!({"b": 2, "a": 1});
        let expected = {
            let digest = Sha256::digest(r#"{"a":1,"b":2}"#.as_bytes());
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(content_address(&value).unwrap(), expected);
        assert_eq!(content_address(&value).unwrap().len(), 64);
    }

    #[test]
    fn content_address_ignores_key_order_but_not_content() {
        let one = content_address(&json!({"a": 1, "b": 2})).unwrap();
        let same = content_address(&json!({"b": 2, "a": 1})).unwrap();
        let different = content_address(&json!({"a": 1, "b": 3})).unwrap();
        assert_eq!(one, same);
        assert_ne!(one, different);
    }
}
