//! The builder, the immutable snapshot, and the typed read API.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fmt,
    sync::Arc,
};

use serde::de::DeserializeOwned;

use crate::{
    de::{Cursor, DeError, Node},
    error::{Error, SUPPORTED_DELIMITERS},
    source::{PendingSource, ResolvedSource, Source},
    value::Value,
};

#[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
use crate::source::Deferred;

/// The source name reported for a node that no source wrote. Only the root has no source, and the
/// root is not addressable by a path, so this is not observable through [`Config::origin`].
const NO_ORIGIN: &str = "configuration";

/// The provenance of every addressable node, keyed by its path components.
pub(crate) type Origins = BTreeMap<Vec<String>, Arc<str>>;

/// One immutable merged value tree with per-node source attribution.
pub(crate) struct Snapshot {
    pub(crate) value: Value,
    pub(crate) origin: Origins,
}

/// Builds one process-local configuration snapshot.
///
/// Each registration method adds one source. Registration order is precedence: the later source
/// wins per key, nested maps merge, and any other value replaces. There are no profiles and no
/// implicit discovery.
///
/// # Secrets
///
/// [`fmt::Debug`] reports the number of sources and the delimiter, not their contents.
pub struct Builder {
    sources: Vec<PendingSource>,
    delimiter: char,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "a source list is reported by size, never by contents"
)]
impl fmt::Debug for Builder {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Builder")
            .field("sources", &self.sources.len())
            .field("delimiter", &self.delimiter)
            .finish()
    }
}

impl Builder {
    /// Starts an empty builder with `.` as its path delimiter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            delimiter: '.',
        }
    }

    /// Selects the delimiter for string paths.
    ///
    /// The delimiter separates components for [`Config::child`], [`Config::has`],
    /// [`Config::get`], [`Config::get_or`], and [`Config::origin`]. With `:`, `http:port` and
    /// `http.port` are different paths: the first is two components and the second is one
    /// component whose name happens to contain a dot.
    ///
    /// # Supported delimiters
    ///
    /// The delimiter must be one of `.` (the default), `:`, `/`, `,`, or `|`. The set is fixed and
    /// documented rather than caller-defined: each entry is one ASCII byte that is not
    /// alphanumeric, not whitespace, and not a control character, so the split is predictable and
    /// a path reads the same in a message and in a log line. Any other character fails
    /// [`Builder::build`] with [`Error::is_invalid_delimiter`] before a source is loaded. The
    /// builder keeps the value until then, so this method is never the failing call.
    ///
    /// A source key that contains the delimiter, or is empty, is not addressable through a
    /// string path, because the path splits on the delimiter and rejects an empty component. It
    /// stays in the tree and is readable through [`Config::load`] or [`Config::value`].
    ///
    /// With the default `.`, the path `http.port` always means the nested map `http` then `port`.
    /// A top-level key whose name is literally `http.port` is not selected by it, and a missing
    /// nested map is [`Error::is_missing`], not the literal key.
    ///
    /// A provider registered through `arguments`, `environment`, or `dotenv_file` takes the final
    /// value as its default key separator, whatever order the registration and this method are
    /// called in. A source registered through [`Builder::source`] keeps its own policy.
    ///
    /// ```
    /// use neotales_config::{Builder, Value};
    ///
    /// let config = Builder::new()
    ///     .delimiter('|')
    ///     .memory(
    ///         "defaults",
    ///         Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
    ///     )
    ///     .build()?;
    /// assert_eq!(config.get::<u16>("http|port")?, 8080);
    ///
    /// let error = Builder::new()
    ///     .delimiter('a')
    ///     .build()
    ///     .expect_err("an alphanumeric delimiter is not supported");
    /// assert!(error.is_invalid_delimiter());
    /// # Ok::<(), neotales_config::Error>(())
    /// ```
    #[must_use]
    pub const fn delimiter(mut self, delimiter: char) -> Self {
        self.delimiter = delimiter;
        self
    }

    /// Registers an in-memory source.
    ///
    /// `value` must be a [`Value::Map`]; any other root fails [`Builder::build`] with
    /// [`Error::is_root_not_map`].
    #[must_use]
    pub fn memory(self, name: impl Into<String>, value: Value) -> Self {
        self.source(name, move || Ok(value.clone()))
    }

    /// Registers a process argument source. Requires the `args` feature.
    ///
    /// The provider receives the builder's final delimiter as its default key separator, whatever
    /// order this method and [`Builder::delimiter`] are called in: `--http.port=8080` maps to
    /// `http` then `port` under the default `.`, and to one literal key under a custom delimiter
    /// unless [`crate::Arguments::separator`] or [`crate::Arguments::flat`] is chosen. See
    /// [`crate::Arguments`] for the accepted switches and errors.
    #[cfg(feature = "args")]
    #[must_use]
    pub fn arguments(self, name: impl Into<String>, arguments: crate::Arguments) -> Self {
        self.provider(name, Deferred::Arguments(arguments))
    }

    /// Registers a process environment source. Requires the `environment` feature.
    ///
    /// The provider receives the builder's final delimiter as its default key separator, whatever
    /// order this method and [`Builder::delimiter`] are called in. See [`crate::Environment`] for
    /// prefix, case, collision, duplicate, and error behavior.
    #[cfg(feature = "environment")]
    #[must_use]
    pub fn environment(self, name: impl Into<String>, environment: crate::Environment) -> Self {
        self.provider(name, Deferred::Environment(environment))
    }

    /// Registers a dotenv file source. Requires the `dotenv` feature.
    ///
    /// The provider receives the builder's final delimiter as its default key separator, whatever
    /// order this method and [`Builder::delimiter`] are called in. The file is read at build and
    /// the process environment is never changed. See [`crate::DotenvFile`].
    #[cfg(feature = "dotenv")]
    #[must_use]
    pub fn dotenv_file(self, name: impl Into<String>, file: crate::DotenvFile) -> Self {
        self.provider(name, Deferred::DotenvFile(file))
    }

    /// Registers a JSON file source. Requires the `json` feature.
    ///
    /// The file is read at build and again whenever the source is loaded. It is read as RFC 8259
    /// JSON; comments and trailing commas are not accepted. See [`crate::JsonFile`].
    #[cfg(feature = "json")]
    #[must_use]
    pub fn json_file(self, name: impl Into<String>, file: crate::JsonFile) -> Self {
        self.source(name, file)
    }

    /// Registers a JSON5 file source. Requires the `json5` feature.
    ///
    /// The file is read at build and again whenever the source is loaded. JSON5 is a distinct
    /// dialect from JSONC: it accepts unquoted names, single-quoted strings, hexadecimal numbers,
    /// trailing commas, `Infinity`, and `NaN`. See [`crate::Json5File`].
    #[cfg(feature = "json5")]
    #[must_use]
    pub fn json5_file(self, name: impl Into<String>, file: crate::Json5File) -> Self {
        self.source(name, file)
    }

    /// Registers a JSONC file source. Requires the `jsonc` feature.
    ///
    /// The file is read at build and again whenever the source is loaded. JSONC is standard JSON
    /// plus `//` and `/* */` comments; JSON5-only syntax and trailing commas are rejected. See
    /// [`crate::JsoncFile`].
    #[cfg(feature = "jsonc")]
    #[must_use]
    pub fn jsonc_file(self, name: impl Into<String>, file: crate::JsoncFile) -> Self {
        self.source(name, file)
    }

    /// Registers a TOML file source. Requires the `toml` feature.
    ///
    /// The file is read at build and again whenever the source is loaded. A TOML date or time is
    /// kept as [`crate::DateTime`]. See [`crate::TomlFile`].
    #[cfg(feature = "toml")]
    #[must_use]
    pub fn toml_file(self, name: impl Into<String>, file: crate::TomlFile) -> Self {
        self.source(name, file)
    }

    /// Registers a YAML file source. Requires the `yaml` feature.
    ///
    /// The file is read at build and again whenever the source is loaded. Anchors, aliases, tags,
    /// duplicate keys, non-string keys, and multiple documents are rejected. See
    /// [`crate::YamlFile`].
    #[cfg(feature = "yaml")]
    #[must_use]
    pub fn yaml_file(self, name: impl Into<String>, file: crate::YamlFile) -> Self {
        self.source(name, file)
    }

    /// Registers an INI file source. Requires the `ini` feature.
    ///
    /// The file is read at build and again whenever the source is loaded. Values stay strings and
    /// no interpolation is performed. See [`crate::IniFile`].
    #[cfg(feature = "ini")]
    #[must_use]
    pub fn ini_file(self, name: impl Into<String>, file: crate::IniFile) -> Self {
        self.source(name, file)
    }

    /// Registers a built-in provider whose default key separator is the builder's final delimiter.
    ///
    /// The delimiter is applied at build, not here, so registration order cannot decide the
    /// separator.
    #[cfg(any(feature = "args", feature = "environment", feature = "dotenv"))]
    fn provider(mut self, name: impl Into<String>, provider: Deferred) -> Self {
        self.sources
            .push(PendingSource::deferred(name.into(), provider));
        self
    }

    /// Registers a caller-supplied source in precedence order.
    ///
    /// The source returns a value tree whose root is a [`Value::Map`], or a
    /// [`crate::SourceError`]. Its error text is not forwarded, so a provider in another crate
    /// cannot leak a value through this one. A closure of type
    /// `Fn() -> Result<Value, SourceError>` implements [`Source`].
    ///
    /// A provider registered here is used as-is. Unlike the dedicated provider methods, this
    /// method cannot pass the builder's delimiter to it, so the provider keeps `.` as its default
    /// key separator unless it was configured explicitly.
    ///
    /// A source is loaded once, at build. It is not called again for the life of the snapshot.
    #[must_use]
    pub fn source(mut self, name: impl Into<String>, source: impl Source + 'static) -> Self {
        self.sources.push(PendingSource::ready(name.into(), source));
        self
    }

    /// Builds the fixed immutable snapshot.
    ///
    /// The delimiter is checked first, then source names, then the delimiter is applied to every
    /// registered provider before any of them is loaded. Sources are loaded in registration order
    /// before any value is merged. A source may therefore fail without a partial result being
    /// observable, and a bad delimiter fails before a source is loaded.
    ///
    /// # Errors
    ///
    /// - [`Error::is_invalid_delimiter`] when the builder's delimiter is not in the supported
    ///   set. This is checked before any source is loaded.
    /// - [`Error::is_empty_source_name`] when a source has an empty name.
    /// - [`Error::is_duplicate_source`] when two sources share a name.
    /// - [`Error::is_source_failure`] when a source cannot load, contains a non-finite number, or
    ///   returns a root that is not a map.
    pub fn build(self) -> Result<Config, Error> {
        check_delimiter(self.delimiter)?;
        check_names(&self.sources)?;
        let Self { sources, delimiter } = self;

        // Resolve the delimiter before loading: a built-in provider's default key separator is the
        // builder's final delimiter, so registration order relative to `delimiter` cannot change
        // how its keys are split.
        let sources: Vec<ResolvedSource> = sources
            .into_iter()
            .map(|pending| pending.resolve(delimiter))
            .collect();

        // Load and check every source before merging any of them. A source that fails therefore
        // cannot leave a partially merged tree to reason about.
        let mut roots = Vec::with_capacity(sources.len());
        for source in &sources {
            let value = source
                .load()
                .map_err(|error| Error::from_source(source.name.as_str(), error))?;
            if !value.is_representable() {
                return Err(Error::unrepresentable(source.name.as_str()));
            }
            let Value::Map(root) = value else {
                return Err(Error::root_not_map(source.name.as_str()));
            };
            roots.push(root);
        }

        let mut snapshot = Snapshot {
            value: Value::Map(BTreeMap::new()),
            origin: BTreeMap::new(),
        };
        for (source, root) in sources.iter().zip(roots) {
            let name: Arc<str> = Arc::from(source.name.as_str());
            merge_root(&mut snapshot, root, &name);
        }
        Ok(Config::root(Arc::new(snapshot), delimiter))
    }
}

/// Rejects a delimiter outside the documented set before any source is loaded.
fn check_delimiter(delimiter: char) -> Result<(), Error> {
    if SUPPORTED_DELIMITERS.contains(&delimiter) {
        Ok(())
    } else {
        Err(Error::invalid_delimiter())
    }
}

/// Rejects an empty or repeated source name before any source is loaded.
fn check_names(sources: &[PendingSource]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for source in sources {
        if source.name.is_empty() {
            return Err(Error::empty_source_name());
        }
        if !seen.insert(source.name.as_str()) {
            return Err(Error::duplicate_source(&source.name));
        }
    }
    Ok(())
}

/// Merges one source root map into the snapshot, recording provenance.
fn merge_root(snapshot: &mut Snapshot, later: BTreeMap<String, Value>, source: &Arc<str>) {
    let Value::Map(base) = &mut snapshot.value else {
        return;
    };
    merge_map(base, &mut snapshot.origin, later, source, &mut Vec::new());
}

/// Merges map entries recursively. `path` is the path of `base` itself.
fn merge_map(
    base: &mut BTreeMap<String, Value>,
    origin: &mut Origins,
    later: BTreeMap<String, Value>,
    source: &Arc<str>,
    path: &mut Vec<String>,
) {
    for (key, value) in later {
        path.push(key.clone());
        match (base.entry(key), value) {
            (Entry::Occupied(mut entry), Value::Map(later_map)) => {
                if let Value::Map(base_map) = entry.get_mut() {
                    merge_map(base_map, origin, later_map, source, path);
                } else {
                    forget_below(origin, path);
                    let value = Value::Map(later_map);
                    record_tree(&value, origin, source, path);
                    entry.insert(value);
                }
            }
            (Entry::Occupied(mut entry), value) => {
                forget_below(origin, path);
                record_tree(&value, origin, source, path);
                entry.insert(value);
            }
            (Entry::Vacant(entry), value) => {
                record_tree(&value, origin, source, path);
                entry.insert(value);
            }
        }
        path.pop();
    }
}

/// Drops attribution for every node below `path`, which a replacement made stale.
fn forget_below(origin: &mut Origins, path: &[String]) {
    let stale: Vec<Vec<String>> = origin
        .keys()
        .filter(|key| key.len() > path.len() && key.starts_with(path))
        .cloned()
        .collect();
    for key in stale {
        origin.remove(&key);
    }
}

/// Records `source` for `value` and every node below it.
fn record_tree(value: &Value, origin: &mut Origins, source: &Arc<str>, path: &mut Vec<String>) {
    if !path.is_empty() {
        origin.insert(path.clone(), Arc::clone(source));
    }
    if let Value::Map(map) = value {
        for (key, child) in map {
            path.push(key.clone());
            record_tree(child, origin, source, path);
            path.pop();
        }
    }
}

/// Joins path components for display.
fn join(cursor: &[String], delimiter: char) -> String {
    let mut output = String::new();
    for (index, component) in cursor.iter().enumerate() {
        if index > 0 {
            output.push(delimiter);
        }
        output.push_str(component);
    }
    output
}

/// An immutable configuration snapshot, focused at a path.
///
/// Cloning is two `Arc` reference-count increments: one for the snapshot and one for the cursor.
/// The value tree is never copied, and the snapshot never changes.
#[derive(Clone)]
pub struct Config {
    snapshot: Arc<Snapshot>,
    cursor: Arc<[String]>,
    delimiter: char,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "a snapshot is reported by path and type, never by value"
)]
impl fmt::Debug for Config {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Config")
            .field("path", &join(&self.cursor, self.delimiter))
            .field("type", &self.focused().map(Value::type_name))
            .finish()
    }
}

impl Config {
    pub(crate) fn root(snapshot: Arc<Snapshot>, delimiter: char) -> Self {
        Self {
            snapshot,
            cursor: Arc::from(Vec::new()),
            delimiter,
        }
    }

    /// Focuses a child of this snapshot. A missing child is an error, not an empty document.
    ///
    /// The result is a cursor over the same snapshot; no part of the tree is cloned.
    ///
    /// # Errors
    ///
    /// Returns [`Error::is_invalid_path`] when `path` is empty or has an empty component, and
    /// [`Error::is_missing`] when the path is absent.
    pub fn child(&self, path: &str) -> Result<Self, Error> {
        self.validate_path(path)?;
        let mut cursor: Vec<String> = self.cursor.to_vec();
        cursor.extend(path.split(self.delimiter).map(str::to_owned));
        if self
            .snapshot
            .value
            .descend(cursor.iter().map(String::as_str))
            .is_none()
        {
            return Err(Error::missing(join(&cursor, self.delimiter)));
        }
        Ok(Self {
            snapshot: Arc::clone(&self.snapshot),
            cursor: cursor.into(),
            delimiter: self.delimiter,
        })
    }

    /// Tests whether a path is present, without deserializing it.
    ///
    /// Null, false, zero, and empty are present.
    ///
    /// # Errors
    ///
    /// Returns [`Error::is_invalid_path`] when `path` is empty or has an empty component.
    pub fn has(&self, path: &str) -> Result<bool, Error> {
        self.validate_path(path)?;
        Ok(self.resolve(path).is_some())
    }

    /// Deserializes a present value.
    ///
    /// Numeric kinds are strict. A [`Value::Integer`] reads as an integer type and a
    /// [`Value::Float`] reads as a float type: an integer is never read as a float and a float is
    /// never read as an integer, even when the float has no fraction. Within the integer kinds a
    /// value narrows only when the target type holds it, and within the float kinds a value reads
    /// only when the target type holds it exactly. A string is never parsed as a number.
    ///
    /// # Errors
    ///
    /// Returns [`Error::is_invalid_path`] when `path` is empty or has an empty component,
    /// [`Error::is_missing`] when the path is absent, and [`Error::is_type_mismatch`] when the
    /// value cannot be read as `T`. The mismatch names the expected shape, never the value and
    /// never a caller-supplied serde message or expectation. Its source is the one that wrote the
    /// failing leaf when the snapshot records one, otherwise the nearest recorded ancestor: a
    /// failure below a struct field is attributed to that field's source, not to the map that
    /// contains it, and a failure inside an externally tagged enum payload is attributed through
    /// the variant key that holds the payload.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.validate_path(path)?;
        let Some(value) = self.resolve(path) else {
            return Err(Error::missing(self.full_path(path)));
        };
        let mut components: Vec<String> = self.cursor.to_vec();
        components.extend(path.split(self.delimiter).map(str::to_owned));
        let mut leaf = Vec::new();
        T::deserialize(Cursor::new(Node::Value(value), &mut leaf))
            .map_err(|error| self.mismatch(&components, &error))
    }

    /// Returns `default` only when the path is absent.
    ///
    /// A present null, false, zero, or empty value is returned as-is, and a present value of the
    /// wrong type is still an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::is_invalid_path`] when `path` is empty or has an empty component, and
    /// [`Error::is_type_mismatch`] when the value is present and cannot be read as `T`.
    pub fn get_or<T: DeserializeOwned>(&self, path: &str, default: T) -> Result<T, Error> {
        self.validate_path(path)?;
        if self.resolve(path).is_some() {
            self.get(path)
        } else {
            Ok(default)
        }
    }

    /// Deserializes the focused document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::is_type_mismatch`] when the document is not shaped like `T`. The source
    /// named is the one that wrote the failing leaf when the snapshot records one.
    pub fn load<T: DeserializeOwned>(&self) -> Result<T, Error> {
        let Some(value) = self.focused() else {
            return Err(Error::missing(join(&self.cursor, self.delimiter)));
        };
        let components: Vec<String> = self.cursor.to_vec();
        let mut leaf = Vec::new();
        T::deserialize(Cursor::new(Node::Value(value), &mut leaf))
            .map_err(|error| self.mismatch(&components, &error))
    }

    /// Returns the focused node without deserializing it.
    ///
    /// `None` only when the focused path is absent, which a `Config` obtained from
    /// [`Self::child`] or from [`Builder::build`] cannot be.
    #[must_use]
    pub fn value(&self) -> Option<&Value> {
        self.focused()
    }

    /// Returns the source that introduced or last replaced the node at `path`.
    ///
    /// A map created by an earlier source keeps that source when a later source only merges
    /// children into it. A later source that replaces a map with a scalar, or a scalar with a map,
    /// owns the whole subtree, and the stale provenance below it is dropped.
    ///
    /// A literal key that contains the delimiter has its own identity. It never answers for a
    /// nested path of the same spelling, because the path split decides which node is meant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::is_invalid_path`] when `path` is empty or has an empty component, and
    /// [`Error::is_missing`] when the path is absent.
    pub fn origin(&self, path: &str) -> Result<&str, Error> {
        self.validate_path(path)?;
        if self.resolve(path).is_none() {
            return Err(Error::missing(self.full_path(path)));
        }
        Ok(self.origin_for(path).unwrap_or(NO_ORIGIN))
    }

    /// Rejects an empty path or an empty component.
    fn validate_path(&self, path: &str) -> Result<(), Error> {
        if path.is_empty() || path.split(self.delimiter).any(str::is_empty) {
            return Err(Error::invalid_path(path));
        }
        Ok(())
    }

    /// Resolves a path that [`Self::validate_path`] already accepted.
    fn resolve(&self, path: &str) -> Option<&Value> {
        self.snapshot.value.descend(
            self.cursor
                .iter()
                .map(String::as_str)
                .chain(path.split(self.delimiter)),
        )
    }

    fn focused(&self) -> Option<&Value> {
        self.snapshot
            .value
            .descend(self.cursor.iter().map(String::as_str))
    }

    /// Returns the source for a path that [`Self::validate_path`] already accepted.
    fn origin_for(&self, path: &str) -> Option<&str> {
        let mut components: Vec<String> = self.cursor.to_vec();
        components.extend(path.split(self.delimiter).map(str::to_owned));
        self.snapshot
            .origin
            .get(components.as_slice())
            .map(|source| &**source)
    }

    /// Builds a type-mismatch error for the value at `components`.
    ///
    /// `error` names the leaf it failed on, relative to that value. Attribution goes to the source
    /// that wrote the failing leaf when the tree records one, otherwise to the nearest recorded
    /// ancestor. A leaf inside an array has no entry of its own, so the array's source, or the
    /// nearest map above it, answers for it. Omitting attribution when nothing is recorded is
    /// deliberate: a guess is worse than no name.
    fn mismatch(&self, components: &[String], error: &DeError) -> Error {
        let mut failing = components.to_vec();
        if let Some(leaf) = error.leaf() {
            failing.extend_from_slice(leaf);
        }
        Error::type_mismatch(
            join(components, self.delimiter),
            self.origin_below(&failing),
            error.expected(),
        )
    }

    /// The source of the deepest node at or above `components` that has one.
    fn origin_below(&self, components: &[String]) -> Option<&str> {
        for length in (0..=components.len()).rev() {
            if let Some(source) = self.snapshot.origin.get(&components[..length]) {
                return Some(source);
            }
        }
        None
    }

    /// The full path of `path` relative to this cursor, for an error.
    fn full_path(&self, path: &str) -> String {
        let mut output = join(&self.cursor, self.delimiter);
        if !output.is_empty() {
            output.push(self.delimiter);
        }
        output.push_str(path);
        output
    }
}
