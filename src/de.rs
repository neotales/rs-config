//! A crate-local serde deserializer over [`Value`].
//!
//! This module replaces a trip through a format parser. It exists for two reasons: the core has
//! no format parser dependency, and a serde error from a foreign parser can echo the value it
//! failed on.
//!
//! Nothing a caller supplies reaches a [`DeError`]. A `Deserialize` implementation chooses the
//! message of [`serde::de::Error::custom`] and the expectation handed to `invalid_type`,
//! `invalid_value`, and `invalid_length`; any of those can contain a value or a secret. Only the
//! fixed labels declared in this module are stored, so an error can name a shape but never echo
//! text.

use std::{collections::btree_map, fmt, slice};

use serde::de::{
    self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, VariantAccess, Visitor,
};

use crate::value::Value;

/// One node handed to serde: a borrowed value, or a map key.
#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    /// A value in the tree.
    Value(&'a Value),
    /// A string that is not stored as a [`Value`], such as a map key.
    Str(&'a str),
}

// The complete set of text a deserialization error can carry. Each label is written here, so no
// caller-supplied expectation, field name, or custom message can appear in an error.
const COMPATIBLE: &str = "a compatible value";
const EXPECTED_TYPE: &str = "a value of the expected type";
const PERMITTED_VALUE: &str = "a permitted value";
const EXPECTED_LENGTH: &str = "a sequence of the expected length";
const KNOWN_VARIANT: &str = "a known variant";
const KNOWN_FIELDS: &str = "a map without unknown fields";
const REQUIRED_FIELDS: &str = "a map with every required field";
const DUPLICATE_FIELDS: &str = "a map without duplicate fields";

/// A failed deserialization, reduced to a label from this module.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeError {
    expected: &'static str,
}

impl DeError {
    const fn mismatch(expected: &'static str) -> Self {
        Self { expected }
    }

    /// The description of the expected shape, never the value that was found.
    pub(crate) const fn expected(&self) -> &'static str {
        self.expected
    }
}

impl fmt::Display for DeError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(self.expected)
    }
}

impl std::error::Error for DeError {}

impl de::Error for DeError {
    fn custom<T: fmt::Display>(_message: T) -> Self {
        // The message is dropped: a derived or caller-written `Deserialize` may put a value in it.
        Self::mismatch(COMPATIBLE)
    }

    fn invalid_type(_unexpected: de::Unexpected<'_>, _expected: &dyn de::Expected) -> Self {
        // Both the unexpected value and the caller's expectation are dropped.
        Self::mismatch(EXPECTED_TYPE)
    }

    fn invalid_value(_unexpected: de::Unexpected<'_>, _expected: &dyn de::Expected) -> Self {
        Self::mismatch(PERMITTED_VALUE)
    }

    fn invalid_length(_len: usize, _expected: &dyn de::Expected) -> Self {
        Self::mismatch(EXPECTED_LENGTH)
    }

    fn unknown_variant(_variant: &str, _expected: &'static [&'static str]) -> Self {
        Self::mismatch(KNOWN_VARIANT)
    }

    fn unknown_field(_field: &str, _expected: &'static [&'static str]) -> Self {
        Self::mismatch(KNOWN_FIELDS)
    }

    fn missing_field(_field: &'static str) -> Self {
        Self::mismatch(REQUIRED_FIELDS)
    }

    fn duplicate_field(_field: &'static str) -> Self {
        Self::mismatch(DUPLICATE_FIELDS)
    }
}

/// The node as an exact integer. A float counts only when it has no fraction.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    reason = "an exact round trip is the check; the range guard keeps each cast meaningful"
)]
fn float_to_integer(number: f64) -> Option<i128> {
    if !number.is_finite() || number.fract() != 0.0 {
        return None;
    }
    if number < i64::MIN as f64 || number > u64::MAX as f64 {
        return None;
    }
    let integer = number as i128;
    // Reject a float that no longer round-trips, such as 2^63 rendered imprecisely.
    (integer as f64 == number).then_some(integer)
}

fn exact_integer(node: Node<'_>) -> Option<i128> {
    match node {
        Node::Value(Value::Integer(number)) => Some(*number),
        Node::Value(Value::Float(number)) => float_to_integer(*number),
        _ => None,
    }
}

/// Converts an integer to `f64` only when the float denotes exactly the same number.
///
/// An `f64` carries 53 significant bits. An integer that needs more is representable only when
/// every bit the mantissa drops is zero, so `2^60` converts exactly and `u64::MAX` does not. A
/// plain `as f64` followed by `as i128` would accept `i128::MAX`, because that back-cast
/// saturates onto the value it rounded away from.
fn integer_into_f64(number: i128) -> Option<f64> {
    let magnitude = number.unsigned_abs();
    let significant_bits = magnitude.bit_width();
    if significant_bits > f64::MANTISSA_DIGITS {
        let dropped = significant_bits - f64::MANTISSA_DIGITS;
        if magnitude & ((1_u128 << dropped) - 1) != 0 {
            return None;
        }
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "the check above proves the conversion is exact"
    )]
    let converted = number as f64;
    converted.is_finite().then_some(converted)
}

/// Narrows to `f32` only when the value survives unchanged.
#[allow(
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    reason = "the round trip rejects a changed value"
)]
fn exact_f32(number: f64) -> Option<f32> {
    let narrowed = number as f32;
    (f64::from(narrowed) == number).then_some(narrowed)
}

fn visit_integer<'de, V: Visitor<'de>>(number: i128, visitor: V) -> Result<V::Value, DeError> {
    if let Ok(value) = i64::try_from(number) {
        visitor.visit_i64(value)
    } else if let Ok(value) = u64::try_from(number) {
        visitor.visit_u64(value)
    } else {
        visitor.visit_i128(number)
    }
}

impl<'de> Deserializer<'de> for Node<'de> {
    type Error = DeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Str(text) => visitor.visit_borrowed_str(text),
            Self::Value(value) => match value {
                Value::Null => visitor.visit_unit(),
                Value::Bool(flag) => visitor.visit_bool(*flag),
                Value::Integer(number) => visit_integer(*number, visitor),
                Value::Float(number) => visitor.visit_f64(*number),
                Value::String(text) => visitor.visit_borrowed_str(text),
                Value::Array(items) => visitor.visit_seq(SequenceAccess {
                    items: items.iter(),
                }),
                Value::Map(map) => visitor.visit_map(MapAccessor {
                    entries: map.iter(),
                    value: None,
                }),
            },
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Value(Value::Null) => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Value(Value::Null) => visitor.visit_unit(),
            _ => Err(DeError::mismatch("null")),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Value(Value::Bool(flag)) => visitor.visit_bool(*flag),
            _ => Err(DeError::mismatch("a boolean")),
        }
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| i8::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("a signed 8-bit integer")),
                |value| visitor.visit_i8(value),
            )
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| i16::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("a signed 16-bit integer")),
                |value| visitor.visit_i16(value),
            )
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| i32::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("a signed 32-bit integer")),
                |value| visitor.visit_i32(value),
            )
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| i64::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("a signed 64-bit integer")),
                |value| visitor.visit_i64(value),
            )
    }

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self).map_or_else(
            || Err(DeError::mismatch("a signed integer")),
            |value| visitor.visit_i128(value),
        )
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| u8::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("an unsigned 8-bit integer")),
                |value| visitor.visit_u8(value),
            )
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| u16::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("an unsigned 16-bit integer")),
                |value| visitor.visit_u16(value),
            )
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| u32::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("an unsigned 32-bit integer")),
                |value| visitor.visit_u32(value),
            )
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| u64::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("an unsigned 64-bit integer")),
                |value| visitor.visit_u64(value),
            )
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        exact_integer(self)
            .and_then(|number| u128::try_from(number).ok())
            .map_or_else(
                || Err(DeError::mismatch("an unsigned integer")),
                |value| visitor.visit_u128(value),
            )
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let number = match self {
            Self::Value(Value::Float(number)) => exact_f32(*number),
            Self::Value(Value::Integer(number)) => integer_into_f64(*number).and_then(exact_f32),
            _ => None,
        };
        number.map_or_else(
            || {
                Err(DeError::mismatch(
                    "a 32-bit float that holds the value exactly",
                ))
            },
            |value| visitor.visit_f32(value),
        )
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        let number = match self {
            Self::Value(Value::Float(number)) => Some(*number),
            Self::Value(Value::Integer(number)) => integer_into_f64(*number),
            _ => None,
        };
        number.map_or_else(
            || {
                Err(DeError::mismatch(
                    "a 64-bit float that holds the value exactly",
                ))
            },
            |value| visitor.visit_f64(value),
        )
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Str(text) => visitor.visit_borrowed_str(text),
            Self::Value(Value::String(text)) => visitor.visit_borrowed_str(text),
            Self::Value(_) => Err(DeError::mismatch("a string")),
        }
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_any(visitor)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_any(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Value(Value::Array(items)) => visitor.visit_seq(SequenceAccess {
                items: items.iter(),
            }),
            _ => Err(DeError::mismatch("an array")),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        fixed_sequence(self, len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, DeError> {
        fixed_sequence(self, len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        match self {
            Self::Value(Value::Map(map)) => visitor.visit_map(MapAccessor {
                entries: map.iter(),
                value: None,
            }),
            _ => Err(DeError::mismatch("a map")),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeError> {
        match self {
            Self::Value(Value::Map(map)) => visitor.visit_map(MapAccessor {
                entries: map.iter(),
                value: None,
            }),
            _ => Err(DeError::mismatch("a map")),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeError> {
        match self {
            Self::Str(variant) => visitor.visit_enum(EnumAccessor {
                variant,
                value: None,
            }),
            Self::Value(Value::String(variant)) => visitor.visit_enum(EnumAccessor {
                variant,
                value: None,
            }),
            Self::Value(Value::Map(map)) if map.len() == 1 => {
                let Some((variant, value)) = map.iter().next() else {
                    return Err(DeError::mismatch("an enum"));
                };
                visitor.visit_enum(EnumAccessor {
                    variant,
                    value: Some(value),
                })
            }
            Self::Value(_) => Err(DeError::mismatch("an enum")),
        }
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
        visitor.visit_unit()
    }
}

/// Visits an array only when it has exactly `len` elements.
///
/// Serde's tuple visitor reads `len` elements and ignores the rest, so a length check here is the
/// only place an extra or missing element is caught.
fn fixed_sequence<'de, V: Visitor<'de>>(
    node: Node<'de>,
    len: usize,
    visitor: V,
) -> Result<V::Value, DeError> {
    match node {
        Node::Value(Value::Array(items)) if items.len() == len => {
            visitor.visit_seq(SequenceAccess {
                items: items.iter(),
            })
        }
        _ => Err(DeError::mismatch(EXPECTED_LENGTH)),
    }
}

struct SequenceAccess<'de> {
    items: slice::Iter<'de, Value>,
}

impl<'de> SeqAccess<'de> for SequenceAccess<'de> {
    type Error = DeError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, DeError> {
        match self.items.next() {
            Some(item) => seed.deserialize(Node::Value(item)).map(Some),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len())
    }
}

struct MapAccessor<'de> {
    entries: btree_map::Iter<'de, String, Value>,
    value: Option<&'de Value>,
}

impl<'de> MapAccess<'de> for MapAccessor<'de> {
    type Error = DeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, DeError> {
        match self.entries.next() {
            Some((key, value)) => {
                self.value = Some(value);
                seed.deserialize(Node::Str(key)).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, DeError> {
        let value = self
            .value
            .take()
            .ok_or_else(|| DeError::mismatch("a map value"))?;
        seed.deserialize(Node::Value(value))
    }
}

struct EnumAccessor<'de> {
    variant: &'de str,
    value: Option<&'de Value>,
}

impl<'de> de::EnumAccess<'de> for EnumAccessor<'de> {
    type Error = DeError;
    type Variant = VariantAccessor<'de>;

    fn variant_seed<K: DeserializeSeed<'de>>(
        self,
        seed: K,
    ) -> Result<(K::Value, Self::Variant), DeError> {
        let variant = seed.deserialize(Node::Str(self.variant))?;
        Ok((variant, VariantAccessor { value: self.value }))
    }
}

struct VariantAccessor<'de> {
    value: Option<&'de Value>,
}

impl<'de> VariantAccess<'de> for VariantAccessor<'de> {
    type Error = DeError;

    fn unit_variant(self) -> Result<(), DeError> {
        match self.value {
            None => Ok(()),
            Some(_) => Err(DeError::mismatch("a unit variant")),
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, DeError> {
        match self.value {
            Some(value) => seed.deserialize(Node::Value(value)),
            None => Err(DeError::mismatch("a newtype variant")),
        }
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, DeError> {
        match self.value {
            Some(value) => fixed_sequence(Node::Value(value), len, visitor),
            None => Err(DeError::mismatch("a tuple variant")),
        }
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeError> {
        match self.value {
            Some(Value::Map(map)) => visitor.visit_map(MapAccessor {
                entries: map.iter(),
                value: None,
            }),
            _ => Err(DeError::mismatch("a struct variant")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::Node;
    use crate::value::Value;

    fn read<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, super::DeError> {
        T::deserialize(Node::Value(value))
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Http {
        port: u16,
        host: Option<String>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    enum Mode {
        Fast,
        Slow(u8),
        Pair(u8, u8),
        Struct(Http),
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct PairStruct(u8, u8);

    #[test]
    fn reads_structs_and_options() {
        let value = Value::map([
            ("port", Value::from(8080_i64)),
            ("host", Value::from("localhost")),
        ]);
        assert_eq!(
            read::<Http>(&value).unwrap(),
            Http {
                port: 8080,
                host: Some("localhost".to_owned())
            }
        );

        let value = Value::map([("port", Value::from(8080_i64)), ("host", Value::Null)]);
        assert_eq!(read::<Http>(&value).unwrap().host, None);
    }

    #[test]
    fn reads_sequences_maps_and_enums() {
        let value = Value::array([Value::from(1_i64), Value::from(2_i64)]);
        assert_eq!(read::<Vec<i64>>(&value).unwrap(), vec![1, 2]);

        let value = Value::map([("a", Value::from(1_i64))]);
        assert_eq!(
            read::<std::collections::BTreeMap<String, i64>>(&value).unwrap()["a"],
            1
        );

        assert_eq!(read::<Mode>(&Value::from("Fast")).unwrap(), Mode::Fast);
        let value = Value::map([("Slow", Value::from(3_i64))]);
        assert_eq!(read::<Mode>(&value).unwrap(), Mode::Slow(3));
    }

    #[test]
    fn reads_externally_tagged_tuple_and_struct_variants() {
        let value = Value::map([(
            "Pair",
            Value::array([Value::from(1_i64), Value::from(2_i64)]),
        )]);
        assert_eq!(read::<Mode>(&value).unwrap(), Mode::Pair(1, 2));

        let http = Value::map([
            ("port", Value::from(8080_i64)),
            ("host", Value::from("localhost")),
        ]);
        let value = Value::map([("Struct", http)]);
        assert_eq!(
            read::<Mode>(&value).unwrap(),
            Mode::Struct(Http {
                port: 8080,
                host: Some("localhost".to_owned())
            })
        );

        // A tuple variant of the wrong width is rejected rather than truncated.
        let value = Value::map([(
            "Pair",
            Value::array([Value::from(1_i64), Value::from(2_i64), Value::from(3_i64)]),
        )]);
        assert!(read::<Mode>(&value).is_err());
    }

    #[test]
    fn tuple_and_tuple_struct_lengths_must_match() {
        let three = Value::array([Value::from(1_i64), Value::from(2_i64), Value::from(3_i64)]);
        assert!(read::<(u8, u8)>(&three).is_err());
        assert!(read::<PairStruct>(&three).is_err());

        let two = Value::array([Value::from(1_i64), Value::from(2_i64)]);
        assert_eq!(read::<(u8, u8)>(&two).unwrap(), (1, 2));
        assert_eq!(read::<PairStruct>(&two).unwrap(), PairStruct(1, 2));
    }

    #[test]
    fn accepts_lossless_numeric_fits_and_rejects_the_rest() {
        assert_eq!(read::<u64>(&Value::from(7_i64)).unwrap(), 7);
        let as_float = read::<f64>(&Value::from(8080_i64)).unwrap();
        assert!((as_float - 8080.0).abs() < f64::EPSILON);
        assert_eq!(read::<i64>(&Value::Float(8080.0)).unwrap(), 8080);
        assert!(read::<i64>(&Value::Float(8080.5)).is_err());
        assert!(read::<i64>(&Value::from("8080")).is_err());
        assert!(read::<u64>(&Value::from(-1_i64)).is_err());
        assert!(read::<u8>(&Value::from(300_i64)).is_err());
        let biggest = Value::Integer(i128::from(u64::MAX));
        assert_eq!(read::<u64>(&biggest).unwrap(), u64::MAX);
        assert!(read::<i64>(&biggest).is_err());
        assert!(read::<f64>(&Value::Integer(i128::from(i64::MAX))).is_err());
    }

    #[test]
    fn an_integer_only_becomes_a_float_when_it_stays_exact() {
        // Two to the sixtieth is exactly representable; one more is not.
        assert_eq!(
            read::<f64>(&Value::Integer(1_i128 << 60))
                .unwrap()
                .to_bits(),
            2_f64.powi(60).to_bits()
        );
        assert!(read::<f64>(&Value::Integer((1_i128 << 60) + 1)).is_err());
        assert!(read::<f64>(&Value::Integer(i128::MAX)).is_err());
        assert!(read::<f64>(&Value::Integer(i128::MIN + 1)).is_err());
        assert_eq!(
            read::<f64>(&Value::Integer(i128::MIN)).unwrap().to_bits(),
            (-2_f64.powi(127)).to_bits()
        );
        // A float that does not survive `f32` is rejected too.
        assert!(read::<f32>(&Value::Float(0.1)).is_err());
    }

    #[test]
    fn a_custom_message_cannot_reach_the_error() {
        #[derive(Debug)]
        struct Strict;

        impl<'de> Deserialize<'de> for Strict {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Err(serde::de::Error::custom(format!("rejected {value}")))
            }
        }

        let error = read::<Strict>(&Value::from("fixture-secret-9f3a2c")).unwrap_err();
        assert!(!error.expected().contains("fixture-secret-9f3a2c"));
        assert!(!format!("{error}").contains("fixture-secret-9f3a2c"));
    }

    #[test]
    fn a_custom_expectation_cannot_reach_the_error() {
        #[derive(Debug)]
        struct Finicky;

        impl<'de> Deserialize<'de> for Finicky {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Err(serde::de::Error::invalid_value(
                    serde::de::Unexpected::Str(&value),
                    &"fixture-secret-review",
                ))
            }
        }

        let error = read::<Finicky>(&Value::from("fixture-secret-9f3a2c")).unwrap_err();
        let shown = format!("{error}");
        let debugged = format!("{error:?}");
        for text in [&shown, &debugged, &error.expected().to_owned()] {
            assert!(
                !text.contains("fixture-secret-review"),
                "the custom expectation reached the error: {text}"
            );
            assert!(
                !text.contains("fixture-secret-9f3a2c"),
                "the value reached the error: {text}"
            );
        }
    }
}
