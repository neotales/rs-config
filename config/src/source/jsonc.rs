//! The opt-in JSONC file source.
//!
//! JSONC is parsed by `jsonc-parser` with every extension except comments turned off. Comments
//! are the only addition this source accepts over JSON; trailing commas, single-quoted strings,
//! unquoted keys, hexadecimal numbers, a unary plus, a bare decimal point, `Infinity`, `NaN`, and
//! JSON5 string escapes all reject the source.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use jsonc_parser::{JsonValue, ParseOptions, parse_to_value};

use crate::{SourceError, Value};

use super::{Source, file::read_text};

/// A JSONC document read as one configuration source.
///
/// The dialect is standard JSON plus `//` line comments and `/* ... */` block comments. It is not
/// JSON5: a trailing comma, a single-quoted string, an unquoted member name, a hexadecimal number,
/// a leading `+`, a bare leading or trailing decimal point, `Infinity`, `NaN`, and a JSON5 string
/// escape each reject the document. A byte-order mark is not part of the dialect.
///
/// The file is read again on every [`Source::load`]; there is no watcher and no cached document.
/// The document root must be an object. A later member with the same name replaces the earlier
/// one, and the resulting map is sorted by name. An array root, a scalar root, and an empty
/// document reject the source.
///
/// Numbers are exact. An integer literal is held as [`Value::Integer`] up to `i128`; an integer
/// outside that range rejects the source instead of being rounded through a float. A float literal
/// is held as [`Value::Float`], and a literal that is not finite rejects the source.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read,
/// [`SourceError::invalid`] when it is not UTF-8, is not valid JSONC, or does not have an object
/// root, and [`SourceError::unrepresentable`] when a number does not fit the value model. The
/// error carries the kind only; it never carries a path, a line, a member name, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, JsoncFile};
///
/// // { "http": { "port": 8080 } } // a comment is allowed
/// let config = Builder::new()
///     .jsonc_file("jsonc", JsoncFile::new("config/defaults.jsonc"))
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
pub struct JsoncFile {
    path: PathBuf,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; neither is reportable"
)]
impl fmt::Debug for JsoncFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.debug_struct("JsoncFile").finish_non_exhaustive()
    }
}

impl JsoncFile {
    /// Registers `path` to be read when the source loads.
    ///
    /// The path is opened once per load and is never written.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the configured path.
    ///
    /// The path is not part of any error or of this type's [`fmt::Debug`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Source for JsoncFile {
    fn load(&self) -> Result<Value, SourceError> {
        let text = read_text(&self.path)?;
        let value = parse(&text)?;
        if value.as_map().is_none() {
            return Err(SourceError::invalid());
        }
        Ok(value)
    }
}

/// Parses one JSONC document with only comments enabled beyond standard JSON.
fn parse(text: &str) -> Result<Value, SourceError> {
    let options = ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    };
    let value = parse_to_value(text, &options).map_err(|_| SourceError::invalid())?;
    let value = value.ok_or_else(SourceError::invalid)?;
    convert(value)
}

fn convert(value: JsonValue<'_>) -> Result<Value, SourceError> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Boolean(flag) => Value::Bool(flag),
        JsonValue::Number(text) => number(text)?,
        JsonValue::String(text) => Value::String(text.into_owned()),
        JsonValue::Array(items) => {
            let mut converted = Vec::new();
            for item in items.take_inner() {
                converted.push(convert(item)?);
            }
            Value::Array(converted)
        }
        JsonValue::Object(object) => {
            let mut map = BTreeMap::new();
            for (key, value) in object.take_inner() {
                // A later duplicate member replaces the earlier one, in document order.
                map.insert(key.into_owned(), convert(value)?);
            }
            Value::Map(map)
        }
    })
}

/// Converts one JSONC number token exactly.
fn number(text: &str) -> Result<Value, SourceError> {
    let is_float = text.contains('.') || text.contains('e') || text.contains('E');
    if is_float {
        let number = text.parse::<f64>().map_err(|_| SourceError::invalid())?;
        if !number.is_finite() {
            return Err(SourceError::unrepresentable());
        }
        Ok(Value::Float(number))
    } else {
        text.parse::<i128>()
            .map(Value::Integer)
            .map_err(|_| SourceError::unrepresentable())
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
    fn accepts_line_and_block_comments() {
        let document = map(r#"{
                // a line comment
                "port": 8080, /* a block comment */
                "host": "localhost"
            }"#);
        assert_eq!(document["port"], Value::Integer(8080));
        assert_eq!(document["host"], Value::from("localhost"));
    }

    #[test]
    fn rejects_json5_only_syntax_and_trailing_commas() {
        for document in [
            r#"{"a": 1,}"#,
            r#"{"a": 'single'}"#,
            r"{a: 1}",
            r#"{"a": 0x10}"#,
            r#"{"a": +1}"#,
            r#"{"a": .5}"#,
            r#"{"a": 5.}"#,
            r#"{"a": Infinity}"#,
            r#"{"a": NaN}"#,
            r#"{"a": "\x41"}"#,
            "{\"a\": \"line\\\nbreak\"}",
            r#"{"a": 1} {"b": 2}"#,
        ] {
            assert!(parse(document).is_err(), "{document}");
        }
    }

    #[test]
    fn keeps_integers_beyond_u64_and_rejects_beyond_i128() {
        let document = map(r#"{"value": 18446744073709551617, "min": -9223372036854775809}"#);
        assert_eq!(
            document["value"],
            Value::Integer(18_446_744_073_709_551_617)
        );
        assert_eq!(document["min"], Value::Integer(-9_223_372_036_854_775_809));

        let error = parse(r#"{"value": 170141183460469231731687303715884105728}"#).unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Unrepresentable);
        assert!(
            !format!("{error}").contains("170141183460469231731687303715884105728"),
            "{error}"
        );
    }

    #[test]
    fn a_float_out_of_range_is_unrepresentable() {
        let error = parse(r#"{"value": 1e400}"#).unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Unrepresentable);
    }

    #[test]
    fn a_quoted_number_stays_a_string() {
        let document = map(r#"{"value": "18446744073709551617"}"#);
        assert_eq!(document["value"], Value::from("18446744073709551617"));
    }

    #[test]
    fn a_later_duplicate_member_wins() {
        let document = map(r#"{"port": 8080, "port": 9090}"#);
        assert_eq!(document["port"], Value::Integer(9090));
    }

    #[test]
    fn a_malformed_document_does_not_echo_its_text() {
        let error = parse(r#"{ "value": fixture-secret-9f3a2c }"#).unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Invalid);
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }
}
