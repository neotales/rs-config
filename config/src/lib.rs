//! Process-local configuration with ordered sources and immutable snapshots.
//!
//! A [`Builder`] registers sources in precedence order, merges them once, and returns a
//! [`Config`]. A [`Config`] is a cursor over an `Arc`-backed snapshot: cloning it increments a
//! reference count and never copies the value tree. A snapshot never changes after it is built.
//!
//! # Contracts
//!
//! - **Later sources win.** Sources register in ascending precedence. A later source wins per key.
//! - **Maps merge, everything else replaces.** Two maps merge recursively. A scalar, array, null,
//!   or map/scalar mismatch replaces the earlier value at that key. Arrays never concatenate.
//! - **A source root is a map.** A source that returns any other [`Value`] fails the build.
//! - **Source names are unique.** An empty name or a repeated name fails the build.
//! - **Typed reads are strict.** A missing path is not a type mismatch, and a string is never
//!   parsed as a number. An integer is never read as a float and a float is never read as an
//!   integer, even when the number has no fraction. [`Config::get_or`] returns its default only
//!   for absence.
//! - **Paths are validated.** A path is one or more non-empty components separated by the
//!   builder's delimiter, `.` by default. An empty path, a leading or trailing delimiter, or two
//!   adjacent delimiters is an [`Error::is_invalid_path`] error. The delimiter is one of `.`,
//!   `:`, `/`, `,`, or `|`; anything else fails the build with [`Error::is_invalid_delimiter`]
//!   before a source is loaded.
//! - **Errors never echo a value.** An error names a path, a source, and a fixed shape label. It
//!   never carries a value, a connection string, or text from a caller-supplied deserializer.
//! - **Provenance is per node.** [`Config::origin`] reports the source that introduced or last
//!   replaced a node. Replacing a subtree drops the provenance below it. A type mismatch names
//!   the source of the failing leaf, not the map that contains it.
//!
//! # Providers
//!
//! The core is memory-only: a caller supplies a [`Value`] map or implements [`Source`]. Process
//! arguments, the process environment, and a dotenv file are opt-in sources, each behind its own
//! feature and each registered explicitly:
//!
//! | Feature | Provider | Dependency |
//! | --- | --- | --- |
//! | `args` | `Arguments` | none |
//! | `environment` | `Environment` | none |
//! | `dotenv` | `DotenvFile` | `neotales-dotenv` |
//!
//! No feature is on by default. A provider value stays a string unless it is an argv boolean, and
//! no provider writes to the process environment. The dedicated builder methods apply the
//! builder's final delimiter as the provider's default key separator, whatever order the
//! registration and the delimiter were given in.
//!
//! There is no file-format, reload, vault, or remote provider in this release.
//!
//! # Example
//!
//! ```
//! use neotales_config::{Builder, Value};
//!
//! let config = Builder::new()
//!     .memory(
//!         "defaults",
//!         Value::map([(
//!             "http",
//!             Value::map([("port", Value::from(8080_i64))]),
//!         )]),
//!     )
//!     .memory(
//!         "overlay",
//!         Value::map([(
//!             "http",
//!             Value::map([("port", Value::from(9090_i64))]),
//!         )]),
//!     )
//!     .build()?;
//!
//! assert_eq!(config.get::<u16>("http.port")?, 9090);
//! assert_eq!(config.origin("http.port")?, "overlay");
//! # Ok::<(), neotales_config::Error>(())
//! ```

mod config;
mod de;
mod error;
mod source;
mod value;

pub use self::config::{Builder, Config};
pub use self::error::{Error, SourceError, SourceErrorKind};
pub use self::source::Source;
pub use self::value::Value;

#[cfg(feature = "args")]
pub use self::source::Arguments;
#[cfg(feature = "dotenv")]
pub use self::source::DotenvFile;
#[cfg(feature = "environment")]
pub use self::source::{Case, Environment};
