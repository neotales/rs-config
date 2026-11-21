//! Sanitized errors. Nothing in this module stores a value or a caller-supplied message.

use std::{error::Error as StdError, fmt};

/// Why a caller-supplied [`crate::Source`] refused to load.
///
/// There is no message field. A source may return a value to the caller, but it cannot put that
/// value, a connection string, or parser text on this error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceErrorKind {
    /// The source could not be read.
    Unavailable,
    /// The source rejected its document.
    Invalid,
}

/// Failure returned by a caller-supplied [`crate::Source`].
///
/// [`fmt::Display`] and [`fmt::Debug`] name the kind only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceError {
    kind: SourceErrorKind,
}

impl SourceError {
    /// The source could not be read.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            kind: SourceErrorKind::Unavailable,
        }
    }

    /// The source rejected its document.
    #[must_use]
    pub const fn invalid() -> Self {
        Self {
            kind: SourceErrorKind::Invalid,
        }
    }

    /// Returns the failure kind.
    #[must_use]
    pub const fn kind(self) -> SourceErrorKind {
        self.kind
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            SourceErrorKind::Unavailable => output.write_str("source unavailable"),
            SourceErrorKind::Invalid => output.write_str("source rejected the document"),
        }
    }
}

impl StdError for SourceError {}

/// The path delimiters a [`crate::Builder`] accepts.
///
/// The set is deliberately small and fixed. Every entry is a single ASCII byte that is not
/// alphanumeric, not whitespace, and not a control character, so a path always splits on a
/// character that reads the same in a message, a log line, and a manifest. Extending the set is a
/// deliberate change to this list, not a per-caller option.
pub(crate) const SUPPORTED_DELIMITERS: [char; 5] = ['.', ':', '/', ',', '|'];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ErrorKind {
    Missing {
        path: String,
    },
    TypeMismatch {
        path: String,
        source: Option<String>,
        expected: &'static str,
    },
    InvalidPath {
        path: String,
    },
    InvalidDelimiter,
    DuplicateSource {
        name: String,
    },
    EmptySourceName,
    Source {
        name: String,
        kind: SourceErrorKind,
    },
    Unrepresentable {
        name: String,
    },
    RootNotMap {
        name: String,
    },
}

/// A configuration error that is safe to log.
///
/// [`fmt::Display`], [`fmt::Debug`], and [`StdError::source`] omit values, secrets, connection
/// strings, and caller-supplied deserializer text. A path, a source name, and a fixed expected
/// shape label are included; a caller must not put a secret in a path or a source name. A type
/// mismatch names the source that wrote the failing leaf when the snapshot records one.
/// [`StdError::source`] is always `None`, so no foreign error can reintroduce its text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub(crate) kind: ErrorKind,
}

impl Error {
    pub(crate) fn missing(path: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Missing { path: path.into() },
        }
    }

    pub(crate) fn type_mismatch(
        path: String,
        source: Option<&str>,
        expected: &'static str,
    ) -> Self {
        Self {
            kind: ErrorKind::TypeMismatch {
                path,
                source: source.map(str::to_owned),
                expected,
            },
        }
    }

    pub(crate) fn invalid_path(path: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::InvalidPath { path: path.into() },
        }
    }

    /// The builder was given a delimiter outside [`SUPPORTED_DELIMITERS`].
    ///
    /// The rejected character is not stored, so the error cannot echo it.
    pub(crate) const fn invalid_delimiter() -> Self {
        Self {
            kind: ErrorKind::InvalidDelimiter,
        }
    }

    pub(crate) fn duplicate_source(name: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::DuplicateSource { name: name.into() },
        }
    }

    pub(crate) const fn empty_source_name() -> Self {
        Self {
            kind: ErrorKind::EmptySourceName,
        }
    }

    pub(crate) fn from_source(name: impl Into<String>, error: SourceError) -> Self {
        Self {
            kind: ErrorKind::Source {
                name: name.into(),
                kind: error.kind,
            },
        }
    }

    pub(crate) fn unrepresentable(name: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Unrepresentable { name: name.into() },
        }
    }

    pub(crate) fn root_not_map(name: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::RootNotMap { name: name.into() },
        }
    }

    /// Whether the path was absent.
    ///
    /// Absence is the only condition for which [`crate::Config::get_or`] returns the
    /// caller-supplied default. A present null, false, zero, or empty value is not missing.
    #[must_use]
    pub const fn is_missing(&self) -> bool {
        matches!(self.kind, ErrorKind::Missing { .. })
    }

    /// Whether a present value had the wrong type.
    #[must_use]
    pub const fn is_type_mismatch(&self) -> bool {
        matches!(self.kind, ErrorKind::TypeMismatch { .. })
    }

    /// Whether a path was empty or had an empty component.
    #[must_use]
    pub const fn is_invalid_path(&self) -> bool {
        matches!(self.kind, ErrorKind::InvalidPath { .. })
    }

    /// Whether the builder was given a path delimiter outside the supported set.
    ///
    /// The supported delimiters are `.`, `:`, `/`, `,`, and `|`. The check runs at
    /// [`crate::Builder::build`], before any source is loaded.
    #[must_use]
    pub const fn is_invalid_delimiter(&self) -> bool {
        matches!(self.kind, ErrorKind::InvalidDelimiter)
    }

    /// Whether two sources were registered under one name.
    #[must_use]
    pub const fn is_duplicate_source(&self) -> bool {
        matches!(self.kind, ErrorKind::DuplicateSource { .. })
    }

    /// Whether a source was registered under an empty name.
    #[must_use]
    pub const fn is_empty_source_name(&self) -> bool {
        matches!(self.kind, ErrorKind::EmptySourceName)
    }

    /// Whether a source contained a number the value model cannot represent.
    #[must_use]
    pub const fn is_unrepresentable(&self) -> bool {
        matches!(self.kind, ErrorKind::Unrepresentable { .. })
    }

    /// Whether a source returned a root that is not a map.
    #[must_use]
    pub const fn is_root_not_map(&self) -> bool {
        matches!(self.kind, ErrorKind::RootNotMap { .. })
    }

    /// Whether a source failed to load or returned an unusable tree.
    ///
    /// This covers [`Self::is_unrepresentable`], [`Self::is_root_not_map`], and a failure reported
    /// by the source itself.
    #[must_use]
    pub const fn is_source_failure(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Source { .. }
                | ErrorKind::Unrepresentable { .. }
                | ErrorKind::RootNotMap { .. }
        )
    }

    /// The source name, when this error has one.
    #[must_use]
    pub fn source_name(&self) -> Option<&str> {
        match &self.kind {
            ErrorKind::TypeMismatch { source, .. } => source.as_deref(),
            ErrorKind::Source { name, .. }
            | ErrorKind::Unrepresentable { name }
            | ErrorKind::RootNotMap { name }
            | ErrorKind::DuplicateSource { name } => Some(name.as_str()),
            ErrorKind::Missing { .. }
            | ErrorKind::InvalidPath { .. }
            | ErrorKind::InvalidDelimiter
            | ErrorKind::EmptySourceName => None,
        }
    }

    /// The path this error names, when it has one.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match &self.kind {
            ErrorKind::Missing { path }
            | ErrorKind::TypeMismatch { path, .. }
            | ErrorKind::InvalidPath { path } => Some(path.as_str()),
            ErrorKind::InvalidDelimiter
            | ErrorKind::DuplicateSource { .. }
            | ErrorKind::EmptySourceName
            | ErrorKind::Source { .. }
            | ErrorKind::Unrepresentable { .. }
            | ErrorKind::RootNotMap { .. } => None,
        }
    }

    /// The fixed shape label a typed read expected.
    #[must_use]
    pub const fn expected(&self) -> Option<&'static str> {
        match &self.kind {
            ErrorKind::TypeMismatch { expected, .. } => Some(*expected),
            _ => None,
        }
    }

    /// The kind reported by a source that failed to load.
    #[must_use]
    pub const fn source_kind(&self) -> Option<SourceErrorKind> {
        match &self.kind {
            ErrorKind::Source { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Missing { path } => write!(output, "missing path `{path}`"),
            ErrorKind::TypeMismatch {
                path,
                source,
                expected,
            } => {
                write!(output, "type mismatch for `{path}`")?;
                if let Some(source) = source {
                    write!(output, " from {source}")?;
                }
                write!(output, ", expected {expected}")
            }
            ErrorKind::InvalidPath { path } => write!(
                output,
                "invalid path `{path}`: a path is one or more non-empty components"
            ),
            ErrorKind::InvalidDelimiter => {
                output.write_str("unsupported path delimiter: expected one of ")?;
                for (index, delimiter) in SUPPORTED_DELIMITERS.iter().enumerate() {
                    if index > 0 {
                        output.write_str(", ")?;
                    }
                    write!(output, "`{delimiter}`")?;
                }
                Ok(())
            }
            ErrorKind::DuplicateSource { name } => {
                write!(output, "duplicate source name `{name}`")
            }
            ErrorKind::EmptySourceName => output.write_str("source name must not be empty"),
            ErrorKind::Source { name, kind } => match kind {
                SourceErrorKind::Unavailable => write!(output, "source `{name}` unavailable"),
                SourceErrorKind::Invalid => {
                    write!(output, "source `{name}` rejected the document")
                }
            },
            ErrorKind::Unrepresentable { name } => {
                write!(output, "source `{name}` has a non-finite number")
            }
            ErrorKind::RootNotMap { name } => {
                write!(output, "source `{name}` root must be a map")
            }
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        None
    }
}
