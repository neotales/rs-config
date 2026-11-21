//! Generated, disposable KDBX fixtures for the benchmark.
//!
//! Every value here is a fixture literal created by this module. Nothing reads a
//! real vault, a user credential, or a file outside the temporary directory. The
//! master password is a synthetic constant that protects a disposable vault for
//! the length of one benchmark run.

#![allow(
    dead_code,
    reason = "helpers used only when the matching feature is enabled"
)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use neotales_config::SourceError;
use neotales_config_keepass::{KeePassFile, Mapping};
use neotales_keepass::{
    Entry, Field, KdbxCompression, KdbxCredentials, KdbxFile, KdbxReadLimits, KdbxUuid,
    KdbxWriteOptions, KeePassDatabase, ProtectedValue,
};
use tempfile::TempDir;

/// A synthetic fixture password. It protects no real data.
pub(crate) const MASTER_PASSWORD: &str = "fixture-bench-master-SENTINEL";

/// The small vault has one entry, like a hand-written test vault.
pub(crate) const SMALL_ENTRIES: usize = 1;

/// The realistic vault has enough entries that mapping and XML parsing are visible.
pub(crate) const REALISTIC_ENTRIES: usize = 48;

/// The low AES-KDF round count used to isolate the provider from key derivation.
///
/// It is a fixture setting, not a recommendation: the codec's own default is
/// [`REALISTIC_KDF_ROUNDS`].
pub(crate) const FIXTURE_KDF_ROUNDS: u64 = 8;

/// The codec's default AES-KDF round count, used by the realistic-cost vault.
pub(crate) const REALISTIC_KDF_ROUNDS: u64 = 600_000;

/// The fields a realistic entry carries in the vault.
const ENTRY_FIELDS: [(&str, bool); 9] = [
    ("Title", false),
    ("UserName", false),
    ("Password", true),
    ("URL", false),
    ("Notes", false),
    ("config.user", false),
    ("config.password", true),
    ("config.token", true),
    ("config.notes", false),
];

/// The fields the provider maps, and the path suffix each one becomes.
const MAPPED_FIELDS: [(&str, &str); 6] = [
    ("Title", "title"),
    ("UserName", "username"),
    ("config.user", "user"),
    ("config.password", "password"),
    ("config.token", "token"),
    ("config.notes", "notes"),
];

/// Three generated vaults in one temporary directory.
pub(crate) struct Fixtures {
    // Kept for its destructor: dropping it removes every generated vault.
    _directory: TempDir,
    small: PathBuf,
    realistic: PathBuf,
    realistic_kdf: PathBuf,
}

impl Fixtures {
    /// Writes the three vaults.
    ///
    /// The small vault and the low-cost realistic vault use
    /// [`FIXTURE_KDF_ROUNDS`], so the rest of the load is measurable. The
    /// realistic-cost vault uses the codec's default AES-KDF cost and gzip, and
    /// measures the full key-derivation path once per iteration.
    pub(crate) fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let small = directory.path().join("small.kdbx");
        let realistic = directory.path().join("realistic.kdbx");
        let realistic_kdf = directory.path().join("realistic-kdf.kdbx");

        write_vault(
            &small,
            SMALL_ENTRIES,
            FIXTURE_KDF_ROUNDS,
            KdbxCompression::None,
        );
        write_vault(
            &realistic,
            REALISTIC_ENTRIES,
            FIXTURE_KDF_ROUNDS,
            KdbxCompression::GZip,
        );
        write_vault(
            &realistic_kdf,
            REALISTIC_ENTRIES,
            REALISTIC_KDF_ROUNDS,
            KdbxCompression::GZip,
        );

        Self {
            _directory: directory,
            small,
            realistic,
            realistic_kdf,
        }
    }

    /// The one-entry vault.
    pub(crate) fn small(&self) -> &Path {
        &self.small
    }

    /// The 48-entry vault with the fixture AES-KDF cost.
    pub(crate) fn realistic(&self) -> &Path {
        &self.realistic
    }

    /// The 48-entry vault with the codec's default AES-KDF cost.
    pub(crate) fn realistic_kdf(&self) -> &Path {
        &self.realistic_kdf
    }
}

/// Returns a small in-memory overlay used by the build benchmark.
pub(crate) fn overlay() -> neotales_config::Value {
    neotales_config::Value::map([(
        "entries",
        neotales_config::Value::map([(
            "entry_00",
            neotales_config::Value::map([(
                "user",
                neotales_config::Value::from("fixture-overlay"),
            )]),
        )]),
    )])
}

/// Returns a provider with one mapping for every mapped field of every entry.
pub(crate) fn provider(path: &Path, entries: usize) -> KeePassFile {
    let mut mappings = Vec::with_capacity(entries * MAPPED_FIELDS.len());
    for index in 0..entries {
        let entry = entry_uuid(index);
        for (field, suffix) in MAPPED_FIELDS {
            mappings.push(Mapping::new(
                entry,
                field,
                format!("entries.entry_{index:02}.{suffix}"),
            ));
        }
    }
    KeePassFile::new(path, KdbxReadLimits::default(), || {
        KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
    })
    .mappings(mappings)
}

/// Returns the stable fixture UUID for one entry.
pub(crate) fn entry_uuid(index: usize) -> KdbxUuid {
    let mut bytes = [0_u8; 16];
    bytes[0] = u8::try_from(index + 1).expect("the fixture entry index fits u8");
    bytes[15] = 0x5A;
    KdbxUuid::from_bytes(bytes)
}

fn write_vault(path: &Path, entries: usize, rounds: u64, compression: KdbxCompression) {
    let database = database(entries);
    let mut credentials = KdbxCredentials::password(MASTER_PASSWORD).expect("fixture credentials");
    let mut bytes = Vec::new();
    KdbxFile::save(
        &database,
        &mut bytes,
        &mut credentials,
        KdbxWriteOptions {
            aes_kdf_rounds: rounds,
            compression,
            ..KdbxWriteOptions::default()
        },
        &KdbxReadLimits::default(),
    )
    .expect("save fixture vault");
    fs::write(path, bytes).expect("write fixture vault");
}

fn database(entries: usize) -> KeePassDatabase {
    let mut database = KeePassDatabase::default();
    for index in 0..entries {
        let mut entry = Entry::new(format!("bench-entry-{index:02}"));
        entry.id = entry_uuid(index);
        entry.username = format!("fixture-user-{index:02}");
        entry.password = ProtectedValue::from_text(format!("fixture-pass-{index:02}"));
        entry.url = format!("https://fixture.example/entry/{index:02}");
        entry.notes = format!("fixture notes {index:02}");

        for (key, protected) in ENTRY_FIELDS {
            entry.set_field(Field {
                key: key.into(),
                value: ProtectedValue::from_text(format!("fixture-value-{index:02}-{key}")),
                protected,
                protect_in_memory: protected,
            });
        }
        database.root.add_entry(entry);
    }
    database
}
