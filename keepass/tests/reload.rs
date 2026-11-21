//! Reload, watcher, and failure-preservation behavior for a KDBX source.

#![cfg(feature = "reload")]
#![allow(missing_docs)]

mod support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use neotales_config::{Builder, RefreshOutcome, SourceError, SourceErrorKind};
use neotales_config_keepass::{KeePassFile, Mapping};
use neotales_keepass::{KdbxCredentials, KdbxReadLimits};
use support::{ENTRY_A, Fixture, MASTER_PASSWORD, TOKEN_A, TOKEN_B, TOKEN_CHANGED, TOKEN_RESTORED};

const TIMEOUT: Duration = Duration::from_secs(15);

#[test]
fn a_refresh_sees_a_replaced_vault_and_calls_the_factory_again() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let reload = Builder::new()
        .source("vault", support::provider(&fixture, Arc::clone(&calls)))
        .build_reloadable()
        .expect("the initial load succeeds");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(reload.watched_path_count(), 1);
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_A
    );

    // A manual refresh reloads every source even when nothing changed.
    assert!(reload.refresh().is_unchanged());
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    fixture.write(TOKEN_CHANGED, TOKEN_B);
    let outcome = reload.refresh_if_changed();
    assert!(outcome.is_reloaded(), "{outcome:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_CHANGED
    );
}

#[test]
fn a_credential_only_change_needs_a_manual_refresh() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let invalid = Arc::new(AtomicBool::new(false));

    let provider = {
        let calls = Arc::clone(&calls);
        let invalid = Arc::clone(&invalid);
        KeePassFile::new(
            fixture.path().to_path_buf(),
            KdbxReadLimits::default(),
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                if invalid.load(Ordering::SeqCst) {
                    return Err(SourceError::unavailable());
                }
                KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
            },
        )
        .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"))
    };

    let reload = Builder::new()
        .source("vault", provider)
        .build_reloadable()
        .expect("the initial load succeeds");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Only the credential source changed. The watched KDBX file did not, so a poll must not load
    // and must not call the factory.
    invalid.store(true, Ordering::SeqCst);
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a poll over unchanged bytes must not call the credential factory"
    );
    assert!(reload.last_error().is_none());
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_A
    );

    // A manual refresh is the documented path after a credential rotation. The bad credentials
    // reject the load; the previous snapshot stays published.
    assert!(reload.refresh().is_failed());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_A
    );

    // Restoring the credential and refreshing loads again. The value is unchanged, so the
    // snapshot is not republished, but the factory ran and the stored error is cleared.
    invalid.store(false, Ordering::SeqCst);
    assert_eq!(reload.refresh(), RefreshOutcome::Unchanged);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(reload.last_error().is_none());
}

#[test]
fn the_watcher_does_not_observe_a_credential_only_change() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let invalid = Arc::new(AtomicBool::new(false));

    let provider = {
        let calls = Arc::clone(&calls);
        let invalid = Arc::clone(&invalid);
        KeePassFile::new(
            fixture.path().to_path_buf(),
            KdbxReadLimits::default(),
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                if invalid.load(Ordering::SeqCst) {
                    return Err(SourceError::unavailable());
                }
                KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
            },
        )
        .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"))
    };

    let reload = Builder::new()
        .source("vault", provider)
        .build_reloadable()
        .expect("the initial load succeeds");
    let watcher = reload
        .start_watching(Duration::from_millis(10))
        .expect("a watcher starts");

    // The watcher polls several times. Nothing in the watched file changed, so it never calls the
    // factory, even though the credential source now fails.
    invalid.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the watcher must not load for a credential-only change"
    );
    assert!(reload.last_error().is_none());
    watcher.stop();

    assert!(reload.refresh().is_failed());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_reload_keeps_the_last_good_snapshot_and_retries() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let failing = Arc::new(AtomicBool::new(false));

    let provider = {
        let calls = Arc::clone(&calls);
        let failing = Arc::clone(&failing);
        KeePassFile::new(
            fixture.path().to_path_buf(),
            KdbxReadLimits::default(),
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                if failing.load(Ordering::SeqCst) {
                    return Err(SourceError::unavailable());
                }
                KdbxCredentials::password(MASTER_PASSWORD).map_err(|_| SourceError::unavailable())
            },
        )
        .mapping(Mapping::new(ENTRY_A, "config.token", "app.token"))
    };

    let reload = Builder::new()
        .source("vault", provider)
        .build_reloadable()
        .expect("the initial load succeeds");

    failing.store(true, Ordering::SeqCst);
    let outcome = reload.refresh();
    assert!(outcome.is_failed(), "{outcome:?}");
    assert_eq!(
        outcome
            .error()
            .and_then(neotales_config::Error::source_kind),
        Some(SourceErrorKind::Unavailable)
    );
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_A,
        "the previous good snapshot stays published"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let reported = format!("{:?}", reload.last_error());
    assert!(!reported.contains(MASTER_PASSWORD));
    assert!(!reported.contains(TOKEN_A));

    // The file did not change. The pending failure is still retried, so a
    // transient credential-store failure recovers without a second write.
    failing.store(false, Ordering::SeqCst);
    assert!(reload.refresh_if_changed().is_unchanged());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(reload.last_error().is_none());
}

#[test]
fn a_corrupt_replacement_fails_and_a_later_valid_replacement_recovers() {
    let fixture = Fixture::new();
    let reload = Builder::new()
        .source(
            "vault",
            support::provider(&fixture, Arc::new(AtomicUsize::new(0))),
        )
        .build_reloadable()
        .expect("the initial load succeeds");

    fixture.corrupt();
    let outcome = reload.refresh();
    assert!(outcome.is_failed(), "{outcome:?}");
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_A,
        "a corrupt write does not replace the good snapshot"
    );

    fixture.write(TOKEN_RESTORED, TOKEN_B);
    let outcome = reload.refresh_if_changed();
    assert!(outcome.is_reloaded(), "{outcome:?}");
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_RESTORED
    );
}

#[test]
fn the_explicit_watcher_sees_a_replacement_and_survives_a_failed_one() {
    let fixture = Fixture::new();
    let reload = Builder::new()
        .source(
            "vault",
            support::provider(&fixture, Arc::new(AtomicUsize::new(0))),
        )
        .build_reloadable()
        .expect("the initial load succeeds");

    let subscription = reload.subscribe();
    let watcher = reload
        .start_watching(Duration::from_millis(25))
        .expect("a watcher starts");

    fixture.write(TOKEN_CHANGED, TOKEN_B);
    let config = subscription
        .recv_timeout(TIMEOUT)
        .expect("the watcher publishes the replacement");
    assert_eq!(
        config.get::<String>("app.token").expect("mapped"),
        TOKEN_CHANGED
    );

    // A failed replacement leaves the last good snapshot in place and does
    // not stop the polling thread.
    fixture.corrupt();
    let _ = reload.refresh();
    assert!(reload.last_error().is_some());
    assert_eq!(
        reload.config().get::<String>("app.token").expect("mapped"),
        TOKEN_CHANGED
    );
    assert!(watcher.is_running());

    fixture.write(TOKEN_RESTORED, TOKEN_B);
    let config = subscription
        .recv_timeout(TIMEOUT)
        .expect("the watcher recovers after the failed replacement");
    assert_eq!(
        config.get::<String>("app.token").expect("mapped"),
        TOKEN_RESTORED
    );

    watcher.stop();
}
