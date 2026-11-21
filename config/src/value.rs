//! The immutable configuration value tree.

use std::{collections::BTreeMap, fmt};

use serde::{
    Deserialize,
    de::{self, Deserializer, Visitor},
};

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
    /// Holds every `i128`. A typed read narrows it to the target integer type and fails when the
    /// target cannot hold it. It is never read as a float, and a float is never read as an
    /// integer, even when the number has no fraction.
    Integer(i128),
    /// A floating-point number.
    ///
    /// A source that would produce a non-finite number is rejected at build, so this variant is
    /// always finite in a published snapshot. A typed read reads it only as a narrower float that
    /// holds it exactly.
    Float(f64),
    /// A string. A secret reference stays a string and is never resolved.
    String(String),
    /// A date, time, or offset date-time.
    ///
    /// Only a TOML document produces this variant. It preserves the canonical RFC 3339 spelling
    /// the parser produced, so a local date such as `1979-05-27`, a local time such as
    /// `07:32:00`, and an offset date-time all keep their exact form. A typed read requires
    /// [`DateTime`]; it is never read as a [`String`] and a string is never read as this, and it
    /// is never read as a map and a map is never read as this.
    DateTime(DateTime),
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
            Self::DateTime(_) => "datetime",
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
            Self::Null | Self::Bool(_) | Self::Integer(_) | Self::String(_) | Self::DateTime(_) => {
                true
            }
        }
    }
}

/// A date, time, or offset date-time, preserved without conversion.
///
/// A TOML document keeps the canonical RFC 3339 spelling the parser produced: a local date such
/// as `1979-05-27`, a local time such as `07:32:00`, a local date-time, or an offset date-time.
/// Whether the value carries a date, a time, and an offset is observable in the text, so nothing
/// is dropped when a document is read into a [`Value`].
///
/// A typed read uses this type, `get::<DateTime>("started")`. It is never read as a [`String`],
/// a string is never read as this type, and it is never read as a map: the marker this type hands
/// its deserializer names the date-time kind rather than the one-field map an earlier version
/// used, so a map that happens to contain a `text` key cannot become a [`DateTime`] and a
/// [`DateTime`] cannot become a generic map.
///
/// ```
/// use neotales_config::{Builder, DateTime, Value};
///
/// let config = Builder::new()
///     .memory(
///         "defaults",
///         Value::map([("started", Value::from(DateTime::new("1979-05-27")))]),
///     )
///     .build()?;
/// assert_eq!(config.get::<DateTime>("started")?.as_str(), "1979-05-27");
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// [`std::fmt::Debug`] shows the text, like the rest of a [`Value`] tree. Do not log a date-time
/// that came from a document you would not log.
#[derive(Clone, Eq, PartialEq)]
pub struct DateTime {
    text: String,
}

impl DateTime {
    /// Wraps RFC 3339 text without validating it.
    ///
    /// A value produced by a TOML source always holds canonical text. This constructor exists so
    /// an in-memory or caller-supplied source can build a [`Value::DateTime`]; the caller owns the
    /// spelling.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Returns the canonical RFC 3339 text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.text)
    }
}

impl fmt::Debug for DateTime {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.debug_tuple("DateTime").field(&self.text).finish()
    }
}

/// The marker `DateTime`'s `Deserialize` hands its deserializer.
///
/// The value cursor treats this name as the date-time kind. It is not a map member and it is not
/// a generic newtype, so neither a map nor a string can satisfy a [`DateTime`] read, and a
/// [`DateTime`] cannot satisfy a generic map read.
pub(crate) const DESERIALIZE_NAME: &str = "$neotales_config::DateTime";

impl<'de> de::Deserialize<'de> for DateTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// Reads the exact text the value cursor hands a date-time.
        struct DateTimeVisitor;

        impl<'de> Visitor<'de> for DateTimeVisitor {
            type Value = DateTime;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a date-time")
            }

            fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                // The cursor hands the preserved RFC 3339 text, which is the only content a
                // `DateTime` holds.
                Ok(DateTime::new(String::deserialize(deserializer)?))
            }
        }

        deserializer.deserialize_newtype_struct(DESERIALIZE_NAME, DateTimeVisitor)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<DateTime> for Value {
    fn from(value: DateTime) -> Self {
        Self::DateTime(value)
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
