//! The opt-in YAML file source.
//!
//! The parser shape follows the plab rs-lib `neotales-config` YAML source
//! (MIT, Copyright (c) 2026 Neotales), rewritten for this crate's error type.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use saphyr::{Scalar, ScalarStyle};
use saphyr_parser::{Event, Parser, SpannedEventReceiver};

use crate::{SourceError, Value};

use super::{Source, file::read_text};

/// A YAML document read as one configuration source.
///
/// The dialect is YAML 1.2 core schema in UTF-8. The parser is driven event by event, which is
/// what makes every rejection explicit rather than silent. Rejected: an anchor, an alias, an
/// explicit tag, a mapping key that is not a string, a duplicate mapping key, an empty document,
/// and more than one document in the file. A merge key is a tag or an alias in practice and is
/// rejected for the same reason.
///
/// A plain scalar follows the YAML 1.2 core schema: `true`/`false`, `null`/`~`, decimal,
/// hexadecimal (`0x`), and octal (`0o`) integers, and finite floats. A quoted scalar is always a
/// string, so `"8080"` stays a string. A plain scalar that is not an integer literal is resolved
/// by the parser; `yes` and `no` are strings under YAML 1.2.
///
/// The file is read again on every [`Source::load`]; there is no watcher and no cached document.
/// The document root must be a mapping. A sequence root, a scalar root, and an empty document
/// reject the source.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read,
/// [`SourceError::invalid`] when it is not UTF-8, is not valid YAML, has an anchor, alias, tag,
/// non-string key, duplicate key, or a root that is not a mapping, and
/// [`SourceError::unrepresentable`] when a number does not fit the value model. The error carries
/// the kind only; it never carries a path, a line, a key, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, YamlFile};
///
/// let config = Builder::new()
///     .yaml_file("yaml", YamlFile::new("config/defaults.yaml"))
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
pub struct YamlFile {
    path: PathBuf,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; neither is reportable"
)]
impl fmt::Debug for YamlFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.debug_struct("YamlFile").finish_non_exhaustive()
    }
}

impl YamlFile {
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

impl Source for YamlFile {
    fn load(&self) -> Result<Value, SourceError> {
        let text = read_text(&self.path)?;
        let value = parse(&text)?;
        if value.as_map().is_none() {
            return Err(SourceError::invalid());
        }
        Ok(value)
    }
}

/// Parses one YAML document into the value model.
fn parse(text: &str) -> Result<Value, SourceError> {
    let mut parser = Parser::new_from_str(text);
    let mut receiver = Receiver::default();
    parser
        .load(&mut receiver, true)
        .map_err(|_| SourceError::invalid())?;
    if receiver.failed {
        return Err(SourceError::invalid());
    }
    if receiver.unrepresentable {
        return Err(SourceError::unrepresentable());
    }
    let mut documents = receiver.documents;
    if documents.len() != 1 {
        return Err(SourceError::invalid());
    }
    documents.pop().ok_or_else(SourceError::invalid)
}

/// The exact value of a YAML 1.2 core-schema integer literal.
///
/// `saphyr` resolves a plain scalar to `i64`, and only then falls back to `f64`, so a larger
/// integer would be rounded (`9223372036854775809` becomes `9223372036854775808`). The core-schema
/// integer forms are parsed here instead, into the value model's `i128`.
///
/// `None` means the scalar is not an integer literal, so the parser's own resolution applies.
/// `Some(Err(()))` means it is one but its digits do not fit the model.
fn integer_literal(text: &str) -> Option<Result<i128, ()>> {
    if let Some(digits) = text.strip_prefix("0x") {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        return Some(i128::from_str_radix(digits, 16).map_err(|_| ()));
    }
    if let Some(digits) = text.strip_prefix("0o") {
        if digits.is_empty() || !digits.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
            return None;
        }
        return Some(i128::from_str_radix(digits, 8).map_err(|_| ()));
    }
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(text.parse::<i128>().map_err(|_| ()))
}

/// One open collection while the event stream is read. A mapping keeps its own pending key, so a
/// nested mapping cannot lose the key its parent is waiting for.
enum Container {
    Sequence(Vec<Value>),
    Mapping {
        map: BTreeMap<String, Value>,
        key: Option<String>,
    },
}

#[derive(Default)]
struct Receiver {
    documents: Vec<Value>,
    stack: Vec<Container>,
    document: Option<Value>,
    failed: bool,
    unrepresentable: bool,
}

impl Receiver {
    /// Attaches one finished node to the open collection, or records it as the document root.
    fn attach(&mut self, value: Value) {
        match self.stack.last_mut() {
            None => {
                if self.document.is_some() {
                    self.failed = true;
                    return;
                }
                self.document = Some(value);
            }
            Some(Container::Sequence(items)) => items.push(value),
            Some(Container::Mapping { map, key }) => match key.take() {
                Some(key) => {
                    if map.insert(key, value).is_some() {
                        // A duplicate key would silently drop one of the two values.
                        self.failed = true;
                    }
                }
                None => match value {
                    Value::String(text) => *key = Some(text),
                    // A non-string key cannot be addressed by a string path.
                    Value::Null
                    | Value::Bool(_)
                    | Value::Integer(_)
                    | Value::Float(_)
                    | Value::DateTime(_)
                    | Value::Array(_)
                    | Value::Map(_) => self.failed = true,
                },
            },
        }
    }

    fn scalar(&mut self, text: &str, style: ScalarStyle) {
        if style == ScalarStyle::Plain {
            match integer_literal(text) {
                Some(Ok(integer)) => {
                    self.attach(Value::Integer(integer));
                    return;
                }
                Some(Err(())) => {
                    // The parser would resolve this to a rounded float.
                    self.unrepresentable = true;
                    return;
                }
                None => {}
            }
        }
        let Some(resolved) =
            Scalar::parse_from_cow_and_metadata(std::borrow::Cow::Borrowed(text), style, None)
        else {
            self.failed = true;
            return;
        };
        let value = match resolved {
            Scalar::Null => Value::Null,
            Scalar::Boolean(flag) => Value::Bool(flag),
            Scalar::Integer(integer) => Value::Integer(i128::from(integer)),
            Scalar::FloatingPoint(float) => {
                let float = float.into_inner();
                if !float.is_finite() {
                    self.unrepresentable = true;
                    return;
                }
                Value::Float(float)
            }
            Scalar::String(text) => Value::String(text.into_owned()),
        };
        self.attach(value);
    }
}

impl<'input> SpannedEventReceiver<'input> for Receiver {
    fn on_event(&mut self, event: Event<'input>, _span: saphyr_parser::Span) {
        if self.failed || self.unrepresentable {
            return;
        }
        match event {
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentStart(_) => {}
            Event::DocumentEnd => match self.document.take() {
                Some(value) => self.documents.push(value),
                None => self.failed = true,
            },
            Event::Alias(_) => self.failed = true,
            Event::Scalar(text, style, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.failed = true;
                    return;
                }
                self.scalar(text.as_ref(), style);
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.failed = true;
                    return;
                }
                self.stack.push(Container::Sequence(Vec::new()));
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.failed = true;
                    return;
                }
                self.stack.push(Container::Mapping {
                    map: BTreeMap::new(),
                    key: None,
                });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let Some(container) = self.stack.pop() else {
                    self.failed = true;
                    return;
                };
                let value = match container {
                    Container::Sequence(items) => Value::Array(items),
                    Container::Mapping { map, .. } => Value::Map(map),
                };
                self.attach(value);
            }
        }
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
            .expect("the document is a mapping")
            .clone()
    }

    #[test]
    fn reads_yaml_12_core_scalars() {
        let document = map("port: 8080\nhost: localhost\nratio: 0.5\non: true\nnothing: ~\n");
        assert_eq!(document["port"], Value::Integer(8080));
        assert_eq!(document["host"], Value::from("localhost"));
        assert_eq!(document["ratio"], Value::Float(0.5));
        assert_eq!(document["on"], Value::Bool(true));
        assert_eq!(document["nothing"], Value::Null);
    }

    #[test]
    fn yaml_12_treats_yes_and_no_as_strings() {
        let document = map("yes: maybe\nmaybe: yes\n");
        assert_eq!(document["maybe"], Value::from("yes"));
    }

    #[test]
    fn reads_nested_mappings_and_sequences() {
        let document = map("http:\n  port: 8080\n  hosts:\n    - a\n    - b\nlist: [1, 2]\n");
        let http = document["http"].as_map().unwrap();
        assert_eq!(http["port"], Value::Integer(8080));
        assert_eq!(
            http["hosts"],
            Value::array([Value::from("a"), Value::from("b")])
        );
        assert_eq!(document["list"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rejects_aliases_anchors_tags_and_non_string_keys() {
        for document in [
            "a: &anchor 1\nb: *anchor\n",
            "a: !custom 1\n",
            "1: one\n",
            "? [a, b]\n: value\n",
            "a: &anchor\n  b: 1\n",
            "a: &anchor [1, 2]\n",
            "a: !custom\n  b: 1\n",
            "a: !!str 1\n",
            "base: &base { a: 1 }\nchild: { <<: *base }\n",
        ] {
            let error = parse(document).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Invalid, "{document}");
        }
    }

    #[test]
    fn rejects_duplicates_and_multiple_documents() {
        for document in [
            "a: 1\na: 2\n",
            "outer:\n  a: 1\n  a: 2\n",
            "a: 1\n---\nb: 2\n",
            "",
        ] {
            assert!(parse(document).is_err(), "{document}");
        }
    }

    #[test]
    fn keeps_every_integer_the_model_can_hold() {
        let cases = [
            ("9223372036854775809", 9_223_372_036_854_775_809_i128),
            ("18446744073709551615", i128::from(u64::MAX)),
            ("18446744073709551617", 18_446_744_073_709_551_617),
            ("170141183460469231731687303715884105727", i128::MAX),
            ("-170141183460469231731687303715884105728", i128::MIN),
            ("0xFFFFFFFFFFFFFFFF", i128::from(u64::MAX)),
            ("0o1777777777777777777777", i128::from(u64::MAX)),
        ];
        for (text, expected) in cases {
            let document = map(&format!("value: {text}\n"));
            assert_eq!(document["value"], Value::Integer(expected), "for {text}");
        }
    }

    #[test]
    fn an_integer_beyond_i128_is_unrepresentable_not_rounded() {
        for text in [
            "170141183460469231731687303715884105728",
            "-170141183460469231731687303715884105729",
            "0x80000000000000000000000000000000",
        ] {
            let error = parse(&format!("value: {text}\n")).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Unrepresentable, "{text}");
            assert!(!format!("{error}").contains(text));
        }
    }

    #[test]
    fn non_finite_floats_are_unrepresentable() {
        for document in ["value: .inf\n", "value: -.inf\n", "value: .nan\n"] {
            let error = parse(document).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Unrepresentable, "{document}");
        }
    }

    #[test]
    fn a_quoted_number_stays_a_string() {
        let document = map("value: '9223372036854775809'\n");
        assert_eq!(document["value"], Value::from("9223372036854775809"));
    }
}
