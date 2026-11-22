//! The opt-in TOML file source.
//!
//! The conversion follows the plab rs-lib `neotales-config` TOML source
//! (MIT, Copyright (c) 2026 Neotales), rewritten for this crate's error type.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use crate::{SourceError, Value, value::DateTime};

use super::{Source, file::read_text};

/// A TOML document read as one configuration source.
///
/// The dialect is TOML as implemented by the `toml` crate 0.9 (spec 1.1.0), in UTF-8. A key is
/// used literally for its table; the source never splits a dotted key further, because TOML has
/// already done that. The file is read again on every [`Source::load`]; there is no watcher and no
/// cached document.
///
/// By default the file is required and its path is watched. [`Self::optional`] and
/// [`Self::reload_on_change`] change those two defaults independently: an optional source treats
/// only a genuine not-found as an empty document, and a source with change watching off removes
/// only its own contribution to the watched path list. A poll loads every source when any watched
/// path changes, and the path stays watched if another source reports it.
///
/// The document root is always a table, so an empty document is an empty map. A duplicate key or
/// table is a TOML error and rejects the source; TOML forbids a later definition from redefining
/// an existing key. A TOML date, time, or offset date-time becomes [`Value::DateTime`] with its
/// canonical RFC 3339 spelling, not a string.
///
/// An integer is held as [`Value::Integer`]. A float is held as [`Value::Float`]; `nan`, `inf`,
/// and `-inf` are not finite and reject the source as unrepresentable.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read (including when it is
/// absent, unless [`Self::optional`] is enabled), [`SourceError::invalid`] when it is not UTF-8
/// or not valid TOML, and [`SourceError::unrepresentable`] when a float is not finite. The error
/// carries the kind only; it never carries a path, a line, a key, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, DateTime, TomlFile};
///
/// // [http]
/// // port = 8080
/// // started = 1979-05-27T07:32:00Z
/// let config = Builder::new()
///     .toml_file("toml", TomlFile::new("config/defaults.toml"))
///     .build()?;
/// assert_eq!(config.get::<u16>("http.port")?, 8080);
/// assert_eq!(
///     config.get::<DateTime>("http.started")?.as_str(),
///     "1979-05-27T07:32:00Z"
/// );
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; a configuration file may hold a secret. Its
/// error and its [`fmt::Debug`] omit the path and the contents.
#[derive(Clone)]
pub struct TomlFile {
    path: PathBuf,
    optional: bool,
    reload_on_change: bool,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; only the two \
              behavior flags are reportable"
)]
impl fmt::Debug for TomlFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("TomlFile")
            .field("optional", &self.optional)
            .field("reload_on_change", &self.reload_on_change)
            .finish_non_exhaustive()
    }
}

impl TomlFile {
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

impl Source for TomlFile {
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

/// Parses one TOML document, preserving date-times and rejecting non-finite floats.
fn parse(text: &str) -> Result<Value, SourceError> {
    let table: toml::Table = text.parse().map_err(|_| SourceError::invalid())?;
    convert_table(&table)
}

fn convert_table(table: &toml::Table) -> Result<Value, SourceError> {
    let mut map = BTreeMap::new();
    for (key, value) in table {
        map.insert(key.clone(), convert(value)?);
    }
    Ok(Value::Map(map))
}

fn convert(value: &toml::Value) -> Result<Value, SourceError> {
    Ok(match value {
        toml::Value::String(text) => Value::String(text.clone()),
        toml::Value::Integer(integer) => Value::Integer(i128::from(*integer)),
        toml::Value::Float(float) => {
            if !float.is_finite() {
                return Err(SourceError::unrepresentable());
            }
            Value::Float(*float)
        }
        toml::Value::Boolean(flag) => Value::Bool(*flag),
        toml::Value::Datetime(date_time) => Value::DateTime(DateTime::new(date_time.to_string())),
        toml::Value::Array(items) => {
            let mut converted = Vec::with_capacity(items.len());
            for item in items {
                converted.push(convert(item)?);
            }
            Value::Array(converted)
        }
        toml::Value::Table(table) => convert_table(table)?,
    })
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::{SourceErrorKind, Value};

    fn map(text: &str) -> std::collections::BTreeMap<String, Value> {
        parse(text)
            .expect("the document parses")
            .as_map()
            .expect("the document is a table")
            .clone()
    }

    #[test]
    fn preserves_dates_times_and_offsets() {
        let document = map("\
when = 1979-05-27T07:32:00Z
date = 1979-05-27
time = 07:32:00.5
local = 1979-05-27T07:32:00
");
        let Value::DateTime(when) = &document["when"] else {
            panic!("expected a date-time");
        };
        assert_eq!(when.as_str(), "1979-05-27T07:32:00Z");
        let Value::DateTime(date) = &document["date"] else {
            panic!("expected a date");
        };
        assert_eq!(date.as_str(), "1979-05-27");
        let Value::DateTime(time) = &document["time"] else {
            panic!("expected a time");
        };
        assert_eq!(time.as_str(), "07:32:00.5");
        let Value::DateTime(local) = &document["local"] else {
            panic!("expected a local date-time");
        };
        assert_eq!(local.as_str(), "1979-05-27T07:32:00");
    }

    #[test]
    fn reads_nested_tables_and_arrays() {
        let document = map("\
[server]
port = 8080
hosts = [\"a\", \"b\"]

[server.tls]
enabled = true
");
        let server = document["server"].as_map().unwrap();
        assert_eq!(server["port"], Value::Integer(8080));
        assert_eq!(
            server["hosts"],
            Value::array([Value::from("a"), Value::from("b")])
        );
        assert_eq!(
            server["tls"].as_map().unwrap()["enabled"],
            Value::Bool(true)
        );
    }

    #[test]
    fn rejects_non_finite_floats_as_unrepresentable() {
        for text in ["value = nan\n", "value = inf\n", "value = -inf\n"] {
            let error = parse(text).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Unrepresentable, "{text}");
            assert!(!format!("{error}").contains("nan"));
        }
    }

    #[test]
    fn rejects_duplicate_keys_and_syntax_errors() {
        for text in [
            "port = 1\nport = 2\n",
            "[server]\nport = 1\n[server]\nhost = \"a\"\n",
            "value =\n",
            "value = fixture-secret-9f3a2c\n",
        ] {
            let error = parse(text).unwrap_err();
            assert_eq!(error.kind(), SourceErrorKind::Invalid, "{text}");
            assert!(!format!("{error:?}").contains("fixture-secret"));
        }
    }

    #[test]
    fn a_quoted_integer_stays_a_string() {
        let document = map("value = \"8080\"\n");
        assert_eq!(document["value"], Value::from("8080"));
    }
}
