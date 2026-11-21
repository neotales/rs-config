//! The opt-in tokenized argv source.

use std::{collections::BTreeMap, ffi::OsString, fmt};

use crate::{SourceError, Value};

use super::{
    Source,
    keys::{Keys, insert},
};

/// A configuration source over an already-tokenized argument vector.
///
/// The source consumes `OsString` tokens. It never joins them into a command-line string, never
/// evaluates quotes, escapes, or shell syntax, and never changes the process environment or the
/// argument vector.
///
/// # Accepted switches
///
/// - `--<key>=<value>` sets `<value>` as a string. The value may be empty, may contain `=`, and
///   may contain spaces; it is already one token. It is never parsed as a number or a boolean.
/// - `--<key>` sets the boolean `true`.
/// - `--no-<key>` sets the boolean `false`. The `no-` form applies only when there is no `=`;
///   `--no-<key>=<value>` is the string switch for the literal key `no-<key>`.
///
/// `<key>` is split into path components by [`Arguments::separator`], or by the builder's
/// delimiter when no separator was chosen. Every component must be non-empty. A switch may
/// appear more than once; the later switch wins at the same path.
///
/// # Rejected input
///
/// A token that does not start with `--` (including a short switch, a bare `-`, a positional
/// argument, and the empty token) rejects the source. So does `--`, an empty key (`--=value`),
/// `--no-` with no key, an empty path component, a non-Unicode token, and a scalar/map conflict.
/// There is no `--` terminator and no positional-argument support. There is no separate value
/// token: `--key value` sets `key` to `true` and then rejects `value`.
///
/// An error carries the kind only; it never carries a token or a key.
///
/// # Examples
///
/// ```
/// use std::ffi::OsString;
///
/// use neotales_config::{Arguments, Builder};
///
/// let config = Builder::new()
///     .arguments(
///         "arguments",
///         Arguments::new([
///             OsString::from("--http.port=8080"),
///             OsString::from("--verbose"),
///         ]),
///     )
///     .build()?;
///
/// assert_eq!(config.get::<String>("http.port")?, "8080");
/// assert!(config.get::<bool>("verbose")?);
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; a switch may carry a secret. Its error and its
/// [`fmt::Debug`] omit tokens.
#[derive(Clone)]
pub struct Arguments {
    argv: Vec<OsString>,
    keys: Keys,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the tokens may carry a secret; only their number and the key policy are reportable"
)]
impl fmt::Debug for Arguments {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Arguments")
            .field("tokens", &self.argv.len())
            .field("keys", &self.keys)
            .finish()
    }
}

impl Arguments {
    /// Uses the supplied tokens as the complete argument vector.
    ///
    /// `argv` is the arguments only. It is not a command-line string and is not re-tokenized.
    /// [`Self::from_process`] removes the program path from `std::env::args_os`; a caller that
    /// passes the full vector here must remove it first.
    #[must_use]
    pub fn new(argv: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            argv: argv.into_iter().collect(),
            keys: Keys::default(),
        }
    }

    /// Reads the process argument vector, excluding the program path.
    ///
    /// The read happens when this is called. The source never writes to the process environment
    /// or to the argument vector. A later [`crate::Builder`] load reads this snapshot, not the
    /// process again.
    #[must_use]
    pub fn from_process() -> Self {
        let mut argv = std::env::args_os();
        let _program = argv.next();
        Self::new(argv)
    }

    /// Splits a switch key on `separator` into nested maps.
    ///
    /// An explicit separator overrides the builder's delimiter. An empty separator rejects the
    /// source when it loads, even when the vector holds no switch at all.
    #[must_use]
    pub fn separator(mut self, separator: impl Into<String>) -> Self {
        self.keys = self.keys.separator(separator);
        self
    }

    /// Keeps a switch key as one component instead of splitting it.
    #[must_use]
    pub fn flat(mut self) -> Self {
        self.keys = self.keys.flat();
        self
    }

    /// Uses `delimiter` as the default key separator.
    ///
    /// [`crate::Builder::arguments`] replaces it with the builder's final delimiter when the
    /// snapshot is built, whatever order the two calls were made in. Registering the source
    /// through [`crate::Builder::source`] keeps this value; that path keeps `.` unless this
    /// method, an explicit separator, or [`Self::flat`] is used.
    #[must_use]
    pub fn delimiter(mut self, delimiter: char) -> Self {
        self.keys = self.keys.delimiter(delimiter);
        self
    }

    fn build(&self) -> Result<Value, SourceError> {
        // Check the policy before the first token: an empty separator is a caller error even for
        // an empty vector, which selects no key at all.
        self.keys.validate().map_err(|_| SourceError::invalid())?;

        let mut root = BTreeMap::new();
        for token in &self.argv {
            let Some(token) = token.to_str() else {
                return Err(SourceError::invalid());
            };
            let Some(body) = token.strip_prefix("--") else {
                return Err(SourceError::invalid());
            };
            if body.is_empty() {
                return Err(SourceError::invalid());
            }

            let (key, value) = match body.split_once('=') {
                Some((key, value)) => (key, Value::String(value.to_owned())),
                None => match body.strip_prefix("no-") {
                    Some(key) => (key, Value::Bool(false)),
                    None => (body, Value::Bool(true)),
                },
            };
            if key.is_empty() {
                return Err(SourceError::invalid());
            }

            let Some(components) = self
                .keys
                .components(key)
                .map_err(|_| SourceError::invalid())?
            else {
                return Err(SourceError::invalid());
            };
            insert(&mut root, &components, value).map_err(|_| SourceError::invalid())?;
        }
        Ok(Value::Map(root))
    }
}

impl Source for Arguments {
    fn load(&self) -> Result<Value, SourceError> {
        self.build()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::Arguments;
    use crate::{Source, SourceErrorKind, Value};

    #[test]
    fn bare_switches_are_booleans_and_valued_switches_stay_strings() {
        let arguments = Arguments::new([
            OsString::from("--port=8080"),
            OsString::from("--verbose"),
            OsString::from("--no-color"),
        ]);
        let value = arguments.load().expect("the arguments load");
        let map = value.as_map().unwrap();
        assert_eq!(map["port"], Value::from("8080"));
        assert_eq!(map["verbose"], Value::Bool(true));
        assert_eq!(map["color"], Value::Bool(false));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_unicode_token_is_invalid_without_echoing_it() {
        use std::os::unix::ffi::OsStringExt;

        let bad = OsString::from_vec(b"fixture-secret-\xff".to_vec());
        let error = Arguments::new([bad])
            .load()
            .expect_err("a non-Unicode token is rejected");
        assert_eq!(error.kind(), SourceErrorKind::Invalid);
        assert!(!format!("{error}").contains("fixture-secret"));
        assert!(!format!("{error:?}").contains("fixture-secret"));
    }
}
