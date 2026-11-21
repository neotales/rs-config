//! Generated, disposable KDBX fixtures.
//!
//! Every value here is a fixture literal created by this test module. The tests never read a real
//! vault, a user credential, or a file outside the temporary directory.

#![allow(dead_code, missing_docs)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use neotales_config::SourceError;
use neotales_config_keepass::{KeePassFile, Mapping};
use neotales_keepass::{
    Entry, Field, KdbxCredentials, KdbxFile, KdbxReadLimits, KdbxUuid, KdbxWriteOptions,
    KeePassDatabase, ProtectedValue,
};
use tempfile::TempDir;

pub(crate) const MASTER_PASSWORD: &str = "fixture-master-SENTINEL-4f2a";
pub(crate) const WRONG_PASSWORD: &str = "fixture-wrong-SENTINEL-8c1d";

pub(crate) const ENTRY_A: KdbxUuid = KdbxUuid::from_bytes([0x11; 16]);
pub(crate) const ENTRY_B: KdbxUuid = KdbxUuid::from_bytes([0x22; 16]);

pub(crate) const TITLE_A: &str = "fixture-entry-a";
pub(crate) const TITLE_B: &str = "fixture-entry-b";
pub(crate) const USERNAME_A: &str = "fixture-user-a";
pub(crate) const NOTES_A: &str = "fixture notes a";
pub(crate) const ENTRY_PASSWORD_A: &str = "fixture-entry-secret-SENTINEL-91be";
pub(crate) const TOKEN_A: &str = "fixture-token-alpha";
pub(crate) const TOKEN_B: &str = "fixture-token-beta";
pub(crate) const TOKEN_CHANGED: &str = "fixture-token-changed";
pub(crate) const TOKEN_RESTORED: &str = "fixture-token-restored";
pub(crate) const BLOCK_A: &str = "first-line\nsecond-line";

/// One temporary directory and its generated vault.
pub(crate) struct Fixture {
    directory: TempDir,
    vault: PathBuf,
}

impl Fixture {
    /// Writes a password-protected KDBX4 fixture with two entries.
    pub(crate) fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let vault = directory.path().join("fixture.kdbx");
        write_vault(&vault, TOKEN_A, TOKEN_B);
        Self { directory, vault }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.vault
    }

    pub(crate) fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Replaces the vault with a new generation of the same two entries.
    pub(crate) fn write(&self, token_a: &str, token_b: &str) {
        write_vault(&self.vault, token_a, token_b);
    }

    /// Replaces the vault with bytes that are not a KDBX document.
    pub(crate) fn corrupt(&self) {
        fs::write(&self.vault, b"fixture-not-a-kdbx-document").expect("write corrupt vault");
    }
}

/// The standard set of mappings used by the load and reload tests.
pub(crate) fn provider(fixture: &Fixture, calls: Arc<AtomicUsize>) -> KeePassFile {
    KeePassFile::new(
        fixture.path().to_path_buf(),
        KdbxReadLimits::default(),
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
        },
    )
    .mapping(Mapping::new(ENTRY_A, "config.user", "app.db.user"))
    .mapping(Mapping::new(ENTRY_A, "config.password", "app.db.password"))
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"))
    .mapping(Mapping::new(ENTRY_A, "config.block", "app.block"))
    .mapping(Mapping::new(ENTRY_A, "Title", "entry.title"))
    .mapping(Mapping::new(ENTRY_A, "UserName", "entry.user"))
    .mapping(Mapping::new(ENTRY_B, "config.token", "other.token"))
}

/// Writes the vault bytes as one complete file, the way a caller replacing a vault would.
fn write_vault(path: &Path, token_a: &str, token_b: &str) {
    let database = database(token_a, token_b);
    let mut credentials = KdbxCredentials::password(MASTER_PASSWORD).expect("fixture credentials");
    let mut bytes = Vec::new();
    KdbxFile::save(
        &database,
        &mut bytes,
        &mut credentials,
        KdbxWriteOptions {
            // A low AES-KDF cost keeps a generated fixture fast; it is not a
            // recommendation for a real vault.
            aes_kdf_rounds: 8,
            ..KdbxWriteOptions::default()
        },
        &KdbxReadLimits::default(),
    )
    .expect("save fixture vault");
    fs::write(path, bytes).expect("write fixture vault");
}

fn database(token_a: &str, token_b: &str) -> KeePassDatabase {
    let mut database = KeePassDatabase::default();

    let mut entry_a = Entry::new(TITLE_A);
    entry_a.id = ENTRY_A;
    entry_a.username = USERNAME_A.into();
    entry_a.password = ProtectedValue::from_text(ENTRY_PASSWORD_A);
    entry_a.notes = NOTES_A.into();
    entry_a.set_field(text_field("config.user", USERNAME_A, false));
    entry_a.set_field(text_field("config.password", ENTRY_PASSWORD_A, true));
    entry_a.set_field(text_field("config.token", token_a, true));
    entry_a.set_field(text_field("config.block", BLOCK_A, false));
    database.root.add_entry(entry_a);

    let mut entry_b = Entry::new(TITLE_B);
    entry_b.id = ENTRY_B;
    entry_b.set_field(text_field("config.token", token_b, true));
    database.root.add_entry(entry_b);

    database
}

fn text_field(key: &str, value: &str, protected: bool) -> Field {
    Field {
        key: key.into(),
        value: ProtectedValue::from_text(value),
        protected,
        protect_in_memory: protected,
    }
}
