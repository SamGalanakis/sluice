//! Validity hashes of effective data and separate execution byte provenance.

use crate::{
    error::PublicError,
    rpc::{JsonMap, JsonValue},
    types::validate_json,
};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, str::FromStr};

/// Version 1 is a new hash domain, with no Python hash compatibility.
pub const INPUTS_HASH_FORMAT: &str = "sluice-inputs-v1";

fn bad_request(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn hex(bytes: &[u8; 32], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}
fn parse_digest(s: &str) -> Result<[u8; 32], PublicError> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad_request(
            "expected a 64-character lowercase SHA-256 hex digest",
        ));
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| bad_request(e.to_string()))?;
    }
    Ok(bytes)
}

macro_rules! digest_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(#[schemars(with = "String")] [u8; 32]);
        impl $name {
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                hex(&self.0, f)
            }
        }
        impl FromStr for $name {
            type Err = PublicError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(parse_digest(s)?))
            }
        }
        impl TryFrom<String> for $name {
            type Error = PublicError;
            fn try_from(s: String) -> Result<Self, Self::Error> {
                s.parse()
            }
        }
        impl From<$name> for String {
            fn from(digest: $name) -> String {
                digest.to_string()
            }
        }
    };
}

digest_type!(InputsHash);
digest_type!(ExecutionProvenance);

/// One effective data binding. Literals, defaults, plan inputs and handoffs all
/// use Value after resolution. A file contributes its path only. The plan
/// compiler validates absolute file paths; this module does not read files.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectiveInput {
    Value(JsonValue),
    File(String),
}

/// Encode a strict JSON value as a compact tagged JSON tree, without a version
/// envelope. Every node is an array: `["null"]`, `["boolean",bool]`,
/// `["string",string]`, `["int","decimal i64"]`,
/// `["float","16 lowercase hexadecimal digits of the IEEE-754 binary64 bits"]`,
/// `["array",[nodes...]]`, or `["object",{key:node,...}]`.
/// Object keys sort lexicographically by their UTF-8 bytes at every depth.
/// Strings and keys use JSON escaping: quote and backslash become `\"` and
/// `\\`; U+0008, U+0009, U+000A, U+000C and U+000D become `\b`, `\t`, `\n`,
/// `\f` and `\r`. Other U+0000..U+001F characters become `\u00xx` with lowercase
/// hex digits. Every other character stays literal UTF-8, including `/`.
/// No whitespace or trailing newline is emitted. Arrays retain order. Integers
/// use minimal base-10 spelling; float payloads include leading zeroes. Thus
/// int 1 differs from float 1.0, positive zero differs from negative zero, and
/// equivalent float lexemes such as 1.0 and 1e0 have the same representation.
/// All nodes are tagged, so user objects cannot collide with numeric encodings.
/// Duplicate keys must be refused by `rpc::decode_json` before constructing Value.
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, PublicError> {
    validate_json(value, "inputs").map_err(|errors| {
        bad_request(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    serde_json::to_vec(&canonical_node(value)).map_err(|e| bad_request(e.to_string()))
}

/// Structural equality in the validity-hash domain. Object order is irrelevant;
/// arrays retain order, integers differ from floats and float bits must match.
pub fn data_equal(left: &Value, right: &Value) -> Result<bool, PublicError> {
    Ok(canonical_json(left)? == canonical_json(right)?)
}
fn canonical_node(value: &Value) -> Value {
    match value {
        Value::Null => json!(["null"]),
        Value::Bool(b) => json!(["boolean", b]),
        Value::String(s) => json!(["string", s]),
        Value::Number(n) if n.is_f64() => json!([
            "float",
            format!(
                "{:016x}",
                n.as_f64().expect("strict finite float").to_bits()
            )
        ]),
        Value::Number(n) => json!(["int", n.as_i64().expect("strict i64").to_string()]),
        Value::Array(a) => json!(["array", a.iter().map(canonical_node).collect::<Vec<_>>()]),
        Value::Object(o) => json!([
            "object",
            o.iter()
                .map(|(k, v)| (k, canonical_node(v)))
                .collect::<BTreeMap<_, _>>()
        ]),
    }
}

impl InputsHash {
    /// SHA-256 over these exact UTF-8 bytes:
    /// `{"format":"sluice-inputs-v1","inputs":<canonical tagged object>}`.
    /// The object is the effective bound data, encoded by `canonical_json`.
    /// Every unbound optional input is absent, not null. A bound null remains
    /// present. File bindings are ordinary objects `{"file":"<path>"}` before
    /// tagging. Gates, fn source, declaration metadata and file contents never
    /// enter this map. The digest displays/serializes as 64 lowercase hex digits.
    /// Unknown data is represented by the caller as `Option<InputsHash>::None`,
    /// never by hashing an incomplete effective-input map.
    ///
    /// ```
    /// use sluice_model::{hash::InputsHash, rpc::decode_json};
    /// let a = decode_json(br#"{"x":1,"y":true}"#).unwrap();
    /// let b = decode_json(br#"{"y":true,"x":1}"#).unwrap();
    /// assert_eq!(InputsHash::of(&a).unwrap(), InputsHash::of(&b).unwrap());
    /// ```
    pub fn of(inputs: &JsonMap) -> Result<Self, PublicError> {
        let value = Value::Object(
            inputs
                .0
                .iter()
                .map(|(name, value)| (name.clone(), value.as_value().clone()))
                .collect(),
        );
        let canonical = canonical_json(&value)?;
        let mut bytes = format!("{{\"format\":\"{INPUTS_HASH_FORMAT}\",\"inputs\":").into_bytes();
        bytes.extend(canonical);
        bytes.push(b'}');
        Ok(Self(Sha256::digest(bytes).into()))
    }

    /// Hash only supplied resolved data bindings. Do not populate this map from
    /// fn declarations: an unbound optional input has no entry.
    pub fn from_bindings(bindings: &IndexMap<String, EffectiveInput>) -> Result<Self, PublicError> {
        let inputs = JsonMap(
            bindings
                .iter()
                .map(|(name, binding)| {
                    let value = match binding {
                        EffectiveInput::Value(value) => value.clone(),
                        EffectiveInput::File(path) => JsonValue::try_from(json!({"file":path}))?,
                    };
                    Ok((name.clone(), value))
                })
                .collect::<Result<_, PublicError>>()?,
        );
        Self::of(&inputs)
    }

    /// External JSON ingress always passes through the strict shared decoder.
    pub fn parse_json(bytes: &[u8]) -> Result<Self, PublicError> {
        Self::of(&crate::rpc::decode_json(bytes)?)
    }
}

impl ExecutionProvenance {
    /// SHA-256 of the exact frozen file bytes, with no JSON encoding, decoding,
    /// path or validity hash. Record alongside the path in the execution request.
    /// This fingerprint must never be substituted for InputsHash.
    pub fn fingerprint(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}
