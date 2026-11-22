use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry as MapEntry},
    fmt,
    fs::File,
    io,
    path::{Path, PathBuf},
};

use neotales_config::{Source, SourceError, Value};
use neotales_keepass::{
    Entry, KdbxCredentials, KdbxFailureKind, KdbxFile, KdbxReadLimits, ProtectedValue,
};

use crate::Mapping;

/// The path delimiters the core builder accepts.
///
/// The provider validates against the same fixed set so a mapping cannot be built with a
/// delimiter the builder would reject later. Extending the set is a deliberate change in the core
/// crate, not a per-caller option.
const SUPPORTED_DELIMITERS: [char; 5] = ['.', ':', '/', ',', '|'];

/// A caller-supplied source of KeePass credentials.
///
/// The provider calls [`CredentialFactory::credentials`] at most once for every **load**, after
/// the mappings validate and the file opens successfully. A load is the initial build, every
/// manual `neotales_config::Reload::refresh`, and a polling pass that actually loads because a
/// watched file changed or an earlier load failed. A polling pass over unchanged files does not
/// load and does not call the factory, and a load that fails validation or cannot open the file
/// calls it zero times. The reload watcher fingerprints only the KDBX path, so a change in the
/// credential source alone is invisible to it; call `neotales_config::Reload::refresh` after
/// rotating a credential. The returned credentials are dropped after that one load.
///
/// The factory is the only credential path. The provider never reads a password itself, never
/// opens a key file, never discovers a vault, and never prompts. Implement the factory with a value
/// the process already holds, or with a platform secret store, and do not make it interactive: it
/// runs on whichever thread performs the load, including the watcher thread.
///
/// The error type is [`SourceError`], which carries a kind and no message. That is deliberate: a
/// credential store's own error can hold a path, a user, or a token, and the provider must not be
/// able to forward it. Return [`SourceError::unavailable`] when the credentials could not be
/// obtained.
///
/// A blanket implementation covers a closure of type
/// `Fn() -> Result<KdbxCredentials, SourceError> + Send + Sync`.
///
/// ```
/// use neotales_config::SourceError;
/// use neotales_config_keepass::CredentialFactory;
/// use neotales_keepass::KdbxCredentials;
///
/// struct Fixed(Option<&'static [u8]>);
///
/// impl CredentialFactory for Fixed {
///     fn credentials(&self) -> Result<KdbxCredentials, SourceError> {
///         let password = self.0.ok_or(SourceError::unavailable())?;
///         KdbxCredentials::password(password).map_err(|_| SourceError::unavailable())
///     }
/// }
/// ```
pub trait CredentialFactory: Send + Sync {
    /// Returns fresh credentials for one load.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::unavailable`] when the credential value cannot be obtained, or
    /// another kind-only error. The text is never forwarded by the provider and must not carry a
    /// value.
    fn credentials(&self) -> Result<KdbxCredentials, SourceError>;
}

impl<F> CredentialFactory for F
where
    F: Fn() -> Result<KdbxCredentials, SourceError> + Send + Sync,
{
    fn credentials(&self) -> Result<KdbxCredentials, SourceError> {
        self()
    }
}

/// A KeePass KDBX file read as one configuration source.
///
/// Construct one with [`KeePassFile::new`], add the explicit [`Mapping`]s, and register it with
/// [`neotales_config::Builder::source`]. The structure is:
///
/// - the KDBX path is read once per [`Source::load`], always read-only, and the open file is
///   passed to the codec as a stream so the provider never buffers the whole vault;
/// - the credential factory is called at most once per load, after validation and a successful
///   file open;
/// - the read limits are the ones supplied at construction;
/// - every mapped field becomes a [`Value::String`] at the configured path.
///
/// By default the vault is required and its path is watched. [`Self::optional`] and
/// [`Self::reload_on_change`] change those two defaults independently. An optional vault that is
/// absent is an empty map, and the credential factory is not called because no file was opened. A
/// path that is a directory, a permission failure, a corrupt or truncated document, wrong
/// credentials, an exceeded limit, and a mapping that does not resolve all still reject the
/// source. The open result itself decides absence: there is no separate existence check.
///
/// # Configuration validation
///
/// Validation runs before the vault is opened and before the credential factory is called. The
/// source rejects, with [`SourceError::invalid`], an unsupported delimiter, a delimiter outside the
/// core set, no mappings at all, an empty field key, an empty path or empty path component, the
/// same entry and field mapped twice, the same configuration path mapped twice, and a path that is
/// a prefix of another path (which would need one node to be both a scalar and a map).
///
/// A mapping that names an absent entry, an absent field, or a field whose bytes are not UTF-8 also
/// rejects the source. A missing mapping is never skipped, so a partial tree is never published.
/// Presence is taken from the entry's ordered field list only: the provider does not populate a
/// missing field from a codec property. See the crate documentation for the `Title` limitation.
///
/// # Errors
///
/// A file that cannot be opened is [`SourceError::unavailable`], including an absent required
/// vault. With [`Self::optional`], only an actual [`std::io::ErrorKind::NotFound`] from opening
/// the path is an empty map instead. Wrong credentials, a malformed or truncated KDBX document,
/// exceeded read limits, and a mapping that does not resolve are [`SourceError::invalid`]. A
/// credential-factory error keeps the kind the factory chose. The provider never forwards the
/// `neotales-keepass` error text, which is why the error cannot name a path, an entry, a field, or
/// a value.
///
/// # Secrets
///
/// Every mapped value is stored in the published snapshot as plaintext. See the crate
/// documentation for the lifetime and the limits of that statement.
///
/// # Example
///
/// ```no_run
/// use neotales_config::{Builder, SourceError};
/// use neotales_config_keepass::{KeePassFile, Mapping};
/// use neotales_keepass::{KdbxCredentials, KdbxReadLimits, KdbxUuid};
///
/// # fn main() -> Result<(), neotales_config::Error> {
/// let entry = KdbxUuid::from_base64("EREREREREREREREREREREQ==").expect("fixed UUID");
/// let vault = KeePassFile::new(
///     "secrets/application.kdbx",
///     KdbxReadLimits::default(),
///     || {
///         KdbxCredentials::password(b"from-the-caller")
///             .map_err(|_| SourceError::unavailable())
///     },
/// )
/// .mapping(Mapping::new(entry, "UserName", "database.user"))
/// .mapping(Mapping::new(entry, "Password", "database.password"));
///
/// let config = Builder::new().source("vault", vault).build()?;
/// assert!(config.has("database.password")?);
/// # Ok(())
/// # }
/// ```
pub struct KeePassFile {
    path: PathBuf,
    limits: KdbxReadLimits,
    credentials: Box<dyn CredentialFactory>,
    mappings: Vec<Mapping>,
    delimiter: char,
    optional: bool,
    reload_on_change: bool,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path, the credential factory, and the mappings are deliberately unreportable; \
              only counts, the delimiter, the flags, and the non-secret limits are printed"
)]
impl fmt::Debug for KeePassFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("KeePassFile")
            .field("mappings", &self.mappings.len())
            .field("delimiter", &self.delimiter)
            .field("limits", &self.limits)
            .field("optional", &self.optional)
            .field("reload_on_change", &self.reload_on_change)
            .finish_non_exhaustive()
    }
}

impl KeePassFile {
    /// Registers one KDBX file with explicit read limits and a credential factory.
    ///
    /// The file is not opened here. The path is used by [`Source::load`] and, by default, reported
    /// by [`Source::watch_paths`]; it may be relative to the process's working directory. The
    /// vault is required and watched by default; see [`Self::optional`] and
    /// [`Self::reload_on_change`]. The provider never creates, replaces, or writes the file.
    ///
    /// Use [`KdbxReadLimits`] to set the input, header, KDF, block, decompression, XML, and binary
    /// budgets. The value is used unchanged for every load; the provider adds no bound of its own.
    /// At load the file is opened and handed to the codec as a stream, so the provider never holds
    /// a whole-vault buffer.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        limits: KdbxReadLimits,
        credentials: impl CredentialFactory + 'static,
    ) -> Self {
        Self {
            path: path.into(),
            limits,
            credentials: Box::new(credentials),
            mappings: Vec::new(),
            delimiter: '.',
            optional: false,
            reload_on_change: true,
        }
    }

    /// Treats an absent vault as an empty source instead of an error.
    ///
    /// The default is `false`, so an absent vault fails the load with
    /// [`SourceError::unavailable`] exactly as before. With `true`, only an actual
    /// [`std::io::ErrorKind::NotFound`] from opening the path becomes an empty map. The credential
    /// factory is not called for an absent optional vault, because no file was opened: the
    /// documented order is validation, a successful open, then one credential call. A path that is
    /// a directory, a permission failure, a corrupt or truncated document, wrong credentials, an
    /// exceeded limit, and a mapping that does not resolve all still reject the source. The open
    /// result itself decides: there is no separate existence check for a vault to appear or vanish
    /// between.
    ///
    /// An optional vault that is absent may appear later, and a watched optional vault that
    /// disappears is a change the next refresh observes: the values it contributed are dropped and
    /// any value from an earlier source shows through again. A corrupt replacement, wrong
    /// credentials, and an exceeded limit are not an absence; each fails the refresh and keeps the
    /// previous snapshot published.
    #[must_use]
    pub const fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Reports the KDBX path to the polling watcher.
    ///
    /// The default is `true`. With `false`, [`Source::watch_paths`] returns an empty list for this
    /// source, so `Reload::refresh_if_changed` and `Reload::start_watching` do not poll the vault;
    /// `Reload::refresh` still loads every source, including this one. If no source reports a path,
    /// `Reload::start_watching` returns `WatchError::NoWatchableSources`.
    ///
    /// This setting is independent of [`Self::optional`]: it changes what is observed, not whether
    /// an absent vault is accepted. A change in the credential source is never observable here;
    /// call `Reload::refresh` after rotating a credential.
    #[must_use]
    pub const fn reload_on_change(mut self, reload_on_change: bool) -> Self {
        self.reload_on_change = reload_on_change;
        self
    }

    /// Selects the delimiter used to split [`Mapping::path`] values.
    ///
    /// The default is `.`. The supported set is the core builder's: `.`, `:`, `/`, `,`, and `|`.
    /// Any other character makes [`Source::load`] fail with [`SourceError::invalid`] before the
    /// vault is opened. To read a mapped path back with a string path, configure the core builder
    /// with the same delimiter.
    ///
    /// The delimiter never changes a field key: keys are matched literally and are never split.
    #[must_use]
    pub const fn delimiter(mut self, delimiter: char) -> Self {
        self.delimiter = delimiter;
        self
    }

    /// Adds one explicit entry-field-to-path mapping.
    ///
    /// Mappings are validated together at load, because duplicate and prefix collisions depend on
    /// the whole set. Adding two mappings for the same entry and field, adding the same path twice,
    /// and mapping both a parent and a child path all reject the source rather than resolving by
    /// order.
    #[must_use]
    pub fn mapping(mut self, mapping: Mapping) -> Self {
        self.mappings.push(mapping);
        self
    }

    /// Adds several explicit mappings in iterator order.
    ///
    /// Order does not affect precedence: every mapping is placed in its own path, and collisions
    /// reject the source. It matters only for the order the tree is assembled.
    #[must_use]
    pub fn mappings(mut self, mappings: impl IntoIterator<Item = Mapping>) -> Self {
        self.mappings.extend(mappings);
        self
    }

    /// Returns the configured KDBX path.
    ///
    /// The path is not part of an error or of this type's [`fmt::Debug`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the configured read limits.
    ///
    /// The limits are not secret and are reported by [`fmt::Debug`].
    #[must_use]
    pub fn limits(&self) -> KdbxReadLimits {
        self.limits
    }

    /// Validates the delimiter and the mappings, and returns one component list per mapping.
    ///
    /// Nothing here reads the vault or the credentials.
    fn validated_paths(&self) -> Result<Vec<Vec<String>>, SourceError> {
        if !SUPPORTED_DELIMITERS.contains(&self.delimiter) {
            return Err(SourceError::invalid());
        }
        if self.mappings.is_empty() {
            return Err(SourceError::invalid());
        }

        let mut paths = Vec::with_capacity(self.mappings.len());
        let mut seen = BTreeSet::new();
        for mapping in &self.mappings {
            if mapping.field().is_empty() {
                return Err(SourceError::invalid());
            }
            if !seen.insert((mapping.entry(), mapping.field().to_owned())) {
                return Err(SourceError::invalid());
            }
            let components: Vec<String> = mapping
                .path()
                .split(self.delimiter)
                .map(str::to_owned)
                .collect();
            if components.iter().any(String::is_empty) {
                return Err(SourceError::invalid());
            }
            paths.push(components);
        }

        // Sorting puts a duplicate or a prefix immediately before the longer path it collides
        // with, so one adjacent comparison covers both cases.
        let mut sorted = paths.clone();
        sorted.sort();
        if sorted.windows(2).any(|pair| pair[1].starts_with(&pair[0])) {
            return Err(SourceError::invalid());
        }
        Ok(paths)
    }

    /// Reads the vault once and builds the mapped tree.
    fn load_tree(&self) -> Result<BTreeMap<String, Value>, SourceError> {
        let paths = self.validated_paths()?;

        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if self.optional && error.kind() == io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(_) => return Err(SourceError::unavailable()),
        };
        let mut credentials = self.credentials.credentials()?;
        let database = KdbxFile::load(file, &mut credentials, &self.limits)
            .map_err(|error| map_failure(error.kind))?;

        let mut root = BTreeMap::new();
        for (mapping, components) in self.mappings.iter().zip(paths) {
            let entry = database
                .find_entry(mapping.entry())
                .ok_or_else(SourceError::invalid)?;
            let text = field_text(entry, mapping.field())?;
            insert(&mut root, &components, Value::String(text))?;
        }
        Ok(root)
    }
}

impl Source for KeePassFile {
    fn load(&self) -> Result<Value, SourceError> {
        self.load_tree().map(Value::Map)
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        // The path is reported even when the file is absent: creation, replacement, and deletion
        // are all changes the reload watcher should poll for. The credential source is not a path
        // and is not reported; a credential-only change needs a manual refresh.
        if self.reload_on_change {
            vec![self.path.clone()]
        } else {
            Vec::new()
        }
    }
}

/// Maps a KDBX failure to a kind-only source error without reading its message.
fn map_failure(kind: KdbxFailureKind) -> SourceError {
    if matches!(kind, KdbxFailureKind::Io) {
        SourceError::unavailable()
    } else {
        SourceError::invalid()
    }
}

/// Returns the exact field value as text.
///
/// Presence is exactly [`Entry::field`]: the codec keeps every KDBX `<String>` element in the
/// ordered field list, including the conventional `Title`, `UserName`, `Password`, `URL`, and
/// `Notes` elements.
/// An absent field is rejected rather than read from an [`Entry`] property, because the published
/// codec does not preserve presence for a property: a missing `<Name>` becomes `"Entry"`, and the
/// other properties are mirrored from this same list. An explicitly empty field is present and
/// therefore yields an empty string.
fn field_text(entry: &Entry, name: &str) -> Result<String, SourceError> {
    let field = entry.field(name).ok_or_else(SourceError::invalid)?;
    reveal(&field.value)
}

/// Copies a protected value out as plaintext, rejecting non-UTF-8 bytes.
fn reveal(value: &ProtectedValue) -> Result<String, SourceError> {
    value
        .reveal_text()
        .map(|text| text.as_str().to_owned())
        .map_err(|_| SourceError::invalid())
}

/// Inserts one mapped value into the tree at `components`.
///
/// Validation has already rejected every prefix collision, so the walk never meets a scalar where
/// it needs a map. The check is kept rather than asserted: a provider that panics on a configuration
/// mistake is worse than one that rejects the load.
fn insert(
    root: &mut BTreeMap<String, Value>,
    components: &[String],
    value: Value,
) -> Result<(), SourceError> {
    let Some((leaf, parents)) = components.split_last() else {
        return Err(SourceError::invalid());
    };
    let mut map = root;
    for component in parents {
        if let MapEntry::Vacant(entry) = map.entry(component.clone()) {
            entry.insert(Value::Map(BTreeMap::new()));
        }
        match map.get_mut(component) {
            Some(Value::Map(child)) => map = child,
            _ => return Err(SourceError::invalid()),
        }
    }
    map.insert(leaf.clone(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use neotales_keepass::{Entry, Field, ProtectedValue};

    use super::field_text;

    fn field(key: &str, value: &str, protected: bool) -> Field {
        Field {
            key: key.into(),
            value: ProtectedValue::from_text(value),
            protected,
            protect_in_memory: protected,
        }
    }

    #[test]
    fn a_codec_property_does_not_answer_for_an_absent_field() {
        let mut entry = Entry::new("fixture title");
        entry.username = "fixture user".into();
        entry.password = ProtectedValue::from_text("fixture password");
        entry.url = "fixture url".into();
        entry.notes = "fixture notes".into();

        // The codec properties are populated, but a property is not field presence: `Entry::title`
        // is a separate `<Name>` value. A fallback would return `"fixture title"` for an absent
        // Title, which is exactly what this provider must not do.
        for name in ["Title", "UserName", "Password", "URL", "Notes"] {
            assert!(
                field_text(&entry, name).is_err(),
                "an absent field must not be filled from a property: {name}"
            );
        }
    }

    #[test]
    fn an_explicit_field_answers_including_an_empty_value() {
        let mut entry = Entry::new("fixture title");
        for (name, value, protected) in [
            ("Title", "fixture title field", false),
            ("UserName", "fixture user field", false),
            ("Password", "fixture password field", true),
            ("URL", "fixture url field", false),
            ("Notes", "", false),
        ] {
            entry.set_field(field(name, value, protected));
            assert_eq!(
                field_text(&entry, name).expect("the explicit field answers"),
                value
            );
        }
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        let entry = Entry::new("fixture title");
        assert!(field_text(&entry, "missing").is_err());
    }
}
