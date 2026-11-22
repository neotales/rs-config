//! Optional file appearance/disappearance and per-source change-watch behavior under `reload`.
//!
//! The provider-level flags are covered for every format in `tests/optional.rs`. These tests use
//! TOML to exercise the holder, the polling primitive, and the watcher, because the reload
//! machinery is format-independent.

#![cfg(all(feature = "reload", feature = "toml"))]

use std::{fs, path::PathBuf, time::Duration};

use neotales_config::{Builder, RefreshOutcome, Reload, TomlFile, Value, WatchError};

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

    // Changing only the un-watched file is invisible to a poll.
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
