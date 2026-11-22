//! Optional file appearance/disappearance and per-source change-watch behavior under `reload`.
//!
//! The provider-level flags are covered for every format in `tests/optional.rs`. These tests use
//! TOML to exercise the holder, the polling primitive, and the watcher, because the reload
//! machinery is format-independent.

#![cfg(all(feature = "reload", feature = "toml"))]

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use neotales_config::{
    Builder, RefreshOutcome, Reload, Source, SourceError, SourceErrorKind, TomlFile, Value,
    WatchError,
};

fn fixture(text: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("app.toml");
    fs::write(&path, text).expect("the fixture is writable");
    (directory, path)
}

fn reload(path: &std::path::Path) -> Reload {
    Builder::new()
        .toml_file("app", TomlFile::new(path).optional(true))
        .build_reloadable()
        .expect("the optional fixture builds")
}

/// Reports a watched path without reading it, so a load can succeed while that path is unreadable.
struct WatchingConstant {
    path: PathBuf,
}

impl Source for WatchingConstant {
    fn load(&self) -> Result<Value, SourceError> {
        Ok(Value::map([("constant", Value::from(1_i64))]))
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        vec![self.path.clone()]
    }
}

#[test]
fn an_absent_optional_file_appears_on_the_next_poll() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("app.toml");

    let reload = reload(&path);
    assert_eq!(
        reload.watched_path_count(),
        1,
        "optional does not change whether the path is watched"
    );
    assert!(!reload.config().has("probe").unwrap());
    // The baseline and the observed state are both "missing", so nothing is reloaded yet.
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);

    fs::write(&path, "probe = 8080\n").expect("the fixture is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    let config = reload.config();
    assert_eq!(config.get::<i64>("probe").unwrap(), 8080);
    assert_eq!(config.origin("probe").unwrap(), "app");
}

#[test]
fn a_disappearing_optional_file_drops_its_values_and_restores_the_lower_source() {
    let (_directory, path) = fixture("probe = 2\n");
    let reload = Builder::new()
        .memory("defaults", Value::map([("probe", Value::from(1_i64))]))
        .toml_file("app", TomlFile::new(&path).optional(true))
        .build_reloadable()
        .expect("the fixture builds");

    let config = reload.config();
    assert_eq!(config.get::<i64>("probe").unwrap(), 2);
    assert_eq!(config.origin("probe").unwrap(), "app");

    fs::remove_file(&path).expect("the fixture is removable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);

    // Value and provenance come from the same published snapshot: the optional source's value is
    // gone and the earlier source's value and origin show through together.
    let config = reload.config();
    assert_eq!(config.get::<i64>("probe").unwrap(), 1);
    assert_eq!(config.origin("probe").unwrap(), "defaults");
}

#[test]
fn an_optional_parse_failure_keeps_the_previous_snapshot_then_an_absence_removes_it() {
    let (_directory, path) = fixture("probe = 2\n");
    let reload = reload(&path);

    fs::write(&path, "probe = [\n").expect("the replacement is writable");
    let failed = reload.refresh();
    assert!(failed.is_failed(), "{failed:?}");
    assert!(reload.last_error().is_some());
    let config = reload.config();
    assert_eq!(config.get::<i64>("probe").unwrap(), 2);
    assert_eq!(config.origin("probe").unwrap(), "app");

    // Disappearance is an absence, not a parse failure, so the next load succeeds and the value is
    // removed rather than preserved.
    fs::remove_file(&path).expect("the fixture is removable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(reload.last_error(), None);
    assert!(!reload.config().has("probe").unwrap());
}

#[test]
fn change_watching_off_is_not_polled_but_manual_refresh_still_loads() {
    let (_directory, path) = fixture("probe = 8080\n");
    let reload = Builder::new()
        .toml_file("app", TomlFile::new(&path).reload_on_change(false))
        .build_reloadable()
        .expect("the fixture builds");

    assert_eq!(reload.watched_path_count(), 0);
    assert_eq!(
        reload
            .start_watching(Duration::from_millis(10))
            .expect_err("no source reports a path"),
        WatchError::NoWatchableSources
    );

    fs::write(&path, "probe = 9090\n").expect("the replacement is writable");
    // With no reported path there is nothing to observe, so a poll does not load.
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(reload.config().get::<i64>("probe").unwrap(), 8080);

    // A manual refresh loads every source, including the one whose change watch is off.
    assert_eq!(reload.refresh(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().get::<i64>("probe").unwrap(), 9090);
}

#[test]
fn an_absent_optional_file_replaced_by_a_directory_fails_and_recovers() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("app.toml");

    let reload = reload(&path);
    assert_eq!(
        reload.refresh_if_changed(),
        RefreshOutcome::Unchanged,
        "an absent optional file at the baseline is a stable observation"
    );
    assert!(!reload.config().has("probe").unwrap());

    // A directory at the watched path is not an absence. The optional flag accepts only a real
    // not-found, so a poll must load and report the failure instead of comparing the path with the
    // absent baseline and reporting Unchanged.
    fs::create_dir(&path).expect("the directory fixture is creatable");
    let outcome = reload.refresh_if_changed();
    assert!(outcome.is_failed(), "{outcome:?}");
    assert_eq!(
        outcome
            .error()
            .and_then(neotales_config::Error::source_kind),
        Some(SourceErrorKind::Unavailable)
    );
    assert!(!reload.config().has("probe").unwrap());

    // The failure is retried on every poll even though the watched fingerprint cannot change, so a
    // persistently unreadable path can never be mistaken for an unchanged one. Retry is bounded by
    // the caller's interval, not a loop inside a call.
    assert!(reload.refresh_if_changed().is_failed());
    assert!(reload.refresh_if_changed().is_failed());

    // The diagnostic stays sanitized: no path and no document text.
    let reported = format!("{:?}", reload.last_error());
    assert!(!reported.contains("app.toml"), "{reported}");
    assert!(
        !reported.contains(&*directory.path().to_string_lossy()),
        "{reported}"
    );
    assert!(!reported.contains("probe"), "{reported}");

    // Recovery: the directory is replaced by a valid document and the next poll publishes it.
    fs::remove_dir(&path).expect("the directory fixture is removable");
    fs::write(&path, "probe = 7\n").expect("the replacement is writable");
    let outcome = reload.refresh_if_changed();
    assert!(outcome.is_reloaded(), "{outcome:?}");
    assert_eq!(reload.last_error(), None);
    let config = reload.config();
    assert_eq!(config.get::<i64>("probe").unwrap(), 7);
    assert_eq!(config.origin("probe").unwrap(), "app");
}

#[test]
fn an_unreadable_path_that_is_not_an_absence_forces_a_load_until_it_recovers() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let parent = directory.path().join("conf");
    fs::create_dir(&parent).expect("the parent directory is creatable");
    let path = parent.join("app.toml");

    let reload = reload(&path);
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);

    // Replacing the parent with a regular file makes every path below it fail with a
    // non-not-found error. That is not an absence the optional flag accepts, so the poll must
    // attempt the load and report it.
    fs::remove_dir(&parent).expect("the empty parent is removable");
    fs::write(&parent, "not a directory").expect("the replacement is writable");
    assert!(reload.refresh_if_changed().is_failed());
    assert!(!reload.config().has("probe").unwrap());

    // A real directory with a valid document is a recovery, and the value is published.
    fs::remove_file(&parent).expect("the replacement file is removable");
    fs::create_dir(&parent).expect("the parent directory is creatable again");
    fs::write(&path, "probe = 3\n").expect("the document is writable");
    let outcome = reload.refresh_if_changed();
    assert!(outcome.is_reloaded(), "{outcome:?}");
    assert_eq!(reload.last_error(), None);
    assert_eq!(reload.config().get::<i64>("probe").unwrap(), 3);
}

#[test]
fn an_unreadable_path_forces_a_load_that_a_custom_source_can_still_satisfy() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("app.toml");
    fs::create_dir(&path).expect("the directory fixture is creatable");

    // The source reports the watched path without reading it: the fingerprint decides whether a
    // load is attempted, not whether the sources can satisfy it.
    let reload = Builder::new()
        .source("constant", WatchingConstant { path: path.clone() })
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(reload.watched_path_count(), 1);

    // The unreadable path forces a load, but the load succeeds and produces the published
    // snapshot, so the outcome is Unchanged with no error. It is not a forced failure.
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(reload.last_error(), None);

    // Recovery forces another load but does not guarantee a publication: this source's value does
    // not depend on the path, so the snapshot is still equal.
    fs::remove_dir(&path).expect("the directory fixture is removable");
    fs::write(&path, "probe = 3\n").expect("the replacement is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(reload.generation(), 0);
    let config = reload.config();
    assert_eq!(config.get::<i64>("constant").unwrap(), 1);
}

#[test]
fn a_change_in_one_watched_path_reloads_every_source() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let first = directory.path().join("first.toml");
    let second = directory.path().join("second.toml");
    fs::write(&first, "first = 1\n").expect("the fixture is writable");
    fs::write(&second, "second = 1\n").expect("the fixture is writable");

    // A caller source with no watched path of its own. It proves that a poll triggered by one
    // watched file loads every source, not only the one that reported the change.
    let loads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&loads);
    let reload = Builder::new()
        .source("counting", move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, SourceError>(Value::map([("counted", Value::from(1_i64))]))
        })
        .toml_file("first", TomlFile::new(&first))
        .toml_file("second", TomlFile::new(&second))
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(loads.load(Ordering::SeqCst), 1, "the initial load");
    assert_eq!(
        reload.watched_path_count(),
        2,
        "two distinct paths are watched"
    );

    // Only the first file changes, so only the first path can trigger the poll. The snapshot still
    // has to be rebuilt from every source.
    fs::write(&first, "first = 2\n").expect("the replacement is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "a poll for one watched path must load every source"
    );

    let config = reload.config();
    assert_eq!(config.get::<i64>("first").unwrap(), 2);
    assert_eq!(config.get::<i64>("second").unwrap(), 1);
    assert_eq!(config.origin("second").unwrap(), "second");
    assert_eq!(config.get::<i64>("counted").unwrap(), 1);
}

#[test]
fn a_path_shared_by_two_sources_stays_watched_when_one_turns_it_off() {
    let (_directory, path) = fixture("probe = 1\n");

    // Both sources read the same file. Disabling change watching on one removes only that source's
    // contribution to the watched list, so the path is still watched through the other.
    let reload = Builder::new()
        .toml_file("primary", TomlFile::new(&path))
        .toml_file("secondary", TomlFile::new(&path).reload_on_change(false))
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(
        reload.watched_path_count(),
        1,
        "a path reported twice is watched once"
    );

    fs::write(&path, "probe = 2\n").expect("the replacement is writable");
    assert_eq!(
        reload.refresh_if_changed(),
        RefreshOutcome::Reloaded,
        "the shared path is still observed through the enabled source"
    );
    let config = reload.config();
    assert_eq!(config.get::<i64>("probe").unwrap(), 2);
    assert_eq!(
        config.origin("probe").unwrap(),
        "secondary",
        "the source with watching off is still loaded by the poll"
    );

    // Only when every reporter turns it off does the path leave the watched list.
    let disabled = Builder::new()
        .toml_file("primary", TomlFile::new(&path).reload_on_change(false))
        .toml_file("secondary", TomlFile::new(&path).reload_on_change(false))
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(disabled.watched_path_count(), 0);

    fs::write(&path, "probe = 3\n").expect("the replacement is writable");
    assert_eq!(disabled.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(disabled.config().get::<i64>("probe").unwrap(), 2);
    assert_eq!(disabled.refresh(), RefreshOutcome::Reloaded);
    assert_eq!(disabled.config().get::<i64>("probe").unwrap(), 3);
}

#[test]
fn a_poll_observes_only_enabled_files_and_a_manual_refresh_observes_all() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let watched = directory.path().join("watched.toml");
    let quiet = directory.path().join("quiet.toml");
    fs::write(&watched, "watched = 8080\n").expect("the fixture is writable");
    fs::write(&quiet, "quiet = 9090\n").expect("the fixture is writable");

    let reload = Builder::new()
        .toml_file("watched", TomlFile::new(&watched))
        .toml_file("quiet", TomlFile::new(&quiet).reload_on_change(false))
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(reload.watched_path_count(), 1);

    // Changing only the un-watched file does not trigger a poll.
    fs::write(&quiet, "quiet = 9191\n").expect("the replacement is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(reload.config().get::<i64>("quiet").unwrap(), 9090);

    // A watched change starts a load, and that load reads every source, including the un-watched
    // one: both new values appear in the same published snapshot.
    fs::write(&watched, "watched = 8181\n").expect("the replacement is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    let config = reload.config();
    assert_eq!(config.get::<i64>("watched").unwrap(), 8181);
    assert_eq!(config.get::<i64>("quiet").unwrap(), 9191);
    assert_eq!(config.origin("quiet").unwrap(), "quiet");

    // The manual path reloads as well, with no watched change at all.
    fs::write(&quiet, "quiet = 9292\n").expect("the replacement is writable");
    assert_eq!(reload.refresh(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().get::<i64>("quiet").unwrap(), 9292);
}
