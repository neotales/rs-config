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
//! arguments, the process environment, and a dotenv file are opt-in sources, as is one source per
//! file format. Every provider is behind its own feature and is registered explicitly:
//!
//! | Feature | Provider | Dependency |
//! | --- | --- | --- |
//! | `args` | `Arguments` | none |
//! | `environment` | `Environment` | none |
//! | `dotenv` | `DotenvFile` | `neotales-dotenv`, `neotales-env` |
//! | `json` | `JsonFile` | `serde_json` |
//! | `json5` | `Json5File` | `json5` |
//! | `jsonc` | `JsoncFile` | `jsonc-parser` |
//! | `toml` | `TomlFile` | `toml` |
//! | `yaml` | `YamlFile` | `saphyr`, `saphyr-parser` |
//! | `ini` | `IniFile` | none |
//! | `reload` | `Builder::build_reloadable`, `Reload` | none |
//!
//! No feature is on by default. A provider value stays a string unless it is an argv boolean or a
//! format value, and no provider writes to the process environment. A dotenv value may use
//! `${NAME}` references; they expand in source order from earlier assignments and then the process
//! environment, and the process environment is never written. The dedicated provider builder
//! methods apply the builder's final delimiter as the default key separator, whatever order the
//! registration and the delimiter were given in. File formats do not take a separator: their
//! structure is already expressed by the document, so every key is used literally.
//!
//! A file source reads its path again on every load. Every file provider is required and watched
//! by default. `optional(true)` on the provider changes only the first: an actual not-found when
//! the path is opened becomes an empty source, while a directory, a permission failure, a
//! non-UTF-8 document, and a parse failure still fail the source. `reload_on_change(false)`
//! changes only the second, and only for that source: the path is not reported to the polling
//! watcher on its account, so a manual `Reload::refresh` still loads the source. A poll triggered
//! by any other watched path also loads every source, and a path another source reports stays
//! watched. The two flags are independent, and both defaults keep the pre-existing behavior.
//!
#![cfg_attr(
    feature = "reload",
    doc = "
# Reload

`Builder::build_reloadable` returns a cloneable [`Reload`] instead of a fixed [`Config`]. It
reloads every source for a new snapshot, publishes the value tree and its provenance together,
and keeps the previous snapshot when a load, parse, or validation fails. [`Reload::refresh`] is
the manual path; [`Reload::subscribe`] delivers coalesced changes to independent subscribers;
[`Reload::start_watching`] is an explicit polling helper, with no async runtime and no thread
until it is called.

See the [`reload`] module documentation for the full contract: polling limits and
retry, shutdown, sanitized errors, and the plaintext lifetime of a snapshot.
"
)]
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
#[cfg(feature = "reload")]
pub mod reload;
mod source;
mod value;

pub use self::config::{Builder, Config};
pub use self::error::{Error, SourceError, SourceErrorKind};
#[cfg(feature = "reload")]
pub use self::reload::{RefreshOutcome, Reload, Subscription, WatchError, Watcher};
pub use self::source::Source;
pub use self::value::{DateTime, Value};

#[cfg(feature = "args")]
pub use self::source::Arguments;
#[cfg(feature = "dotenv")]
pub use self::source::DotenvFile;
#[cfg(feature = "ini")]
pub use self::source::IniFile;
#[cfg(feature = "json5")]
pub use self::source::Json5File;
#[cfg(feature = "json")]
pub use self::source::JsonFile;
#[cfg(feature = "jsonc")]
pub use self::source::JsoncFile;
#[cfg(feature = "toml")]
pub use self::source::TomlFile;
#[cfg(feature = "yaml")]
pub use self::source::YamlFile;
#[cfg(feature = "environment")]
pub use self::source::{Case, Environment};
