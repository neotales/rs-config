//! The opt-in JSON file source.
//!
//! The parser shape follows the plab rs-lib `neotales-config` JSON source
//! (MIT, Copyright (c) 2026 Neotales), rewritten for this crate's error type.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::{SourceError, Value};

use super::{Source, file::read_text};

/// A JSON document read as one configuration source.
///
/// The file is read again on every [`Source::load`], so registering it again or loading it again
/// sees a replaced file. There is no watcher and no cached document. The dialect is RFC 8259 JSON
/// in UTF-8: comments, trailing commas, single-quoted strings, unquoted keys, and a byte-order mark
/// are not part of it and reject the source.
///
/// By default the file is required and its path is watched. [`Self::optional`] and
/// [`Self::reload_on_change`] change those two defaults independently: an optional source treats
/// only a genuine not-found as an empty document, and a source with change watching off keeps its
/// path out of the polling watcher while a manual refresh still reads it.
///
/// The document root must be an object. Object members keep their document order for duplicate
/// detection, and a later member with the same name replaces the earlier one; the resulting map is
/// sorted by name, as every [`Value::Map`] is. An array root, a scalar root, and an empty document
/// reject the source.
///
/// Numbers are exact. An integer literal is held as [`Value::Integer`] up to `i128` and an integer
/// outside that range rejects the source instead of being rounded through a float. A float literal
/// is held as [`Value::Float`]; a literal that is not finite rejects the source. A quoted number
/// stays a string.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read (including when it is
/// absent, unless [`Self::optional`] is enabled), [`SourceError::invalid`] when it is not UTF-8,
/// is not valid JSON, or does not have an object root, and [`SourceError::unrepresentable`] when a
/// number does not fit the value model. The error carries the kind only; it never carries a path,
/// a line, a member name, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, JsonFile};
///
/// let config = Builder::new()
///     .json_file("json", JsonFile::new("config/defaults.json"))
///     .build()?;
/// assert_eq!(config.get::<u16>("http.port")?, 8080);
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; a configuration file may hold a secret. Its
/// error and its [`fmt::Debug`] omit the path and the contents.
#[derive(Clone)]
pub struct JsonFile {
    path: PathBuf,
    optional: bool,
    reload_on_change: bool,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; only the two \
              behavior flags are reportable"
)]
impl fmt::Debug for JsonFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("JsonFile")
            .field("optional", &self.optional)
            .field("reload_on_change", &self.reload_on_change)
            .finish_non_exhaustive()
    }
}

impl JsonFile {
    /// Registers `path` to be read when the source loads.
    ///
    /// The path is opened once per load and is never written. The file is required and watched by
    /// default; see [`Self::optional`] and [`Self::reload_on_change`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            optional: false,
            reload_on_change: true,
        }
    }

    /// Treats an absent file as an empty document instead of an error.
    ///
    /// The default is `false`, so an absent file fails the load with
    /// [`SourceError::unavailable`] exactly as before. With `true`, only an actual
    /// [`std::io::ErrorKind::NotFound`] from opening the path becomes an empty map. A path that is
    /// a directory, a permission failure, a non-UTF-8 file, a parse failure, and a number the model
    /// cannot hold all still reject the source. The open result itself decides: there is no
    /// separate existence check for a file to appear or vanish between.
    ///
    /// An optional file that is absent may appear later, and a watched optional file that
    /// disappears is a change the next refresh observes: the values it contributed are dropped and
    /// any value from an earlier source shows through again. A parse failure is not an absence; it
    /// fails the refresh and keeps the previous snapshot published.
    ///
    /// ```no_run
    /// use neotales_config::{Builder, JsonFile};
    ///
    /// let config = Builder::new()
    ///     .json_file(
    ///         "local",
    ///         JsonFile::new("config/local.json")
    ///             .optional(true)
    ///             .reload_on_change(false),
    ///     )
    ///     .build()?;
    /// assert!(!config.has("local.port")?);
    /// # Ok::<(), neotales_config::Error>(())
    /// ```
    #[must_use]
    pub const fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Reports the configured path to the polling watcher.
    ///
    /// The default is `true`. With `false`, [`Source::watch_paths`] returns an empty list for this
    /// source, so `Reload::refresh_if_changed` and `Reload::start_watching` do not poll the file;
    /// `Reload::refresh` still loads every source, including this one. If no source reports a path,
    /// `Reload::start_watching` returns `WatchError::NoWatchableSources`.
    ///
    /// This setting is independent of [`Self::optional`]: it changes what is observed, not whether
    /// an absent file is accepted.
    #[must_use]
    pub const fn reload_on_change(mut self, reload_on_change: bool) -> Self {
        self.reload_on_change = reload_on_change;
        self
    }

    /// Returns the configured path.
    ///
    /// The path is not part of any error or of this type's [`fmt::Debug`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Source for JsonFile {
    fn load(&self) -> Result<Value, SourceError> {
        let Some(text) = read_text(&self.path, self.optional)? else {
            return Ok(Value::Map(BTreeMap::new()));
        };
        let value = parse(&text)?;
        if value.as_map().is_none() {
            return Err(SourceError::invalid());
        }
        Ok(value)
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        if self.reload_on_change {
            vec![self.path.clone()]
        } else {
            Vec::new()
        }
    }
}

/// Parses one RFC 8259 JSON document and keeps every number exact.
fn parse(text: &str) -> Result<Value, SourceError> {
    let mut tokens = Tokens::new(text);
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let parsed = JsonSeed {
        tokens: &mut tokens,
    }
    .deserialize(&mut deserializer);
    if tokens.unrepresentable {
        return Err(SourceError::unrepresentable());
    }
    let value = parsed.map_err(|_| SourceError::invalid())?;
    if tokens.index != tokens.list.len() {
        // The token scan and the parser disagreed about the document; do not guess which is right.
        return Err(SourceError::invalid());
    }
    deserializer.end().map_err(|_| SourceError::invalid())?;
    Ok(value)
}

/// One number token, taken from the document text in order.
enum NumberToken {
    /// An integer literal that fits the value model.
    Integer(i128),
    /// A number literal that does not fit the value model.
    Unrepresentable,
    /// A float literal. Finite and non-finite literals are both kept here so the builder can
    /// report a non-finite value as unrepresentable rather than as malformed text.
    Float(f64),
}

/// The number tokens of one document, consumed in the order `serde_json` visits them.
struct Tokens {
    list: Vec<NumberToken>,
    index: usize,
    unrepresentable: bool,
}

impl Tokens {
    fn new(text: &str) -> Self {
        let mut list = Vec::new();
        let mut unrepresentable = false;
        for token in number_tokens(text) {
            match token {
                NumberToken::Float(number) if !number.is_finite() => {
                    unrepresentable = true;
                    list.push(NumberToken::Unrepresentable);
                }
                token => {
                    if matches!(token, NumberToken::Unrepresentable) {
                        unrepresentable = true;
                    }
                    list.push(token);
                }
            }
        }
        Self {
            list,
            index: 0,
            unrepresentable,
        }
    }

    /// The next number.
    ///
    /// The token, not the visitor's argument, decides the value: `serde_json` reports an integer
    /// beyond `u64` through `visit_f64`, and that float has already been rounded.
    fn number(&mut self) -> Result<Value, &'static str> {
        match self.next()? {
            NumberToken::Integer(number) => Ok(Value::Integer(*number)),
            NumberToken::Float(number) => Ok(Value::Float(*number)),
            NumberToken::Unrepresentable => Err("a number the value model cannot hold"),
        }
    }

    fn next(&mut self) -> Result<&NumberToken, &'static str> {
        let token = self.list.get(self.index);
        self.index += 1;
        token.ok_or("a number token in document order")
    }
}

/// Scans the document for number tokens, outside strings, in document order.
///
/// The document is parsed by `serde_json` afterwards, so the scan only has to recognise the
/// numbers it will visit. A disagreement is caught by comparing the token count at the end.
fn number_tokens(text: &str) -> Vec<NumberToken> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                if byte == b'-' {
                    index += 1;
                    // A `-` in malformed input can stand alone; only a following digit makes it a
                    // number. A valid document always has one, so nothing is skipped there.
                    if index >= bytes.len() || !bytes[index].is_ascii_digit() {
                        continue;
                    }
                }
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                let mut is_float = false;
                if index < bytes.len() && bytes[index] == b'.' {
                    is_float = true;
                    index += 1;
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        index += 1;
                    }
                }
                if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
                    is_float = true;
                    index += 1;
                    if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
                        index += 1;
                    }
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        index += 1;
                    }
                }
                let token = &text[start..index];
                if is_float {
                    // The strict JSON float grammar always parses as `f64`; anything else is
                    // malformed and `serde_json` reports it, so no token is recorded.
                    if let Ok(number) = token.parse::<f64>() {
                        tokens.push(NumberToken::Float(number));
                    }
                } else {
                    // A valid integer literal always parses unless it overflows the model.
                    tokens.push(
                        token
                            .parse::<i128>()
                            .map_or(NumberToken::Unrepresentable, NumberToken::Integer),
                    );
                }
            }
            _ => index += 1,
        }
    }
    tokens
}

/// Carries the token cursor through `serde_json`'s deserializer.
struct JsonSeed<'a> {
    tokens: &'a mut Tokens,
}

impl<'de> DeserializeSeed<'de> for JsonSeed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(JsonVisitor {
            tokens: self.tokens,
        })
    }
}

struct JsonVisitor<'a> {
    tokens: &'a mut Tokens,
}

impl<'de> Visitor<'de> for JsonVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<Value, E> {
        self.tokens.number().map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, _value: u64) -> Result<Value, E> {
        self.tokens.number().map_err(E::custom)
    }

    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<Value, E> {
        self.tokens.number().map_err(E::custom)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = sequence.next_element_seed(JsonSeed {
            tokens: &mut *self.tokens,
        })? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(JsonSeed {
                tokens: &mut *self.tokens,
            })?;
            entries.insert(key, value);
        }
        Ok(Value::Map(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{SourceErrorKind, Value};

    fn map(text: &str) -> std::collections::BTreeMap<String, Value> {
        parse(text)
            .expect("the document parses")
            .as_map()
            .expect("the document is an object")
            .clone()
    }

    #[test]
    fn preserves_integers_and_rejects_non_finite_floats() {
        let document = map(r#"{"big": 18446744073709551615, "small": -2}"#);
        assert_eq!(document["big"], Value::Integer(i128::from(u64::MAX)));
        assert_eq!(document["small"], Value::Integer(-2));

        let error = parse(r#"{"huge": 1e400}"#).unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Unrepresentable);
    }

    #[test]
    fn keeps_every_integer_the_model_can_hold() {
        let document = map(r#"{
            "above_u64": 18446744073709551617,
            "below_i64": -9223372036854775809,
            "max": 170141183460469231731687303715884105727,
            "min": -170141183460469231731687303715884105728
        }"#);
        assert_eq!(
            document["above_u64"],
            Value::Integer(18_446_744_073_709_551_617)
        );
        assert_eq!(
            document["below_i64"],
            Value::Integer(-9_223_372_036_854_775_809)
        );
        assert_eq!(document["max"], Value::Integer(i128::MAX));
        assert_eq!(document["min"], Value::Integer(i128::MIN));
    }

    #[test]
    fn an_integer_beyond_i128_is_unrepresentable_without_its_digits() {
        let error = parse(r#"{"big": 170141183460469231731687303715884105728}"#).unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Unrepresentable);
        let text = format!("{error}");
        assert!(
            !text.contains("170141183460469231731687303715884105728"),
            "{text}"
        );
    }

    #[test]
    fn numbers_keep_their_document_order_across_nesting() {
        let document = map(
            r#"{"a": 18446744073709551617, "b": 1.5, "c": [2, 18446744073709551618],
                "d": {"e": -9223372036854775809}, "f": [true, null, "18446744073709551617"]}"#,
        );
        assert_eq!(document["a"], Value::Integer(18_446_744_073_709_551_617));
        let Value::Float(ratio) = &document["b"] else {
            panic!("expected a float");
        };
        assert!((ratio - 1.5).abs() < f64::EPSILON);
        assert_eq!(
            document["c"],
            Value::array([
                Value::Integer(2),
                Value::Integer(18_446_744_073_709_551_618)
            ])
        );
        let Value::Map(inner) = &document["d"] else {
            panic!("expected a map");
        };
        assert_eq!(inner["e"], Value::Integer(-9_223_372_036_854_775_809));
        let Value::Array(items) = &document["f"] else {
            panic!("expected an array");
        };
        assert_eq!(items[2], Value::from("18446744073709551617"));
    }

    #[test]
    fn a_digit_run_inside_a_string_is_not_a_number() {
        let document = map(r#"{"text": "18446744073709551617"}"#);
        assert_eq!(document["text"], Value::from("18446744073709551617"));
    }

    #[test]
    fn malformed_text_is_a_parse_failure_not_an_unrepresentable_one() {
        for broken in [
            "{ this is not json, and it carries fixture-secret-9f3a2c",
            r#"{"a": 1e}"#,
            r#"{"a": -}"#,
            r#"{"a": 1.}"#,
            r"{} {}",
            r"[1] trailing",
        ] {
            let error = parse(broken).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Invalid, "{broken}");
            assert!(!format!("{error}").contains("fixture-secret"));
        }
    }

    #[test]
    fn a_later_duplicate_member_wins() {
        let document = map(r#"{"port": 8080, "port": 9090}"#);
        assert_eq!(document["port"], Value::Integer(9090));
    }
}
