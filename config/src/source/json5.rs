//! The opt-in JSON5 file source.
//!
//! JSON5 is a distinct dialect from JSONC. It adds comments, trailing commas, single-quoted
//! strings, unquoted member names, hexadecimal numbers, a unary plus, a bare leading or trailing
//! decimal point, `Infinity`/`NaN`, and multi-line strings. Those forms are accepted here; JSONC
//! accepts only comments beyond JSON.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::{SourceError, Value};

use super::{Source, file::read_text};

/// A JSON5 document read as one configuration source.
///
/// The dialect is JSON5 1.0 in UTF-8: comments, trailing commas, single-quoted strings, unquoted
/// member names, hexadecimal numbers, a unary plus, a bare decimal point, `Infinity`, `NaN`,
/// multi-line strings, and the dialect's Unicode whitespace are accepted. That whitespace set is
/// wider than ASCII and includes U+00A0, U+2028, U+2029, and U+FEFF.
///
/// The file is read again on every [`Source::load`]; there is no watcher and no cached document.
/// The document root must be an object. A later member with the same name replaces the earlier
/// one, and the resulting map is sorted by name. An array root, a scalar root, and an empty
/// document reject the source.
///
/// Numbers are exact where the value model allows it. An integer literal is held as
/// [`Value::Integer`] up to `i128`; a larger integer literal rejects the source instead of being
/// rounded. A finite float literal is held as [`Value::Float`]. `Infinity`, `-Infinity`, `NaN`,
/// and a decimal literal that overflows to infinity are rejected as unrepresentable.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read,
/// [`SourceError::invalid`] when it is not UTF-8, is not valid JSON5, or does not have an object
/// root, and [`SourceError::unrepresentable`] when a number does not fit the value model. The
/// error carries the kind only; it never carries a path, a line, a member name, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, Json5File};
///
/// // { http: { port: 0x1F90 }, } // unquoted names, hex, and a trailing comma
/// let config = Builder::new()
///     .json5_file("json5", Json5File::new("config/defaults.json5"))
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
pub struct Json5File {
    path: PathBuf,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; neither is reportable"
)]
impl fmt::Debug for Json5File {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.debug_struct("Json5File").finish_non_exhaustive()
    }
}

impl Json5File {
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

impl Source for Json5File {
    fn load(&self) -> Result<Value, SourceError> {
        let text = read_text(&self.path)?;
        let value = parse(&text)?;
        if value.as_map().is_none() {
            return Err(SourceError::invalid());
        }
        Ok(value)
    }
}

/// Parses one JSON5 document and keeps every number exact where the model can.
fn parse(text: &str) -> Result<Value, SourceError> {
    let mut tokens = Tokens::new(text);
    let mut deserializer = json5::Deserializer::from_str(text);
    let parsed = Json5Seed {
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
    match <de::IgnoredAny as de::Deserialize>::deserialize(&mut deserializer) {
        Err(error) if error.code() == Some(json5::ErrorCode::EofParsingValue) => Ok(value),
        _ => Err(SourceError::invalid()),
    }
}

/// One number token, taken from the document text in order.
enum NumberToken {
    /// An integer literal that fits the value model.
    Integer(i128),
    /// A number literal that does not fit the value model.
    Unrepresentable,
    /// A float literal, possibly non-finite. The builder reports a non-finite value.
    Float(f64),
}

/// The number tokens of one document, consumed in the order the parser visits them.
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
    /// The token, not the visitor's argument, decides the value: the parser reports an integer
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

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

/// The whitespace set of JSON5 1.0.
///
/// It is wider than ASCII: U+00A0, U+2028, U+2029, and U+FEFF (the zero-width no-break space) are
/// all whitespace to the parser. The scanner mirrors the parser's set, or a number next to one of
/// them would be swallowed by the identifier scan and the token count would disagree.
const fn is_json5_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}' | '\u{0020}' | '\u{00A0}' | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
    )
}

/// Advances past one identifier, including Unicode identifier characters.
///
/// JSON5 whitespace ends an identifier, so `Infinity\u{00A0}:` is the member name `Infinity`
/// followed by whitespace and a colon, not one long identifier.
fn skip_ident(text: &str, mut index: usize) -> usize {
    let bytes = text.as_bytes();
    while index < bytes.len() {
        if is_ident_continue(bytes[index]) {
            index += 1;
        } else if bytes[index] >= 0x80 {
            let Some(character) = text[index..].chars().next() else {
                break;
            };
            if is_json5_whitespace(character) {
                break;
            }
            index += character.len_utf8();
        } else {
            break;
        }
    }
    index
}

/// Scans the document for number tokens, outside strings and comments, in document order.
///
/// The document is parsed afterwards, so the scan only has to recognise the numbers the parser
/// will visit. An unquoted identifier that merely contains digits is skipped whole. A disagreement
/// is caught by comparing the token count at the end, and fails closed.
fn number_tokens(text: &str) -> Vec<NumberToken> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        // A non-ASCII character is either JSON5 whitespace or part of an identifier. Handling it
        // before the byte match keeps a UTF-8 continuation byte from being read as an identifier
        // start that then swallows the number after it.
        if bytes[index] >= 0x80 {
            let Some(character) = text[index..].chars().next() else {
                break;
            };
            if is_json5_whitespace(character) {
                index += character.len_utf8();
            } else {
                index = skip_ident(text, index);
            }
            continue;
        }
        match bytes[index] {
            b'"' | b'\'' => index = skip_string(bytes, index),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            byte if is_ident_start(byte) => {
                let start = index;
                index = skip_ident(text, index);
                let word = &text[start..index];
                if matches!(word, "Infinity" | "NaN") && next_significant(text, index) != Some(b':')
                {
                    tokens.push(NumberToken::Float(if word == "NaN" {
                        f64::NAN
                    } else {
                        f64::INFINITY
                    }));
                }
            }
            b'+' | b'-' | b'0'..=b'9' | b'.' => {
                index = scan_number(text, index, &mut tokens);
            }
            _ => index += 1,
        }
    }
    tokens
}

/// Advances past a quoted string, including a backslash escape and a line continuation.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    index
}

/// The next byte after JSON5 whitespace and comments, without moving the caller's cursor.
fn next_significant(text: &str, mut index: usize) -> Option<u8> {
    let bytes = text.as_bytes();
    loop {
        while index < bytes.len() {
            let character = text[index..].chars().next()?;
            if is_json5_whitespace(character) {
                index += character.len_utf8();
            } else {
                break;
            }
        }
        match (bytes.get(index), bytes.get(index + 1)) {
            (Some(b'/'), Some(b'/')) => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\n' | b'\r') {
                    index += 1;
                }
            }
            (Some(b'/'), Some(b'*')) => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            (byte, _) => return byte.copied(),
        }
    }
}

/// Scans one number starting at `start`, pushes its token, and returns the next index.
///
/// A malformed number pushes nothing; the parser reports it as invalid. A literal the model cannot
/// hold pushes `Unrepresentable`.
fn scan_number(text: &str, start: usize, tokens: &mut Vec<NumberToken>) -> usize {
    let bytes = text.as_bytes();
    let mut index = start;
    let negative = match bytes[index] {
        b'+' => {
            index += 1;
            false
        }
        b'-' => {
            index += 1;
            true
        }
        _ => false,
    };
    if index >= bytes.len() {
        return index;
    }

    if text[index..].starts_with("Infinity") {
        tokens.push(NumberToken::Float(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }));
        return index + "Infinity".len();
    }
    if text[index..].starts_with("NaN") {
        tokens.push(NumberToken::Float(f64::NAN));
        return index + "NaN".len();
    }

    if bytes[index] == b'0' && matches!(bytes.get(index + 1), Some(b'x' | b'X')) {
        index += 2;
        let digits_start = index;
        while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
            index += 1;
        }
        if digits_start == index {
            return index;
        }
        tokens.push(hex_token(&text[digits_start..index], negative));
        return index;
    }

    let mut is_float = false;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
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
    if index == start || (index == start + 1 && negative) {
        return index;
    }

    let literal = &text[start..index];
    if is_float {
        if let Ok(number) = literal.parse::<f64>() {
            tokens.push(NumberToken::Float(number));
        }
    } else if negative && literal.bytes().all(|byte| matches!(byte, b'-' | b'0')) {
        // JSON5 represents negative zero as a float, and so does this source.
        tokens.push(NumberToken::Float(-0.0));
    } else {
        tokens.push(
            literal
                .parse::<i128>()
                .map_or(NumberToken::Unrepresentable, NumberToken::Integer),
        );
    }
    index
}

/// Converts a hexadecimal literal exactly, without the parser's `u128`-then-float fallback.
fn hex_token(digits: &str, negative: bool) -> NumberToken {
    let Ok(magnitude) = u128::from_str_radix(digits, 16) else {
        return NumberToken::Unrepresentable;
    };
    if negative {
        if magnitude == 0 {
            return NumberToken::Float(-0.0);
        }
        if magnitude == i128::MIN.unsigned_abs() {
            return NumberToken::Integer(i128::MIN);
        }
        i128::try_from(magnitude).map_or(NumberToken::Unrepresentable, |value| {
            NumberToken::Integer(-value)
        })
    } else {
        i128::try_from(magnitude).map_or(NumberToken::Unrepresentable, NumberToken::Integer)
    }
}

/// Carries the token cursor through `json5`'s deserializer.
struct Json5Seed<'a> {
    tokens: &'a mut Tokens,
}

impl<'de> DeserializeSeed<'de> for Json5Seed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(Json5Visitor {
            tokens: self.tokens,
        })
    }
}

struct Json5Visitor<'a> {
    tokens: &'a mut Tokens,
}

impl<'de> Visitor<'de> for Json5Visitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON5 value")
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
        while let Some(item) = sequence.next_element_seed(Json5Seed {
            tokens: &mut *self.tokens,
        })? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(Json5Seed {
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
    fn reads_the_json5_extensions() {
        let document = map(r#"{
                // line comment
                unquoted: 'single quoted',
                hex: 0x1F90,
                plus: +42,
                leading: .5,
                trailing: 5.,
                trailingComma: [1, 2,],
                /* block comment */
                multi: "a\
b",
            }"#);
        assert_eq!(document["unquoted"], Value::from("single quoted"));
        assert_eq!(document["hex"], Value::Integer(8080));
        assert_eq!(document["plus"], Value::Integer(42));
        assert_eq!(document["leading"], Value::Float(0.5));
        assert_eq!(document["trailing"], Value::Float(5.0));
        assert_eq!(
            document["trailingComma"],
            Value::array([Value::Integer(1), Value::Integer(2)])
        );
        assert_eq!(document["multi"], Value::from("ab"));
    }

    #[test]
    fn keeps_integers_beyond_u64_exactly() {
        let document = map("{
                above: 18446744073709551617,
                max: 170141183460469231731687303715884105727,
                min: -170141183460469231731687303715884105728,
                hex: 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF,
            }");
        assert_eq!(
            document["above"],
            Value::Integer(18_446_744_073_709_551_617)
        );
        assert_eq!(document["max"], Value::Integer(i128::MAX));
        assert_eq!(document["min"], Value::Integer(i128::MIN));
        assert_eq!(document["hex"], Value::Integer(i128::MAX));
    }

    #[test]
    fn an_integer_beyond_i128_is_unrepresentable_not_rounded() {
        for document in [
            "{ value: 170141183460469231731687303715884105728 }",
            "{ value: 0x80000000000000000000000000000000 }",
        ] {
            let error = parse(document).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Unrepresentable, "{document}");
            assert!(!format!("{error}").contains("170141183460469231731687303715884105728"));
        }
    }

    #[test]
    fn non_finite_numbers_are_unrepresentable() {
        for document in [
            "{ value: Infinity }",
            "{ value: -Infinity }",
            "{ value: NaN }",
            "{ value: 1e400 }",
        ] {
            let error = parse(document).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Unrepresentable, "{document}");
        }
    }

    #[test]
    fn negative_zero_keeps_its_float_kind() {
        let document = map("{ value: -0 }");
        let Value::Float(number) = document["value"] else {
            panic!("expected a float");
        };
        assert_eq!(number.to_bits(), (-0.0_f64).to_bits());
    }

    #[test]
    fn an_infinity_member_name_is_not_mistaken_for_a_number_token() {
        let document = map("{ Infinity: 1, NaN: 2 }");
        assert_eq!(document["Infinity"], Value::Integer(1));
        assert_eq!(document["NaN"], Value::Integer(2));
    }

    #[test]
    fn accepts_json5_unicode_whitespace_around_numbers() {
        let document = map("{ port:\u{00A0}8080, big:\u{2028}18446744073709551617, \
             hex:\u{2029}0x1F90, plus:\u{00A0}+42, negative:\u{2029}-42 }");
        assert_eq!(document["port"], Value::Integer(8080));
        assert_eq!(document["big"], Value::Integer(18_446_744_073_709_551_617));
        assert_eq!(document["hex"], Value::Integer(8080));
        assert_eq!(document["plus"], Value::Integer(42));
        assert_eq!(document["negative"], Value::Integer(-42));
    }

    #[test]
    fn a_unicode_whitespace_before_a_member_colon_does_not_make_it_a_number() {
        let document = map("{ Infinity\u{00A0}: 1, NaN\u{2028}: 2 }");
        assert_eq!(document["Infinity"], Value::Integer(1));
        assert_eq!(document["NaN"], Value::Integer(2));
    }

    #[test]
    fn a_number_after_unicode_whitespace_keeps_its_kind() {
        let document = map("{ whole:\u{00A0}8080, fraction:\u{00A0}8080.5 }");
        assert_eq!(document["whole"], Value::Integer(8080));
        assert_eq!(document["fraction"], Value::Float(8080.5));
    }

    #[test]
    fn a_malformed_document_does_not_echo_its_text() {
        let error = parse("{ value: fixture-secret-9f3a2c }").unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Invalid);
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }

    #[test]
    fn a_second_value_after_the_first_is_invalid() {
        assert!(parse("{ a: 1 } { b: 2 }").is_err());
        assert!(parse("{ a: 1 } trailing").is_err());
    }

    #[test]
    fn a_later_duplicate_member_wins() {
        let document = map("{ port: 8080, port: 9090 }");
        assert_eq!(document["port"], Value::Integer(9090));
    }
}
