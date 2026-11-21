//! The opt-in process environment source.
//!
//! The provider shape follows the plab rs-lib `neotales-config` environment source
//! (MIT, Copyright (c) 2026 Neotales), rewritten for this crate's key mapping.
//!
//! The source never writes to the process environment. [`Environment::process`] reads it once, at
//! build, and [`Environment::from_pairs`] keeps a test or a caller-supplied snapshot out of it.

use std::{collections::BTreeMap, ffi::OsString, fmt};

use crate::{SourceError, Value};

use super::{
    Source,
    keys::{Keys, insert},
};

/// The case policy applied to every key component.
///
/// The policy runs after the prefix is removed and the key is split, so `APP_HTTP.PORT` with
/// [`Case::Lower`] becomes the components `http` and `port`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Case {
    /// Keep the spelling from the environment.
    #[default]
    Preserve,
    /// Lowercase every component.
    Lower,
    /// Uppercase every component.
    Upper,
}

impl Case {
    fn apply(self, component: String) -> String {
        match self {
            Self::Preserve => component,
            Self::Lower => component.to_lowercase(),
            Self::Upper => component.to_uppercase(),
        }
    }
}

/// An environment-variable source.
///
/// The caller supplies the prefix and the key policy; nothing is discovered implicitly. A value
/// stays a string: `APP_HTTP_PORT=8080` yields the string `"8080"`, and reading it as an integer
/// is a type mismatch.
///
/// After the prefix is removed, the key is split into path components. With prefix `APP_`,
/// [`Environment::separator`] `__`, and [`Case::Lower`], `APP_HTTP__PORT=8080` yields
/// `{"http": {"port": "8080"}}`. Without an explicit separator the builder's delimiter is used;
/// [`Environment::flat`] keeps the whole remainder as one component.
///
/// Selected names are sorted by their original spelling before they are mapped. When two names
/// end at one path, the later name in that order wins, so `APP_A` and `APP_a` with
/// [`Case::Lower`] resolve the same way whatever order the environment returned. Two names that
/// would need one key to be both a scalar and a map reject the source instead.
///
/// # Errors
///
/// A non-Unicode name or value, an empty key, an empty path component, an empty separator, or a
/// scalar/map conflict makes the source invalid. The error carries the kind only; it never
/// carries a variable name or a value.
///
/// # Examples
///
/// ```
/// use neotales_config::{Builder, Case, Environment};
///
/// let config = Builder::new()
///     .environment(
///         "environment",
///         Environment::from_pairs([("APP_HTTP_PORT", "8080")])
///             .prefix("APP_")
///             .separator("_")
///             .case(Case::Lower),
///     )
///     .build()?;
///
/// assert_eq!(config.get::<String>("http.port")?, "8080");
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; it may hold a secret. Its error and its
/// [`fmt::Debug`] omit values.
#[derive(Clone)]
pub struct Environment {
    pairs: Vec<(String, String)>,
    from_process: bool,
    keys: Keys,
    case: Case,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the pairs are values; only their number and the key policy are reportable"
)]
impl fmt::Debug for Environment {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Environment")
            .field("from_process", &self.from_process)
            .field("pairs", &self.pairs.len())
            .field("keys", &self.keys)
            .field("case", &self.case)
            .finish()
    }
}

impl Environment {
    fn empty() -> Self {
        Self {
            pairs: Vec::new(),
            from_process: false,
            keys: Keys::default(),
            case: Case::Preserve,
        }
    }

    /// Reads the process environment when the source loads.
    ///
    /// The read happens at build, not when this is constructed. The source never writes to the
    /// process environment. A name or value that is not Unicode rejects the source instead of
    /// being lossily converted.
    #[must_use]
    pub fn process() -> Self {
        let mut environment = Self::empty();
        environment.from_process = true;
        environment
    }

    /// Uses a fixed set of pairs.
    ///
    /// This is how a caller supplies an environment snapshot without touching the process
    /// environment. Values stay strings.
    #[must_use]
    pub fn from_pairs<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let mut environment = Self::empty();
        environment.pairs = pairs
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        environment
    }

    /// Keeps only names that start with `prefix`, and removes it from every selected key.
    ///
    /// An empty prefix selects every name. A name that equals the prefix has an empty key and is
    /// rejected.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.keys = self.keys.prefix(prefix);
        self
    }

    /// Splits a selected key on `separator` into nested maps.
    ///
    /// An explicit separator overrides the builder's delimiter. An empty separator rejects the
    /// source when it loads.
    #[must_use]
    pub fn separator(mut self, separator: impl Into<String>) -> Self {
        self.keys = self.keys.separator(separator);
        self
    }

    /// Keeps a selected key as one component instead of splitting it.
    ///
    /// `APP_HTTP.PORT` becomes the literal key `HTTP.PORT` even when the builder's delimiter is
    /// `.`.
    #[must_use]
    pub fn flat(mut self) -> Self {
        self.keys = self.keys.flat();
        self
    }

    /// Applies a case policy to every key component.
    #[must_use]
    pub fn case(mut self, case: Case) -> Self {
        self.case = case;
        self
    }

    /// Uses `delimiter` as the default key separator.
    ///
    /// [`crate::Builder::environment`] calls this with the builder's delimiter. Registering the
    /// source through [`crate::Builder::source`] does not, so that path keeps `.` unless an
    /// explicit separator or [`Self::flat`] is used.
    #[must_use]
    pub fn delimiter(mut self, delimiter: char) -> Self {
        self.keys = self.keys.delimiter(delimiter);
        self
    }

    fn read_pairs(&self) -> Result<Vec<(String, String)>, SourceError> {
        if !self.from_process {
            return Ok(self.pairs.clone());
        }
        pairs_from_os(std::env::vars_os())
    }

    fn build(&self) -> Result<Value, SourceError> {
        // Sorting makes a collision after case folding visible in one fixed order, whatever order
        // the environment or the caller supplied.
        let mut pairs = self.read_pairs()?;
        pairs.sort_by(|left, right| left.0.cmp(&right.0));

        let mut root = BTreeMap::new();
        for (name, value) in pairs {
            if name.is_empty() {
                return Err(SourceError::invalid());
            }
            let Some(components) = self
                .keys
                .components(&name)
                .map_err(|_| SourceError::invalid())?
            else {
                continue;
            };
            let components = components
                .into_iter()
                .map(|component| self.case.apply(component))
                .collect::<Vec<_>>();
            insert(&mut root, &components, Value::String(value))
                .map_err(|_| SourceError::invalid())?;
        }
        Ok(Value::Map(root))
    }
}

/// Converts a fixed set of OS pairs, rejecting a name or a value that is not Unicode.
fn pairs_from_os(
    pairs: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Vec<(String, String)>, SourceError> {
    let mut converted = Vec::new();
    for (name, value) in pairs {
        let name = name.into_string().map_err(|_| SourceError::invalid())?;
        let value = value.into_string().map_err(|_| SourceError::invalid())?;
        converted.push((name, value));
    }
    Ok(converted)
}

impl Source for Environment {
    fn load(&self) -> Result<Value, SourceError> {
        self.build()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{Environment, pairs_from_os};
    use crate::{Source, SourceErrorKind};

    #[cfg(unix)]
    #[test]
    fn a_non_unicode_process_pair_is_invalid_without_echoing_it() {
        use std::os::unix::ffi::OsStringExt;

        let bad = OsString::from_vec(b"fixture-secret-\xff".to_vec());
        let error = pairs_from_os([(bad.clone(), OsString::from("value"))])
            .expect_err("a non-Unicode name is rejected");
        assert_eq!(error.kind(), SourceErrorKind::Invalid);
        assert!(!format!("{error}").contains("fixture-secret"));
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }

    #[test]
    fn a_fixed_pair_set_does_not_read_the_process() {
        let environment = Environment::from_pairs([("APP_HTTP_PORT", "8080")]).prefix("APP_");
        let value = environment.load().expect("the fixed pairs load");
        assert_eq!(
            value.as_map().unwrap()["HTTP_PORT"],
            crate::Value::from("8080")
        );
    }
}
