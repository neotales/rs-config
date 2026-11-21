//! The source port, the source registration record, and the opt-in providers.

use std::fmt;

use crate::{error::SourceError, value::Value};

#[cfg(feature = "args")]
mod arguments;
#[cfg(feature = "dotenv")]
mod dotenv_file;
#[cfg(feature = "environment")]
mod environment;
#[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
mod keys;

#[cfg(feature = "args")]
pub use self::arguments::Arguments;
#[cfg(feature = "dotenv")]
pub use self::dotenv_file::DotenvFile;
#[cfg(feature = "environment")]
pub use self::environment::{Case, Environment};

/// A caller-supplied configuration source.
///
/// Register one with [`crate::Builder::source`]. [`Source::load`] returns a complete value tree
/// whose root is a [`Value::Map`]; any other root fails the build. A source is loaded once, in
/// registration order, and never again for the life of the snapshot.
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
    /// Returns [`SourceError::unavailable`] when the source could not be read, or
    /// [`SourceError::invalid`] when it rejected its own document.
    fn load(&self) -> Result<Value, SourceError>;
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
    pub(crate) source: Box<dyn Source>,
}

impl PendingSource {
    pub(crate) fn load(&self) -> Result<Value, SourceError> {
        self.source.load()
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
