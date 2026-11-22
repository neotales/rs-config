//! The source port, the source registration record, and the opt-in providers.

use std::{fmt, path::PathBuf};

use crate::{error::SourceError, value::Value};

#[cfg(feature = "args")]
mod arguments;
#[cfg(feature = "dotenv")]
mod dotenv_file;
#[cfg(feature = "environment")]
mod environment;
#[cfg(any(
    feature = "dotenv",
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]
mod file;
#[cfg(feature = "ini")]
mod ini;
#[cfg(feature = "json")]
mod json;
#[cfg(feature = "json5")]
mod json5;
#[cfg(feature = "jsonc")]
mod jsonc;
#[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
mod keys;
#[cfg(feature = "toml")]
mod toml_file;
#[cfg(feature = "yaml")]
mod yaml;

#[cfg(feature = "args")]
pub use self::arguments::Arguments;
#[cfg(feature = "dotenv")]
pub use self::dotenv_file::DotenvFile;
#[cfg(feature = "environment")]
pub use self::environment::{Case, Environment};
#[cfg(feature = "ini")]
pub use self::ini::IniFile;
#[cfg(feature = "json")]
pub use self::json::JsonFile;
#[cfg(feature = "json5")]
pub use self::json5::Json5File;
#[cfg(feature = "jsonc")]
pub use self::jsonc::JsoncFile;
#[cfg(feature = "toml")]
pub use self::toml_file::TomlFile;
#[cfg(feature = "yaml")]
pub use self::yaml::YamlFile;

/// A built-in provider that waits for the builder's final delimiter.
///
/// A provider registered through [`crate::Builder::arguments`],
/// [`crate::Builder::environment`], or [`crate::Builder::dotenv_file`] takes the builder's
/// delimiter as its default key separator. The delimiter is applied at build rather than at
/// registration, so `.arguments(..).delimiter(':')` and `.delimiter(':').arguments(..)` split keys
/// the same way.
#[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
pub(crate) enum Deferred {
    #[cfg(feature = "args")]
    Arguments(Arguments),
    #[cfg(feature = "dotenv")]
    DotenvFile(DotenvFile),
    #[cfg(feature = "environment")]
    Environment(Environment),
}

#[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
impl Deferred {
    /// Applies `delimiter` as the provider's default key separator.
    ///
    /// An explicit separator or a flat key policy is kept: the delimiter only supplies the default
    /// the provider would otherwise take from `.`.
    fn into_source(self, delimiter: char) -> Box<dyn Source> {
        match self {
            #[cfg(feature = "args")]
            Self::Arguments(source) => Box::new(source.delimiter(delimiter)),
            #[cfg(feature = "dotenv")]
            Self::DotenvFile(source) => Box::new(source.delimiter(delimiter)),
            #[cfg(feature = "environment")]
            Self::Environment(source) => Box::new(source.delimiter(delimiter)),
        }
    }
}

/// A caller-supplied configuration source.
///
/// Register one with [`crate::Builder::source`]. [`Source::load`] returns a complete value tree
/// whose root is a [`Value::Map`]; any other root fails the build. A source is loaded once, in
/// registration order, for one snapshot. A `Reload` holder built from the same builder loads
/// every source again on each refresh; the core [`crate::Config`] never does.
///
/// A source reports failure with a [`SourceError`], which carries a kind and no message. It must
/// never put a value, a connection string, or parser text on that error.
///
/// A blanket implementation covers a closure of type `Fn() -> Result<Value, SourceError>`:
///
/// ```
/// use neotales_config::{Builder, SourceError, Value};
///
/// let config = Builder::new()
///     .source("computed", || {
///         Ok(Value::map([("port", Value::from(8080_i64))]))
///     })
///     .build()?;
/// assert_eq!(config.get::<u16>("port")?, 8080);
/// # Ok::<(), neotales_config::Error>(())
/// ```
pub trait Source: Send + Sync {
    /// Loads a complete value tree whose root is a map.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::unavailable`] when the source could not be read,
    /// [`SourceError::invalid`] when it rejected its own document, or
    /// [`SourceError::unrepresentable`] when a value cannot be held without loss.
    fn load(&self) -> Result<Value, SourceError>;

    /// Returns the files whose contents decide what [`Source::load`] returns.
    ///
    /// A file-based provider returns its configured path here unless change watching for that
    /// source is turned off (for example with `JsonFile::reload_on_change(false)`); a provider in
    /// another crate can offer the same switch. The `reload` feature polls those paths when the
    /// caller explicitly starts a watcher. The default is an empty list, so a source over a
    /// process, a network service, or a vault names only the stable local files it reads, if any.
    /// A path that does not exist yet is still returned: deletion and later recreation are
    /// changes the caller wants to observe. Watching is independent of whether a file is required
    /// or optional.
    ///
    /// The returned lists are combined into one watched list, so this report is a contribution,
    /// not a private subscription. A change in any watched path reloads **every** source, and two
    /// sources may name the same path: it is polled once, and a source that stops reporting it
    /// does not remove it while another source still reports it.
    ///
    /// The paths are used for change detection only. They are never stored on an error, printed
    /// by a [`fmt::Debug`] implementation, or published in a snapshot, because a path can name a
    /// host or a user. A source whose content comes from somewhere a local file cannot describe
    /// returns an empty list rather than a guess. Polling observes only these paths: if the
    /// content also depends on another input, such as a credential store, a poll cannot see that
    /// input change and the caller must use `Reload::refresh` instead.
    #[must_use]
    fn watch_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

impl<F> Source for F
where
    F: Fn() -> Result<Value, SourceError> + Send + Sync,
{
    fn load(&self) -> Result<Value, SourceError> {
        self()
    }
}

/// One source waiting for [`crate::Builder::build`].
pub(crate) struct PendingSource {
    pub(crate) name: String,
    source: Pending,
}

/// A registered source, before the builder's final delimiter is known.
enum Pending {
    Ready(Box<dyn Source>),
    #[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
    Deferred(Deferred),
}

impl PendingSource {
    /// Registers a source that is used exactly as supplied.
    pub(crate) fn ready(name: String, source: impl Source + 'static) -> Self {
        Self {
            name,
            source: Pending::Ready(Box::new(source)),
        }
    }

    /// Registers a built-in provider that takes the builder's final delimiter at build.
    #[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
    pub(crate) fn deferred(name: String, provider: Deferred) -> Self {
        Self {
            name,
            source: Pending::Deferred(provider),
        }
    }

    /// Applies the builder's final delimiter to a provider that waits for one.
    ///
    /// This runs at build, not at registration, so the key separator is the delimiter the builder
    /// ends up with whatever order the registration and [`crate::Builder::delimiter`] were called
    /// in.
    #[cfg_attr(
        not(any(feature = "args", feature = "environment", feature = "dotenv")),
        allow(
            unused_variables,
            reason = "no provider feature is compiled in, so no source can defer a delimiter"
        )
    )]
    pub(crate) fn resolve(self, delimiter: char) -> ResolvedSource {
        let Self { name, source } = self;
        match source {
            Pending::Ready(source) => ResolvedSource { name, source },
            #[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
            Pending::Deferred(provider) => ResolvedSource {
                name,
                source: provider.into_source(delimiter),
            },
        }
    }
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "a source may hold a document or a closure; only the registered name is reportable"
)]
impl fmt::Debug for PendingSource {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("PendingSource")
            .field("name", &self.name)
            .finish()
    }
}

/// A source with the builder's final delimiter already applied, ready to load.
pub(crate) struct ResolvedSource {
    pub(crate) name: String,
    source: Box<dyn Source>,
}

impl ResolvedSource {
    /// Loads the source once.
    ///
    /// # Errors
    ///
    /// Returns whatever the source reports; the builder attaches the registered name.
    pub(crate) fn load(&self) -> Result<Value, SourceError> {
        self.source.load()
    }

    /// Returns the files this source reads, for the reload feature's polling watcher.
    #[cfg(feature = "reload")]
    pub(crate) fn watch_paths(&self) -> Vec<PathBuf> {
        self.source.watch_paths()
    }
}
