//! The opt-in dotenv file source.
//!
//! The file is read and parsed by `neotales-dotenv`; this crate maps assignments into a value
//! tree and expands `${NAME}` references with `neotales-env`. Expansion is sequential: an earlier
//! assignment is visible to a later one, and the process environment is the fallback. Nothing is
//! written to the process environment.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use neotales_dotenv::QuoteStyle;
use neotales_env::ExpandOptions;

use crate::{SourceError, Value};

use super::{
    Source,
    keys::{Keys, insert},
};

/// A dotenv file read as one configuration source.
///
/// The file is read once per load and parsed as a dotenv document, never as a shell script.
/// `export KEY=value`, unquoted, single-quoted, double-quoted, and backtick values are accepted.
/// Every value stays a string after expansion, including a numeric or boolean spelling.
///
/// Assignment order is significant. Each assignment is expanded in source order before the prefix
/// selects it, and its expanded value is kept under its exact key for later assignments. A lookup
/// checks those earlier values first and then the process environment. An unselected assignment
/// therefore still affects a later selected one, a duplicate key sees the earlier value before
/// replacing it, and a forward reference reads the environment because the later assignment has
/// not run yet. [`Self::with_environment`] replaces the process lookup with a fixed set, which is
/// how a test or a captured environment stays out of the process.
///
/// Interpolation is on by default. [`Self::interpolate`] with `false` keeps every value literal,
/// for a document that is not a template. An unquoted or double-quoted value expands; a
/// single-quoted or backtick value stays literal. The accepted forms are `$NAME`, `${NAME}`,
/// `${NAME:-text}`, `${NAME:text}`, `${NAME:=text}`, and `${NAME:?text}`. `$$` and `\$` produce a
/// literal `$`. `${NAME:=text}` stores `text` only in the per-load local scope, so a later
/// assignment can read it; it never writes the process environment. Command substitution, Windows
/// `%NAME%` references, arithmetic, word splitting, globbing, and the other Bash parameter forms
/// are not evaluated. See [Interpolation](#interpolation) for the exact subset and its escape
/// limitations.
///
/// A selected key is split into path components. With [`Self::prefix`] `APP_`, the builder's
/// delimiter `.`, and a file containing `APP_http.port=8080`, the result is the nested map
/// `{"http": {"port": "8080"}}`. A later assignment to the same path wins; a line that is not an
/// assignment, an empty key, an empty path component, an empty separator, a non-UTF-8 file, and a
/// scalar/map conflict each reject the source.
///
/// # Interpolation
///
/// The parser runs before expansion: it removes the outer quotes and decodes the escapes it
/// defines, so expansion sees the decoded text. An unquoted or double-quoted `\$NAME` reaches the
/// expander as `\$NAME` and becomes the literal `$NAME`. A single-quoted value is not expanded at
/// all, though the parser still decodes `\\` and `\'` inside it. A backtick value is not expanded
/// either; the parser decodes its escapes like a double-quoted value's.
///
/// A malformed reference, an unterminated reference, and a `${NAME:?text}` whose value is missing
/// or empty reject the whole source, even when the prefix selects no assignment. No expansion
/// error text, including the `text` after `:?`, is stored on the error or printed.
///
/// `${NAME:=text}` stores the operand as written. `neotales-env` does not expand a `$` inside that
/// operand before storing it, and it returns the stored text for that reference, so a later
/// assignment sees the operand literally. Use a literal default when a later assignment reads the
/// stored name.
///
/// This is not a shell. The parser and expander never execute a command, never perform word
/// splitting or globbing, and never read a positional parameter or an array. `${NAME-default}`
/// without the colon is a malformed name. A colon always introduces the default, assign, or
/// required form above: `${NAME:+text}` is not an alternate-value operator and
/// `${NAME:offset:length}` is not a substring operator.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read and
/// [`SourceError::invalid`] when it is not UTF-8, does not parse, or holds a reference the
/// expander rejects. The key policy is checked before the file is opened, so an empty separator is
/// invalid even when the file is missing or holds no assignment. The error carries the kind only;
/// it never carries a path, a line, a variable name, or a value.
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
/// and its [`fmt::Debug`] omit the path, the environment values, and any value from the document.
#[derive(Clone)]
pub struct DotenvFile {
    path: PathBuf,
    keys: Keys,
    interpolate: bool,
    environment: Option<BTreeMap<String, String>>,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the fixed environment may hold secrets; only counts \
              and flags are reportable"
)]
impl fmt::Debug for DotenvFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("DotenvFile")
            .field("keys", &self.keys)
            .field("interpolate", &self.interpolate)
            .field("environment", &self.environment.as_ref().map(BTreeMap::len))
            .finish()
    }
}

impl DotenvFile {
    /// Registers `path` to be read when the source loads.
    ///
    /// The path is opened once per load. It is never followed into another file and never
    /// written.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            keys: Keys::default(),
            interpolate: true,
            environment: None,
        }
    }

    /// Keeps only assignments whose key starts with `prefix`, and removes it from every selected
    /// key.
    ///
    /// An empty prefix selects every assignment. A key that equals the prefix has an empty key
    /// and is rejected. The prefix does not affect expansion: every assignment is expanded and
    /// kept for later lookups before selection.
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

    /// Enables or disables `${NAME}` expansion.
    ///
    /// The default is `true`. With `false`, every value is used exactly as the parser decoded it,
    /// including a `$`, and the process environment is not read. This is the explicit opt-out for
    /// a document that is not a template.
    #[must_use]
    pub fn interpolate(mut self, enabled: bool) -> Self {
        self.interpolate = enabled;
        self
    }

    /// Replaces the process-environment lookup used by interpolation with a fixed set of pairs.
    ///
    /// It is for a test or a captured environment: when it is set, the source never reads the
    /// process environment. It affects interpolation only; it never selects an assignment and
    /// never changes an output path. A repeated name keeps its last value. Names are matched
    /// exactly, including on Windows, where a process lookup is case-insensitive.
    /// `${NAME:=text}` writes only the local expansion scope, not this set. It is ignored when
    /// [`Self::interpolate`] is disabled.
    #[must_use]
    pub fn with_environment<K, V>(mut self, pairs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.environment = Some(
            pairs
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        );
        self
    }

    /// Returns the configured path.
    ///
    /// The path is not part of any error or of this type's [`fmt::Debug`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Expands one value against the current local scope and the configured environment.
    ///
    /// Every lookup goes through this method's callbacks. The default lookup is the process
    /// environment, read with [`neotales_env::get_var`]; [`Self::with_environment`] substitutes a
    /// fixed map. The setter writes only `scope`, so a `${NAME:=text}` assignment is visible to a
    /// later value in the same load and never reaches the process.
    fn expand_value(
        &self,
        value: &str,
        scope: &RefCell<BTreeMap<String, String>>,
    ) -> Result<String, SourceError> {
        let environment = self.environment.as_ref();
        let mut options = ExpandOptions::new()
            .with_get(move |name| {
                if let Some(value) = scope.borrow().get(name) {
                    return Some(value.clone());
                }
                match environment {
                    Some(pairs) => pairs.get(name).cloned(),
                    None => neotales_env::get_var(name),
                }
            })
            .with_set(move |name, value| {
                scope.borrow_mut().insert(name.to_owned(), value.to_owned());
                Ok(())
            });
        options.expand(value).map_err(|_| SourceError::invalid())
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

        // The scope is per load. A reload starts empty, so it never observes a value from the
        // previous generation.
        let scope = RefCell::new(BTreeMap::new());
        let mut root = BTreeMap::new();
        for assignment in document.assignments() {
            if assignment.key.is_empty() {
                return Err(SourceError::invalid());
            }

            let expands = self.interpolate
                && matches!(assignment.quote, QuoteStyle::Unquoted | QuoteStyle::Double);
            let value = if expands {
                self.expand_value(&assignment.value, &scope)?
            } else {
                assignment.value.clone()
            };

            // Every assignment enters the lookup scope, including one the prefix does not select:
            // a later value may refer to it.
            scope
                .borrow_mut()
                .insert(assignment.key.clone(), value.clone());

            let Some(components) = self
                .keys
                .components(&assignment.key)
                .map_err(|_| SourceError::invalid())?
            else {
                continue;
            };
            insert(&mut root, &components, Value::String(value))
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
