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
    strict(value)?;
    serde_json::to_vec(&canonical_node(value)).map_err(|e| bad_request(e.to_string()))
}

/// Structural equality in the validity-hash domain. Object order is irrelevant;
/// arrays retain order, integers differ from floats and float bits must match.
/// Equal exactly when their `canonical_json` encodings are, and refused as that refuses,
/// but compared in place: a whole plan document is compared without encoding it twice.
pub fn data_equal(left: &Value, right: &Value) -> Result<bool, PublicError> {
    strict(left)?;
    strict(right)?;
    Ok(same(left, right))
}
fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => match (a.is_f64(), b.is_f64()) {
            (true, true) => a.as_f64().map(f64::to_bits) == b.as_f64().map(f64::to_bits),
            (false, false) => a.as_i64() == b.as_i64(),
            _ => false,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same(a, b)))
        }
        _ => false,
    }
}
/// `data_equal` of two maps as objects, without building either object.
pub fn data_equal_maps(left: &JsonMap, right: &JsonMap) -> Result<bool, PublicError> {
    let valid = |map: &JsonMap| crate::types::validate_json_map(map, "inputs").is_ok();
    if !valid(left) || !valid(right) {
        return data_equal(
            &serde_json::to_value(left).map_err(|e| bad_request(e.to_string()))?,
            &serde_json::to_value(right).map_err(|e| bad_request(e.to_string()))?,
        );
    }
    Ok(left.0.len() == right.0.len()
        && left.0.iter().all(|(key, value)| {
            right
                .0
                .get(key)
                .is_some_and(|other| same(value.as_value(), other.as_value()))
        }))
}
fn strict(value: &Value) -> Result<(), PublicError> {
    validate_json(value, "inputs").map_err(|errors| {
        bad_request(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })
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

/// One input as `canonical_json` encodes it: a strict value, or a file binding as the
/// object `{"file": path}`.
enum Canonical<'a> {
    Value(&'a Value),
    File(&'a str),
}
/// Feeds SHA-256 what is written to it.
struct Hashing(Sha256);
impl std::io::Write for Hashing {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
/// Write `canonical_json`'s bytes for a strict value without building its tagged tree;
/// strings and keys go through serde_json, so they are escaped as it escapes them.
fn write_canonical(out: &mut impl std::io::Write, value: &Value) -> std::io::Result<()> {
    match value {
        Value::Null => out.write_all(br#"["null"]"#),
        Value::Bool(true) => out.write_all(br#"["boolean",true]"#),
        Value::Bool(false) => out.write_all(br#"["boolean",false]"#),
        Value::String(s) => {
            out.write_all(br#"["string","#)?;
            serde_json::to_writer(&mut *out, s)?;
            out.write_all(b"]")
        }
        Value::Number(n) if n.is_f64() => write!(
            out,
            r#"["float","{:016x}"]"#,
            n.as_f64().expect("strict finite float").to_bits()
        ),
        Value::Number(n) => write!(out, r#"["int","{}"]"#, n.as_i64().expect("strict i64")),
        Value::Array(items) => {
            out.write_all(br#"["array",["#)?;
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.write_all(b",")?;
                }
                write_canonical(out, item)?;
            }
            out.write_all(b"]]")
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            write_object(
                out,
                keys.into_iter().map(|key| (key, &map[key])),
                |out, value| write_canonical(out, value),
            )
        }
    }
}
/// `["object",{...}]` over entries already in key order.
fn write_object<'a, T: 'a, W: std::io::Write>(
    out: &mut W,
    entries: impl Iterator<Item = (&'a String, T)>,
    mut value: impl FnMut(&mut W, T) -> std::io::Result<()>,
) -> std::io::Result<()> {
    out.write_all(br#"["object",{"#)?;
    for (index, (key, item)) in entries.enumerate() {
        if index > 0 {
            out.write_all(b",")?;
        }
        serde_json::to_writer(&mut *out, key)?;
        out.write_all(b":")?;
        value(out, item)?;
    }
    out.write_all(b"}]")
}

impl InputsHash {
    /// The digest of strict inputs, streamed: the bytes `of` hashes, never held whole.
    fn digest<'a>(inputs: impl Iterator<Item = (&'a String, Canonical<'a>)>) -> Self {
        let mut inputs: Vec<_> = inputs.collect();
        inputs.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = Hashing(Sha256::new());
        let written: std::io::Result<()> = (|| {
            use std::io::Write;
            write!(out, "{{\"format\":\"{INPUTS_HASH_FORMAT}\",\"inputs\":")?;
            write_object(&mut out, inputs.into_iter(), |out, input| match input {
                Canonical::Value(value) => write_canonical(out, value),
                Canonical::File(path) => {
                    let key = "file".to_owned();
                    write_object(out, std::iter::once((&key, path)), |out, path| {
                        out.write_all(br#"["string","#)?;
                        serde_json::to_writer(&mut *out, path)?;
                        out.write_all(b"]")
                    })
                }
            })?;
            out.write_all(b"}")
        })();
        written.expect("hashing never fails");
        Self(out.0.finalize().into())
    }
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
        if inputs
            .0
            .values()
            .all(|value| crate::types::strict(value.as_value(), 1))
        {
            return Ok(Self::digest(
                inputs
                    .0
                    .iter()
                    .map(|(name, value)| (name, Canonical::Value(value.as_value()))),
            ));
        }
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
        if bindings.values().all(|binding| match binding {
            EffectiveInput::Value(value) => crate::types::strict(value.as_value(), 1),
            EffectiveInput::File(_) => true,
        }) {
            return Ok(Self::digest(bindings.iter().map(|(name, binding)| {
                (
                    name,
                    match binding {
                        EffectiveInput::Value(value) => Canonical::Value(value.as_value()),
                        EffectiveInput::File(path) => Canonical::File(path),
                    },
                )
            })));
        }
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
