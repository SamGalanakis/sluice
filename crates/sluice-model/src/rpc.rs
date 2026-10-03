use crate::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::*,
};
use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::{borrow::Cow, fmt};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// JSON data with duplicate keys, unsigned overflow and non-finite floats refused.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct JsonValue(Value);
impl JsonValue {
    pub fn as_value(&self) -> &Value {
        &self.0
    }
    pub fn into_value(self) -> Value {
        self.0
    }
}
impl TryFrom<Value> for JsonValue {
    type Error = PublicError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        fn valid(v: &Value) -> bool {
            match v {
                Value::Number(n) => {
                    if n.is_f64() {
                        n.as_f64().is_some_and(f64::is_finite)
                    } else {
                        n.as_i64().is_some()
                    }
                }
                Value::Array(a) => a.iter().all(valid),
                Value::Object(o) => o.values().all(valid),
                _ => true,
            }
        }
        if valid(&value) {
            Ok(Self(value))
        } else {
            Err(PublicError::BadRequest {
                message: "JSON integers must fit i64 and floats must be finite".into(),
            })
        }
    }
}
impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = JsonValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("strict JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(JsonValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(JsonValue(Value::Number(v.into())))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                self.visit_i64(i64::try_from(v).map_err(E::custom)?)
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(JsonValue(Value::Number(
                    Number::from_f64(v).ok_or_else(|| E::custom("non-finite JSON number"))?,
                )))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(JsonValue(Value::String(v.into())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(JsonValue(Value::String(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(JsonValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element::<JsonValue>()? {
                    values.push(v.0);
                }
                Ok(JsonValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate JSON key: {key}"
                        )));
                    }
                    values.insert(key, a.next_value::<JsonValue>()?.0);
                }
                Ok(JsonValue(Value::Object(values)))
            }
        }
        d.deserialize_any(StrictVisitor)
    }
}
impl JsonSchema for JsonValue {
    fn schema_name() -> Cow<'static, str> {
        "JsonValue".into()
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        let child = g.subschema_for::<Self>();
        json_schema!({"x-sluice-integer-lexemes":"i64", "x-sluice-floats":"finite", "anyOf":[{"type":"null"},{"type":"boolean"},{"type":"string"},
            {"type":"integer","minimum":i64::MIN,"maximum":i64::MAX},
            {"type":"number"}, {"type":"array","items":child},
            {"type":"object","additionalProperties":child}]})
    }
}

/// Insertion-ordered object. Decode external requests through `decode_json`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct JsonMap(
    #[schemars(with = "std::collections::BTreeMap<String, JsonValue>")]
    pub  IndexMap<String, JsonValue>,
);

fn check_integer_tokens(bytes: &[u8]) -> Result<(), PublicError> {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if bytes[i] == b'-' || bytes[i].is_ascii_digit() {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || matches!(bytes[i], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                i += 1;
            }
            let token = &bytes[start..i];
            if !token.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                let valid = std::str::from_utf8(token)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .is_some();
                if !valid {
                    return Err(PublicError::BadRequest {
                        message: "JSON integer is outside the i64 range".into(),
                    });
                }
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Every external JSON boundary must use this entry point before typed decoding.
pub fn decode_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, PublicError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(PublicError::BadRequest {
            message: "JSON exceeds 16 MiB".into(),
        });
    }
    check_integer_tokens(bytes)?;
    let strict: JsonValue = serde_json::from_slice(bytes).map_err(|e| PublicError::BadRequest {
        message: e.to_string(),
    })?;
    serde_json::from_value(strict.0).map_err(|e| PublicError::BadRequest {
        message: e.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequestId(pub String);

/// Opaque per-run bearer value. Debug output deliberately redacts it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RunCapability(String);
impl RunCapability {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}
impl fmt::Debug for RunCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RunCapability([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    pub protocol: u16,
    pub request_id: RequestId,
    pub run_capability: Option<RunCapability>,
    pub command: CommandRequest,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "status",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RpcResult {
    Ok(Box<CommandReply>),
    Error(PublicError),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RpcReply {
    pub protocol: u16,
    pub request_id: RequestId,
    pub result: RpcResult,
}

/// Four-byte big-endian byte length followed by one UTF-8 JSON document.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, PublicError> {
    let body = serde_json::to_vec(value).map_err(|e| PublicError::BadRequest {
        message: e.to_string(),
    })?;
    let _: JsonValue = decode_json(&body)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(PublicError::BadRequest {
            message: "frame exceeds 16 MiB".into(),
        });
    }
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    Ok(frame)
}
pub fn decode_frame(bytes: &[u8]) -> Result<RpcRequest, PublicError> {
    let prefix: [u8; 4] = bytes
        .get(..4)
        .and_then(|p| p.try_into().ok())
        .ok_or_else(|| PublicError::BadRequest {
            message: "incomplete frame header".into(),
        })?;
    let size = u32::from_be_bytes(prefix) as usize;
    if size > MAX_FRAME_BYTES || bytes.len() != size + 4 {
        return Err(PublicError::BadRequest {
            message: "invalid frame length".into(),
        });
    }
    let request: RpcRequest = decode_json(&bytes[4..])?;
    if request.protocol != PROTOCOL_VERSION {
        return Err(PublicError::BadRequest {
            message: "unsupported protocol version".into(),
        });
    }
    Ok(request)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FnInvocation {
    pub project: ProjectId,
    pub step: Option<StepId>,
    pub run: RunId,
    pub attempt: AttemptId,
    pub invocation: InvocationId,
    pub name: String,
    pub inputs: JsonMap,
}
