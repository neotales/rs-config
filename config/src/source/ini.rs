//! The opt-in INI file source.
//!
//! The parser shape follows the plab rs-lib `neotales-config` INI source
//! (MIT, Copyright (c) 2026 Neotales), rewritten for this crate's error type.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    path::{Path, PathBuf},
};

use crate::{SourceError, Value};

use super::{Source, file::read_text};

/// An INI document read as one configuration source.
///
/// The dialect is the plain INI this crate defines, not every INI variant in the wild:
///
/// - A line whose first non-whitespace character is `#` or `;` is a comment. An inline `#` or `;`
///   is part of the value; it never starts a comment.
/// - A line `[name]` starts a section. The name is trimmed and must not be empty. Everything
///   between the first `[` and the last `]` is the name, and a section name is one literal key, so
///   `[a.b]` creates the key `a.b` and not a nested map.
/// - A line `key = value` assigns a string. The first `=` separates the key from the value; both
///   are trimmed, the key must not be empty, and the value may be empty. One pair of matching
///   wrapping single or double quotes is removed; no escape sequence and no interpolation is
///   interpreted.
/// - An assignment before the first section is a root key. An assignment after it belongs to the
///   current section.
/// - Every other line, including a bare word and an unterminated section header, rejects the
///   document.
///
/// Every value is a string, including a numeric or boolean spelling. A later assignment to the
/// same key wins, so a repeated key in one section keeps its last value. A repeated section header
/// continues the existing section rather than replacing it, and later assignments merge into it;
/// this is the deterministic duplicate rule. A root scalar and a section with the same name reject
/// the document, because one path cannot be both.
///
/// The file is read again on every [`Source::load`]; there is no watcher and no cached document.
/// A leading UTF-8 byte-order mark is ignored. The document root is always the map of sections
/// and root keys.
///
/// By default the file is required and its path is watched. [`Self::optional`] and
/// [`Self::reload_on_change`] change those two defaults independently: an optional source treats
/// only a genuine not-found as an empty document, and a source with change watching off removes
/// only its own contribution to the watched path list. A poll loads every source when any watched
/// path changes, and the path stays watched if another source reports it.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read (including when it is
/// absent, unless [`Self::optional`] is enabled) and [`SourceError::invalid`] when it is not
/// UTF-8, contains a line outside the dialect, an empty key, an empty section name, or a
/// scalar/section conflict. The error carries the kind only; it never carries a path, a line, a
/// key, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, IniFile};
///
/// // root = 1
/// // [server]
/// // port = 8080
/// let config = Builder::new()
///     .ini_file("ini", IniFile::new("config/defaults.ini"))
///     .build()?;
/// assert_eq!(config.get::<String>("server.port")?, "8080");
/// // Every INI value stays a string, so an integer read is a type mismatch.
/// assert!(config.get::<u16>("server.port").unwrap_err().is_type_mismatch());
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; a configuration file may hold a secret. Its
/// error and its [`fmt::Debug`] omit the path and the contents.
#[derive(Clone)]
pub struct IniFile {
    path: PathBuf,
    optional: bool,
    reload_on_change: bool,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; only the two \
              behavior flags are reportable"
)]
impl fmt::Debug for IniFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("IniFile")
            .field("optional", &self.optional)
            .field("reload_on_change", &self.reload_on_change)
            .finish_non_exhaustive()
    }
}

impl IniFile {
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
    /// a directory, a permission failure, a non-UTF-8 file, a parse failure, and a value the model
    /// cannot hold all still reject the source. The open result itself decides: there is no
    /// separate existence check for a file to appear or vanish between.
    ///
    /// An optional file that is absent may appear later, and a watched optional file that
    /// disappears is a change the next refresh observes: the values it contributed are dropped and
    /// any value from an earlier source shows through again. A parse failure is not an absence; it
    /// fails the refresh and keeps the previous snapshot published.
    #[must_use]
    pub const fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Reports the configured path to the polling watcher.
    ///
    /// The default is `true`. With `false`, [`Source::watch_paths`] returns an empty list for this
    /// source, so a poll does not watch the file on this source's account. That removes **only this
    /// source's contribution** to the watched path list: a poll that loads for any other reason
    /// still loads every source, including this one, and a manual `Reload::refresh` always loads
    /// every source. If another source reports the same path, the path stays watched and a change
    /// to it is observed. If no source reports a path at all, `Reload::start_watching` returns
    /// `WatchError::NoWatchableSources`.
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

impl Source for IniFile {
    fn load(&self) -> Result<Value, SourceError> {
        let Some(text) = read_text(&self.path, self.optional)? else {
            return Ok(Value::Map(BTreeMap::new()));
        };
        parse(&text)
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        if self.reload_on_change {
            vec![self.path.clone()]
        } else {
            Vec::new()
        }
    }
}

/// Parses one INI document into a map of root keys and sections.
fn parse(text: &str) -> Result<Value, SourceError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut root: BTreeMap<String, Value> = BTreeMap::new();
    let mut section: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(inner) = trimmed.strip_prefix('[') {
            let Some(name) = inner.strip_suffix(']') else {
                return Err(SourceError::invalid());
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(SourceError::invalid());
            }
            match root.entry(name.to_owned()) {
                Entry::Occupied(entry) => {
                    // A repeated section continues the existing map; a scalar with the same name
                    // cannot also become a section.
                    if !matches!(entry.get(), Value::Map(_)) {
                        return Err(SourceError::invalid());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(Value::Map(BTreeMap::new()));
                }
            }
            section = Some(name.to_owned());
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(SourceError::invalid());
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(SourceError::invalid());
        }
        let value = Value::String(unquote(value.trim()));

        match &section {
            Some(section) => {
                let Some(Value::Map(map)) = root.get_mut(section) else {
                    return Err(SourceError::invalid());
                };
                // A later assignment to the same key wins, in file order.
                map.insert(key.to_owned(), value);
            }
            None => match root.entry(key.to_owned()) {
                Entry::Occupied(mut entry) => {
                    // A root assignment may replace an earlier root scalar, but never a section.
                    if matches!(entry.get(), Value::Map(_)) {
                        return Err(SourceError::invalid());
                    }
                    entry.insert(value);
                }
                Entry::Vacant(entry) => {
                    entry.insert(value);
                }
            },
        }
    }

    Ok(Value::Map(root))
}

/// Removes one pair of matching wrapping quotes. No escape is interpreted.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[quote.len_utf8()..value.len() - quote.len_utf8()].to_owned();
        }
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{SourceErrorKind, Value};

    fn map(text: &str) -> std::collections::BTreeMap<String, Value> {
        parse(text)
            .expect("the document parses")
            .as_map()
            .expect("the document is a map")
            .clone()
    }

    #[test]
    fn reads_sections_root_keys_and_string_values() {
        let text =
            "\u{feff}# comment\n; other\nroot = 1\n\n[server]\nport = 8080\nname = \"neo env\"\n";
        let document = map(text);
        assert_eq!(document["root"], Value::from("1"));
        let server = document["server"].as_map().unwrap();
        assert_eq!(server["port"], Value::from("8080"));
        assert_eq!(server["name"], Value::from("neo env"));
    }

    #[test]
    fn keeps_dotted_names_whole_and_does_not_strip_inline_comments() {
        let document = map("[a.b]\nvalue = x # literal\n");
        assert!(document.contains_key("a.b"));
        assert_eq!(
            document["a.b"].as_map().unwrap()["value"],
            Value::from("x # literal")
        );
    }

    #[test]
    fn a_later_assignment_wins_and_a_repeated_section_merges() {
        let document = map("[s]\nk = 1\nk = 2\n[t]\na = 1\n[s]\nb = 3\n");
        assert_eq!(document["s"].as_map().unwrap()["k"], Value::from("2"));
        assert_eq!(document["s"].as_map().unwrap()["b"], Value::from("3"));
        assert_eq!(document["t"].as_map().unwrap()["a"], Value::from("1"));
    }

    #[test]
    fn quotes_and_empty_values_are_defined() {
        let document = map("a = ''\nb = \"\"\nc = \"  \"\nd = \"x'\n");
        assert_eq!(document["a"], Value::from(""));
        assert_eq!(document["b"], Value::from(""));
        assert_eq!(document["c"], Value::from("  "));
        assert_eq!(document["d"], Value::from("\"x'"));
    }

    #[test]
    fn values_are_not_interpolated_and_keep_later_equals_signs() {
        let document = map(
            "home = ${HOME}\nappdata = %APPDATA%\nshell = $(pwd)\nquery = a=b=c\nkey = 1\nkey = 2\n",
        );
        assert_eq!(document["home"], Value::from("${HOME}"));
        assert_eq!(document["appdata"], Value::from("%APPDATA%"));
        assert_eq!(document["shell"], Value::from("$(pwd)"));
        assert_eq!(document["query"], Value::from("a=b=c"));
        assert_eq!(document["key"], Value::from("2"));
    }

    #[test]
    fn bad_lines_empty_names_and_conflicts_are_invalid() {
        for text in [
            "this is not an assignment\n",
            "[unterminated\n",
            "[]\n",
            "= value\n",
            "[s]\n= value\n",
            "s = 1\n[s]\na = 2\n",
            "a = 1\nb\n",
        ] {
            let error = parse(text).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Invalid, "{text}");
        }
    }

    #[test]
    fn an_error_never_carries_a_line_or_value() {
        let error = parse("fixture-secret-9f3a2c\n").unwrap_err();
        assert_eq!(error.kind(), SourceErrorKind::Invalid);
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }
}
