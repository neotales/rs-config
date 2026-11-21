//! Loading, mapping, validation, credential, and redaction behavior.
//!
//! The fixtures are generated in a temporary directory by this test binary. No real vault, user
//! credential, or path outside that directory is read.

#![allow(missing_docs)]

mod support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use neotales_config::{Builder, Source, SourceError, SourceErrorKind};
use neotales_config_keepass::{KeePassFile, Mapping};
use neotales_keepass::{KdbxCredentials, KdbxReadLimits, KdbxUuid};
use support::{
    BLOCK_A, ENTRY_A, ENTRY_ABSENT, ENTRY_EMPTY, ENTRY_PASSWORD_A, Fixture, MASTER_PASSWORD,
    NOTES_A, PresenceFixture, STANDARD_FIELDS, TITLE_FIELD_A, TOKEN_A, TOKEN_B, URL_A, USERNAME_A,
    WRONG_PASSWORD,
};

fn blank(fixture: &Fixture) -> KeePassFile {
    KeePassFile::new(
        fixture.path().to_path_buf(),
        KdbxReadLimits::default(),
        || KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable()),
    )
}

fn invalid(provider: &KeePassFile) {
    let error = provider.load().expect_err("the source is rejected");
    assert_eq!(error.kind(), SourceErrorKind::Invalid, "{error:?}");
}

#[test]
fn maps_explicit_entries_and_fields_to_nested_paths() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));

    let config = Builder::new()
        .source("vault", support::provider(&fixture, Arc::clone(&calls)))
        .build()
        .expect("the vault loads");

    assert_eq!(
        config.get::<String>("app.db.user").expect("mapped"),
        USERNAME_A
    );
    assert_eq!(
        config.get::<String>("app.db.password").expect("mapped"),
        ENTRY_PASSWORD_A
    );
    assert_eq!(config.get::<String>("app.token").expect("mapped"), TOKEN_A);
    assert_eq!(config.get::<String>("app.block").expect("mapped"), BLOCK_A);
    assert_eq!(
        config.get::<String>("other.token").expect("mapped"),
        TOKEN_B
    );
    assert_eq!(
        config.get::<String>("entry.title").expect("mapped"),
        TITLE_FIELD_A
    );
    assert_eq!(
        config.get::<String>("entry.user").expect("mapped"),
        USERNAME_A
    );
    assert_eq!(
        config.get::<String>("entry.password").expect("mapped"),
        ENTRY_PASSWORD_A
    );
    assert_eq!(config.get::<String>("entry.url").expect("mapped"), URL_A);
    assert_eq!(
        config.get::<String>("entry.notes").expect("mapped"),
        NOTES_A
    );
    assert_eq!(config.origin("app.db.user").expect("mapped"), "vault");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn standard_fields_are_read_from_the_ordered_field_list() {
    let fixture = Fixture::new();
    let config = Builder::new()
        .source(
            "vault",
            support::provider(&fixture, Arc::new(AtomicUsize::new(0))),
        )
        .build()
        .expect("the vault loads");

    // The `Title` field differs from the entry's `<Name>` (`TITLE_A`), so this proves the
    // provider reads the explicit field list rather than the codec's `title` property.
    assert_eq!(
        config.get::<String>("entry.title").expect("mapped"),
        TITLE_FIELD_A
    );
    assert_eq!(
        config.get::<String>("entry.user").expect("mapped"),
        USERNAME_A
    );
}

#[test]
fn an_absent_standard_field_is_rejected_not_filled_from_a_codec_property() {
    let fixture = PresenceFixture::new();

    // The absent entry has `<Name>standard-absent</Name>` and no standard `<String>` elements.
    // A fallback would return that name for Title or empty strings for the other four.
    for field in STANDARD_FIELDS {
        invalid(&fixture.provider(ENTRY_ABSENT, field));
    }
}

#[test]
fn an_explicitly_empty_standard_field_reads_as_an_empty_string() {
    let fixture = PresenceFixture::new();

    for field in STANDARD_FIELDS {
        let config = Builder::new()
            .source("vault", fixture.provider(ENTRY_EMPTY, field))
            .build()
            .expect("the present field loads");
        assert_eq!(
            config.get::<String>("probe.value").expect("mapped"),
            "",
            "an explicitly empty {field} is present and empty, not absent"
        );
    }
}

#[test]
fn a_mapping_exposes_its_parts_but_not_in_debug() {
    let mapping = Mapping::new(ENTRY_A, "config.token", "app.token");
    assert_eq!(mapping.entry(), ENTRY_A);
    assert_eq!(mapping.field(), "config.token");
    assert_eq!(mapping.path(), "app.token");
    assert_eq!(format!("{mapping:?}"), "Mapping { .. }");
}

#[test]
fn a_configured_delimiter_splits_mapped_paths() {
    let fixture = Fixture::new();
    let provider = blank(&fixture).delimiter(':').mapping(Mapping::new(
        ENTRY_A,
        "config.token",
        "vault:app:token",
    ));

    let config = Builder::new()
        .delimiter(':')
        .source("vault", provider)
        .build()
        .expect("the vault loads");

    assert_eq!(
        config.get::<String>("vault:app:token").expect("mapped"),
        TOKEN_A
    );
    assert!(
        config
            .get::<String>("vault.app.token")
            .expect_err("the delimiter decides the components")
            .is_missing()
    );
}

#[test]
fn no_mapping_is_rejected() {
    invalid(&blank(&Fixture::new()));
}

#[test]
fn an_unsupported_delimiter_is_rejected() {
    let fixture = Fixture::new();
    invalid(&blank(&fixture).delimiter('a').mapping(Mapping::new(
        ENTRY_A,
        "config.token",
        "app.token",
    )));
}

#[test]
fn an_empty_field_key_is_rejected() {
    let fixture = Fixture::new();
    invalid(&blank(&fixture).mapping(Mapping::new(ENTRY_A, "", "app.token")));
}

#[test]
fn an_empty_or_ambiguous_path_is_rejected() {
    let fixture = Fixture::new();
    for path in ["", ".", "a.", ".a", "a..b"] {
        invalid(&blank(&fixture).mapping(Mapping::new(ENTRY_A, "config.token", path)));
    }
}

#[test]
fn one_entry_and_field_mapped_twice_is_rejected() {
    let fixture = Fixture::new();
    invalid(
        &blank(&fixture)
            .mapping(Mapping::new(ENTRY_A, "config.token", "app.one"))
            .mapping(Mapping::new(ENTRY_A, "config.token", "app.two")),
    );
}

#[test]
fn one_path_mapped_twice_is_rejected() {
    let fixture = Fixture::new();
    invalid(
        &blank(&fixture)
            .mapping(Mapping::new(ENTRY_A, "config.token", "app.one"))
            .mapping(Mapping::new(ENTRY_A, "config.user", "app.one")),
    );
}

#[test]
fn a_parent_and_child_path_are_rejected() {
    let fixture = Fixture::new();
    invalid(
        &blank(&fixture)
            .mapping(Mapping::new(ENTRY_A, "config.token", "app"))
            .mapping(Mapping::new(ENTRY_A, "config.user", "app.user")),
    );
}

#[test]
fn a_missing_entry_is_rejected() {
    let fixture = Fixture::new();
    let unknown = KdbxUuid::from_bytes([0x99; 16]);
    invalid(&blank(&fixture).mapping(Mapping::new(unknown, "config.token", "app.token")));
}

#[test]
fn a_missing_field_is_rejected() {
    let fixture = Fixture::new();
    invalid(&blank(&fixture).mapping(Mapping::new(ENTRY_A, "not-a-field", "app.token")));
}

#[test]
fn wrong_credentials_are_rejected_without_reading_the_value() {
    let fixture = Fixture::new();
    let provider = KeePassFile::new(
        fixture.path().to_path_buf(),
        KdbxReadLimits::default(),
        || KdbxCredentials::password(WRONG_PASSWORD).map_err(|_| SourceError::unavailable()),
    )
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"));

    assert_eq!(
        provider.load().expect_err("wrong credentials").kind(),
        SourceErrorKind::Invalid
    );
}

#[test]
fn an_unreadable_file_is_unavailable() {
    let fixture = Fixture::new();
    let provider = KeePassFile::new(
        fixture.directory().join("absent.kdbx"),
        KdbxReadLimits::default(),
        || KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable()),
    )
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"));

    assert_eq!(
        provider.load().expect_err("the file is absent").kind(),
        SourceErrorKind::Unavailable
    );
}

#[test]
fn a_credential_factory_error_keeps_its_kind() {
    let fixture = Fixture::new();
    let provider = KeePassFile::new(
        fixture.path().to_path_buf(),
        KdbxReadLimits::default(),
        || Err(SourceError::unavailable()),
    )
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"));

    assert_eq!(
        provider.load().expect_err("the factory failed").kind(),
        SourceErrorKind::Unavailable
    );
}

#[test]
fn the_credential_factory_runs_once_per_load() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = support::provider(&fixture, Arc::clone(&calls));

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    provider.load().expect("the first load succeeds");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    provider.load().expect("the second load succeeds");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn the_credential_factory_is_not_called_for_invalid_configuration() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = KeePassFile::new(fixture.path().to_path_buf(), KdbxReadLimits::default(), {
        let calls = Arc::clone(&calls);
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
        }
    })
    .mapping(Mapping::new(ENTRY_A, "config.token", ""));

    assert!(provider.load().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn the_supplied_read_limit_is_applied() {
    let fixture = Fixture::new();
    let provider = KeePassFile::new(
        fixture.path().to_path_buf(),
        KdbxReadLimits {
            max_input_bytes: 8,
            ..KdbxReadLimits::default()
        },
        || KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable()),
    )
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"));

    assert_eq!(
        provider
            .load()
            .expect_err("the input exceeds the limit")
            .kind(),
        SourceErrorKind::Invalid
    );
}

#[test]
fn the_provider_reports_the_vault_for_watching() {
    let fixture = Fixture::new();
    let provider = support::provider(&fixture, Arc::new(AtomicUsize::new(0)));
    assert_eq!(
        provider.watch_paths(),
        vec![fixture.path().to_path_buf()],
        "the KDBX file is the change-detection input"
    );

    let absent = fixture.directory().join("not-created-yet.kdbx");
    let provider = KeePassFile::new(absent.clone(), KdbxReadLimits::default(), || {
        KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
    })
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"));
    assert_eq!(provider.watch_paths(), vec![absent]);
}

#[test]
fn debug_and_errors_do_not_print_a_path_a_key_or_a_value() {
    let fixture = Fixture::new();
    let provider = support::provider(&fixture, Arc::new(AtomicUsize::new(0)));
    let debug = format!("{provider:?}");
    for forbidden in [
        MASTER_PASSWORD,
        ENTRY_PASSWORD_A,
        TOKEN_A,
        "config.token",
        "app.token",
        fixture.path().to_str().expect("UTF-8 temporary path"),
    ] {
        assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
    }

    let failing = KeePassFile::new(
        fixture.path().to_path_buf(),
        KdbxReadLimits::default(),
        || KdbxCredentials::password(WRONG_PASSWORD).map_err(|_| SourceError::unavailable()),
    )
    .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"));
    let source_error = failing.load().expect_err("wrong credentials");
    let source_text = format!("{source_error} {source_error:?}");
    assert!(!source_text.contains(WRONG_PASSWORD));
    assert!(!source_text.contains(TOKEN_A));

    let build_error = Builder::new()
        .source("vault", failing)
        .build()
        .expect_err("wrong credentials");
    let build_text = format!("{build_error} {build_error:?}");
    for forbidden in [
        WRONG_PASSWORD,
        TOKEN_A,
        ENTRY_PASSWORD_A,
        fixture.path().to_str().expect("UTF-8 temporary path"),
    ] {
        assert!(!build_text.contains(forbidden), "error leaked {forbidden}");
    }
    assert!(build_error.is_source_failure());
    assert_eq!(build_error.source_kind(), Some(SourceErrorKind::Invalid));
}
