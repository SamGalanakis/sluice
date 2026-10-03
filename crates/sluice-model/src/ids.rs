use std::{borrow::Cow, fmt, str::FromStr};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: {value}")]
pub struct InvalidId {
    pub kind: &'static str,
    pub value: String,
}

macro_rules! uuid_id {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);
        impl $name {
            pub fn new() -> Self { Self(Uuid::now_v7()) }
            pub fn as_uuid(&self) -> &Uuid { &self.0 }
        }
        impl Default for $name { fn default() -> Self { Self::new() } }
        impl TryFrom<Uuid> for $name {
            type Error = InvalidId;
            fn try_from(value: Uuid) -> Result<Self, Self::Error> {
                if value.get_version_num() == 7 && value.get_variant() == uuid::Variant::RFC4122 {
                    Ok(Self(value))
                } else { Err(InvalidId { kind: stringify!($name), value: value.to_string() }) }
            }
        }
        impl FromStr for $name {
            type Err = InvalidId;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map_err(|_| InvalidId { kind: stringify!($name), value: value.into() })?.try_into()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
            }
        }
        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> { stringify!($name).into() }
            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({"type":"string", "format":"uuid", "pattern":"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$"})
            }
        }
    )+};
}
uuid_id!(ProjectId, RunId, AttemptId, ResultId, InvocationId, HomeId);

macro_rules! name_id {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidId> {
                let value = value.into();
                if value.as_bytes().first().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && value.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-') {
                    Ok(Self(value))
                } else { Err(InvalidId { kind: stringify!($name), value }) }
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl FromStr for $name {
            type Err = InvalidId;
            fn from_str(value: &str) -> Result<Self, Self::Err> { Self::new(value) }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }
        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> { stringify!($name).into() }
            fn json_schema(_: &mut SchemaGenerator) -> Schema { json_schema!({"type":"string", "pattern":"^[a-z0-9][a-z0-9_-]*$"}) }
        }
    )+};
}
name_id!(StepId, UnitName, ProjectName);

macro_rules! counter_id {
    ($($name:ident($inner:ty)),+ $(,)?) => {$ (
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub $inner);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
        }
    )+};
}
counter_id!(
    Revision(u64),
    RecordSeq(i64),
    MessageId(i64),
    WorkGeneration(u64),
    StepGeneration(u64),
    LeaseId(i64)
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ProjectSelector {
    Name(ProjectName),
    Id(ProjectId),
}
impl fmt::Display for ProjectSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => name.fmt(f),
            Self::Id(id) => write!(f, "id:{id}"),
        }
    }
}
impl FromStr for ProjectSelector {
    type Err = InvalidId;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.strip_prefix("id:") {
            Some(id) => Ok(Self::Id(id.parse()?)),
            None => Ok(Self::Name(value.parse()?)),
        }
    }
}
