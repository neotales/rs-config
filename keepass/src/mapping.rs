use std::fmt;

use neotales_keepass::KdbxUuid;

/// One explicit mapping from a KeePass entry field to a configuration path.
///
/// The entry is named by its stable UUID and the field by its exact, case-sensitive key. Nothing
/// else is consulted: two entries with the same title are different entries, and a field that is
/// absent rejects the whole source rather than being skipped.
///
/// A mapping is registered with [`crate::KeePassFile::mapping`] or
/// [`crate::KeePassFile::mappings`]. The path is split on the provider's configured delimiter when
/// the source loads, so the same [`Mapping`] is interpreted consistently on every refresh.
///
/// # Secrets
///
/// The field key and the path are configuration metadata, not a value, but they can still describe
/// what a process holds. [`fmt::Debug`] prints neither, and neither appears on an error.
#[derive(Clone, Eq, PartialEq)]
pub struct Mapping {
    entry: KdbxUuid,
    field: String,
    path: String,
}

impl Mapping {
    /// Creates a mapping from an entry UUID and a field key to a configuration path.
    ///
    /// The path is not validated here. [`crate::KeePassFile`] validates all mappings before it
    /// reads the vault, so a bad path fails one load and never produces a partial tree.
    #[must_use]
    pub fn new(entry: KdbxUuid, field: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            entry,
            field: field.into(),
            path: path.into(),
        }
    }

    /// Returns the mapped entry UUID.
    #[must_use]
    pub const fn entry(&self) -> KdbxUuid {
        self.entry
    }

    /// Returns the mapped field key.
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the mapped configuration path, before it is split on the delimiter.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Debug for Mapping {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The UUID, key, and path are all metadata a log line does not need, and the path can
        // name a host or a user. The shape is the only safe report.
        output.debug_struct("Mapping").finish_non_exhaustive()
    }
}
