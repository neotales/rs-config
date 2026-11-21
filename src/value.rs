//! The immutable configuration value tree.

use std::collections::BTreeMap;

/// A format-neutral configuration value.
///
/// Maps use [`BTreeMap`], so iteration order is deterministic and does not depend on the order a
/// source produced. A map is the only valid root for a source.
///
/// # Secrets
///
/// [`std::fmt::Debug`] prints the contents, because a value tree is diagnostic data. Do not log a
/// [`Value`] that holds a secret. [`crate::Config`]'s `Debug` never prints values.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Absence of a value.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer.
    ///
    /// Holds every `i128`. A typed read narrows it to the target type and fails when the target
    /// cannot hold it.
    Integer(i128),
    /// A floating-point number.
    ///
    /// A source that would produce a non-finite number is rejected at build, so this variant is
    /// always finite in a published snapshot.
    Float(f64),
    /// A string. A secret reference stays a string and is never resolved.
    String(String),
    /// An ordered list. Lists replace during a merge; they never concatenate.
    Array(Vec<Self>),
    /// A string-keyed map. Nested maps merge during a build; every other value replaces.
    Map(BTreeMap<String, Self>),
}

impl Value {
    /// Builds a map from string-like keys and values.
    #[must_use]
    pub fn map<K, V>(entries: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<Self>,
    {
        Self::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    /// Builds an array from values.
    #[must_use]
    pub fn array<V>(entries: impl IntoIterator<Item = V>) -> Self
    where
        V: Into<Self>,
    {
        Self::Array(entries.into_iter().map(Into::into).collect())
    }

    /// Returns this value as a map.
    #[must_use]
    pub const fn as_map(&self) -> Option<&BTreeMap<String, Self>> {
        match self {
            Self::Map(map) => Some(map),
            _ => None,
        }
    }

    /// Returns this value as an array.
    #[must_use]
    pub const fn as_array(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Array(array) => Some(array),
            _ => None,
        }
    }

    /// Returns this value's type without formatting its contents.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Float(_) => "float",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Map(_) => "map",
        }
    }

    /// Walks `segments` from this value. An empty sequence returns this value.
    pub(crate) fn descend<'a, 'b>(
        &'a self,
        segments: impl Iterator<Item = &'b str>,
    ) -> Option<&'a Self> {
        let mut current = self;
        for segment in segments {
            match current {
                Self::Map(map) => current = map.get(segment)?,
                _ => return None,
            }
        }
        Some(current)
    }

    /// Whether every number in this tree can be represented without silent loss.
    ///
    /// A non-finite float cannot, and is the only rejected case today.
    pub(crate) fn is_representable(&self) -> bool {
        match self {
            Self::Float(number) => number.is_finite(),
            Self::Array(items) => items.iter().all(Self::is_representable),
            Self::Map(map) => map.values().all(Self::is_representable),
            Self::Null | Self::Bool(_) | Self::Integer(_) | Self::String(_) => true,
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i8> for Value {
    fn from(value: i8) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<i16> for Value {
    fn from(value: i16) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<i128> for Value {
    fn from(value: i128) -> Self {
        Self::Integer(value)
    }
}

impl From<u8> for Value {
    fn from(value: u8) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<u16> for Value {
    fn from(value: u16) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Integer(i128::from(value))
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Self::Float(f64::from(value))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<Vec<Self>> for Value {
    fn from(value: Vec<Self>) -> Self {
        Self::Array(value)
    }
}

impl From<BTreeMap<String, Self>> for Value {
    fn from(value: BTreeMap<String, Self>) -> Self {
        Self::Map(value)
    }
}
