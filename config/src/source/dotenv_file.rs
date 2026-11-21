//! The opt-in dotenv file source.
//!
//! The file is read and parsed by `neotales-dotenv`; this crate only maps assignments into a
//! value tree. Nothing is written to the process environment, and no value is interpolated.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use crate::{SourceError, Value};

use super::{
    Source,
    keys::{Keys, insert},
};

/// A dotenv file read as one configuration source.
///
/// The file is read once, at build. It is parsed as a dotenv document, never as a shell script:
/// `export KEY=value`, single-quoted, double-quoted, and backtick values are accepted, and a
/// value is never interpolated. A line whose first non-whitespace character is `#` is a comment;
/// an unquoted `#` ends a value and starts an inline comment, while a `#` inside a quoted value
/// stays literal. Every value stays a string, including a numeric or boolean spelling.
///
/// A selected key is split into path components. With [`Self::prefix`] `APP_`, the builder's
/// delimiter `.`, and a file containing `APP_http.port=8080`, the result is the nested map
/// `{"http": {"port": "8080"}}`. A later assignment to the same path wins; a line that is not an
/// assignment, an empty key, an empty path component, an empty separator, a non-UTF-8 file, and a
/// scalar/map conflict each reject the source.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read and
/// [`SourceError::invalid`] when it is not UTF-8 or does not parse. The key policy is checked
/// before the file is opened, so an empty separator is invalid even when the file is missing or
/// holds no assignment. The error carries the kind only; it never carries a path, a line, or a
/// value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, DotenvFile};
///
/// let config = Builder::new()
///     .dotenv_file("dotenv", DotenvFile::new("config/.env").prefix("APP_"))
///     .build()?;
/// assert_eq!(config.get::<String>("http.port")?, "8080");
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; a dotenv file usually holds secrets. Its error
/// and its [`fmt::Debug`] omit the path and the contents.
#[derive(Clone)]
pub struct DotenvFile {
    path: PathBuf,
    keys: Keys,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the contents may hold secrets; neither is reportable"
)]
impl fmt::Debug for DotenvFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("DotenvFile")
            .field("keys", &self.keys)
            .finish()
    }
}

impl DotenvFile {
    /// Registers `path` to be read when the source loads.
    ///
    /// The path is opened once, at build. It is never followed into another file and never
    /// written.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            keys: Keys::default(),
        }
    }

    /// Keeps only assignments whose key starts with `prefix`, and removes it from every selected
    /// key.
    ///
    /// An empty prefix selects every assignment. A key that equals the prefix has an empty key
    /// and is rejected.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.keys = self.keys.prefix(prefix);
        self
    }

    /// Splits a selected key on `separator` into nested maps.
    ///
    /// An explicit separator overrides the builder's delimiter. An empty separator rejects the
    /// source when it loads, even when the file holds no selected assignment.
    #[must_use]
    pub fn separator(mut self, separator: impl Into<String>) -> Self {
        self.keys = self.keys.separator(separator);
        self
    }

    /// Keeps a selected key as one component instead of splitting it.
    #[must_use]
    pub fn flat(mut self) -> Self {
        self.keys = self.keys.flat();
        self
    }

    /// Uses `delimiter` as the default key separator.
    ///
    /// [`crate::Builder::dotenv_file`] replaces it with the builder's final delimiter when the
    /// snapshot is built, whatever order the two calls were made in. Registering the source
    /// through [`crate::Builder::source`] keeps this value; that path keeps `.` unless this
    /// method, an explicit separator, or [`Self::flat`] is used.
    #[must_use]
    pub fn delimiter(mut self, delimiter: char) -> Self {
        self.keys = self.keys.delimiter(delimiter);
        self
    }

    /// Returns the configured path.
    ///
    /// The path is not part of any error or of this type's [`fmt::Debug`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn build(&self) -> Result<Value, SourceError> {
        // Check the policy before the file is read: an empty separator is a caller error even when
        // the file is empty or the prefix selects none of its assignments.
        self.keys.validate().map_err(|_| SourceError::invalid())?;

        let bytes = std::fs::read(&self.path).map_err(|_| SourceError::unavailable())?;
        let text = String::from_utf8(bytes).map_err(|_| SourceError::invalid())?;
        let document = neotales_dotenv::parse(&text);
        if document.first_invalid().is_some() {
            return Err(SourceError::invalid());
        }

        let mut root = BTreeMap::new();
        for assignment in document.assignments() {
            if assignment.key.is_empty() {
                return Err(SourceError::invalid());
            }
            let Some(components) = self
                .keys
                .components(&assignment.key)
                .map_err(|_| SourceError::invalid())?
            else {
                continue;
            };
            insert(
                &mut root,
                &components,
                Value::String(assignment.value.clone()),
            )
            .map_err(|_| SourceError::invalid())?;
        }
        Ok(Value::Map(root))
    }
}

impl Source for DotenvFile {
    fn load(&self) -> Result<Value, SourceError> {
        self.build()
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        vec![self.path.clone()]
    }
}
