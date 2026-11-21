//! The reload holder, manual refresh, subscriptions, and the opt-in polling watcher.

#![cfg(feature = "reload")]

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
        mpsc::{RecvTimeoutError, TryRecvError},
    },
    thread,
    time::Duration,
};

use neotales_config::{Builder, RefreshOutcome, Source, SourceError, Value, WatchError};

/// Every blocking test wait is bounded by this; a correct implementation never reaches it.
const TIMEOUT: Duration = Duration::from_secs(10);

fn value(number: i64) -> Value {
    Value::map([("value", Value::from(number))])
}

fn counter_source(counter: &Arc<AtomicI64>) -> impl Source + 'static {
    let counter = Arc::clone(counter);
    move || Ok::<_, SourceError>(value(counter.load(Ordering::SeqCst)))
}

#[test]
fn manual_refresh_publishes_atomically_and_old_configs_stay_readable() {
    let counter = Arc::new(AtomicI64::new(1));
    let reload = Builder::new()
        .source("counter", counter_source(&counter))
        .build_reloadable()
        .expect("the fixture builds");
    let before = reload.config();
    assert_eq!(before.get::<i64>("value").unwrap(), 1);
    assert_eq!(reload.generation(), 0);

    counter.store(2, Ordering::SeqCst);
    let outcome = reload.refresh();
    assert_eq!(outcome, RefreshOutcome::Reloaded);
    assert_eq!(reload.generation(), 1);

    // The old snapshot is immutable and independent, not a view that moved with the holder.
    assert_eq!(before.get::<i64>("value").unwrap(), 1);
    assert_eq!(before.origin("value").unwrap(), "counter");
    let after = reload.config();
    assert_eq!(after.get::<i64>("value").unwrap(), 2);
    assert_eq!(after.origin("value").unwrap(), "counter");
}

#[test]
fn a_provenance_change_alone_publishes_a_new_snapshot() {
    let present = Arc::new(AtomicI64::new(0));
    let overlay = Arc::clone(&present);
    let reload = Builder::new()
        .memory("defaults", Value::map([("port", Value::from(8080_i64))]))
        .source("overlay", move || {
            if overlay.load(Ordering::SeqCst) == 0 {
                Ok::<_, SourceError>(Value::map(std::iter::empty::<(&str, Value)>()))
            } else {
                Ok(Value::map([("port", Value::from(8080_i64))]))
            }
        })
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(reload.config().origin("port").unwrap(), "defaults");
    assert_eq!(reload.generation(), 0);

    present.store(1, Ordering::SeqCst);
    assert_eq!(reload.refresh(), RefreshOutcome::Reloaded);
    // The value is unchanged; the provenance is not, so the snapshot is different.
    assert_eq!(reload.config().get::<i64>("port").unwrap(), 8080);
    assert_eq!(reload.config().origin("port").unwrap(), "overlay");
    assert_eq!(reload.generation(), 1);

    present.store(0, Ordering::SeqCst);
    assert_eq!(reload.refresh(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().origin("port").unwrap(), "defaults");
}

#[test]
fn an_identical_reload_is_unchanged_and_notifies_nobody() {
    let counter = Arc::new(AtomicI64::new(7));
    let reload = Builder::new()
        .source("counter", counter_source(&counter))
        .build_reloadable()
        .expect("the fixture builds");
    let subscription = reload.subscribe();

    assert_eq!(reload.refresh(), RefreshOutcome::Unchanged);
    assert_eq!(reload.generation(), 0);
    assert_eq!(subscription.try_recv().unwrap_err(), TryRecvError::Empty);
    assert_eq!(
        subscription
            .recv_timeout(Duration::from_millis(100))
            .unwrap_err(),
        RecvTimeoutError::Timeout
    );

    counter.store(8, Ordering::SeqCst);
    assert_eq!(reload.refresh(), RefreshOutcome::Reloaded);
    assert_eq!(
        subscription
            .recv_timeout(TIMEOUT)
            .expect("the change is delivered")
            .get::<i64>("value")
            .unwrap(),
        8
    );
    assert_eq!(subscription.try_recv().unwrap_err(), TryRecvError::Empty);
}

#[test]
fn subscribers_are_independent_and_coalesce_to_the_newest_snapshot() {
    let counter = Arc::new(AtomicI64::new(0));
    let reload = Builder::new()
        .source("counter", counter_source(&counter))
        .build_reloadable()
        .expect("the fixture builds");
    let first = reload.subscribe();
    let second = reload.subscribe();

    counter.store(1, Ordering::SeqCst);
    assert!(reload.refresh().is_reloaded());
    assert_eq!(
        first
            .recv_timeout(TIMEOUT)
            .expect("first receives")
            .get::<i64>("value")
            .unwrap(),
        1
    );
    assert_eq!(
        second
            .recv_timeout(TIMEOUT)
            .expect("second receives independently")
            .get::<i64>("value")
            .unwrap(),
        1
    );

    // Two changes while neither subscriber reads: each sees the newest one once.
    counter.store(2, Ordering::SeqCst);
    assert!(reload.refresh().is_reloaded());
    counter.store(3, Ordering::SeqCst);
    assert!(reload.refresh().is_reloaded());

    assert_eq!(
        first
            .try_recv()
            .expect("the coalesced change is pending")
            .get::<i64>("value")
            .unwrap(),
        3
    );
    assert_eq!(
        second
            .try_recv()
            .expect("the other subscriber still has its own signal")
            .get::<i64>("value")
            .unwrap(),
        3
    );
    assert_eq!(first.try_recv().unwrap_err(), TryRecvError::Empty);
    assert_eq!(second.try_recv().unwrap_err(), TryRecvError::Empty);

    // Dropping one subscriber does not disturb the other.
    drop(first);
    counter.store(4, Ordering::SeqCst);
    assert!(reload.refresh().is_reloaded());
    assert_eq!(
        second
            .recv_timeout(TIMEOUT)
            .expect("the remaining subscriber receives")
            .get::<i64>("value")
            .unwrap(),
        4
    );
}

#[test]
fn a_dropped_holder_disconnects_its_subscribers() {
    let reload = Builder::new()
        .memory("defaults", value(1))
        .build_reloadable()
        .expect("the fixture builds");
    let subscription = reload.subscribe();
    drop(reload);
    assert_eq!(
        subscription.try_recv().unwrap_err(),
        TryRecvError::Disconnected
    );
    assert_eq!(
        subscription.recv_timeout(TIMEOUT).unwrap_err(),
        RecvTimeoutError::Disconnected
    );
}

#[test]
fn refresh_if_changed_has_nothing_to_observe_without_watched_files() {
    let loads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&loads);
    let reload = Builder::new()
        .source("counter", move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok::<_, SourceError>(value(1))
        })
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(reload.watched_path_count(), 0);
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "an unobservable source is not reloaded by a poll"
    );
}

#[test]
fn a_large_watched_file_is_fingerprinted_to_its_last_byte() {
    const SIZE: usize = 2 * 1024 * 1024;

    struct TailSource {
        path: PathBuf,
    }

    impl Source for TailSource {
        fn load(&self) -> Result<Value, SourceError> {
            let bytes = fs::read(&self.path).map_err(|_| SourceError::unavailable())?;
            let tail = bytes.last().copied().ok_or_else(SourceError::invalid)?;
            Ok(Value::map([("tail", Value::from(i64::from(tail)))]))
        }

        fn watch_paths(&self) -> Vec<PathBuf> {
            vec![self.path.clone()]
        }
    }

    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("large.bin");
    let mut first = vec![b'a'; SIZE];
    first[SIZE - 1] = b'A';
    fs::write(&path, &first).expect("the fixture is writable");

    let reload = Builder::new()
        .source("tail", TailSource { path: path.clone() })
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(reload.config().get::<i64>("tail").unwrap(), i64::from(b'A'));
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);

    // Same size, different final byte: detection proves the fingerprint read past its first
    // chunk rather than only a prefix of the file.
    let mut second = first.clone();
    second[SIZE - 1] = b'B';
    fs::write(&path, &second).expect("the replacement is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().get::<i64>("tail").unwrap(), i64::from(b'B'));

    // Truncation and growth are changes too.
    fs::write(&path, &second[..SIZE / 2]).expect("the truncation is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().get::<i64>("tail").unwrap(), i64::from(b'a'));

    let mut third = second;
    third.push(b'C');
    fs::write(&path, &third).expect("the growth is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().get::<i64>("tail").unwrap(), i64::from(b'C'));
}

#[test]
fn the_watcher_detects_a_tail_change_in_a_large_file() {
    const SIZE: usize = 1024 * 1024;

    struct TailSource {
        path: PathBuf,
    }

    impl Source for TailSource {
        fn load(&self) -> Result<Value, SourceError> {
            let bytes = fs::read(&self.path).map_err(|_| SourceError::unavailable())?;
            let tail = bytes.last().copied().ok_or_else(SourceError::invalid)?;
            Ok(Value::map([("tail", Value::from(i64::from(tail)))]))
        }

        fn watch_paths(&self) -> Vec<PathBuf> {
            vec![self.path.clone()]
        }
    }

    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("large.bin");
    let mut first = vec![b'a'; SIZE];
    first[SIZE - 1] = b'A';
    fs::write(&path, &first).expect("the fixture is writable");

    let reload = Builder::new()
        .source("tail", TailSource { path: path.clone() })
        .build_reloadable()
        .expect("the fixture builds");
    let subscription = reload.subscribe();
    let watcher = reload
        .start_watching(Duration::from_millis(10))
        .expect("a file source is watchable");

    let mut second = first;
    second[SIZE - 1] = b'Z';
    fs::write(&path, &second).expect("the replacement is writable");
    let config = subscription
        .recv_timeout(TIMEOUT)
        .expect("the watcher publishes the large replacement");
    assert_eq!(config.get::<i64>("tail").unwrap(), i64::from(b'Z'));
    watcher.stop();
}

#[test]
fn concurrent_refreshes_are_serialized() {
    let in_load = Arc::new(AtomicBool::new(false));
    let loads = Arc::new(AtomicUsize::new(0));
    let guard = Arc::clone(&in_load);
    let counted = Arc::clone(&loads);
    let reload = Builder::new()
        .source("guarded", move || {
            if guard.swap(true, Ordering::SeqCst) {
                return Err(SourceError::invalid());
            }
            counted.fetch_add(1, Ordering::SeqCst);
            // Widen the window a non-serialized implementation would expose.
            thread::yield_now();
            guard.store(false, Ordering::SeqCst);
            Ok::<_, SourceError>(value(1))
        })
        .build_reloadable()
        .expect("the fixture builds");

    let mut handles = Vec::new();
    for _ in 0..8 {
        let reload = reload.clone();
        handles.push(thread::spawn(move || reload.refresh()));
    }
    for handle in handles {
        assert_eq!(
            handle.join().expect("the refreshing thread does not panic"),
            RefreshOutcome::Unchanged
        );
    }
    assert_eq!(
        loads.load(Ordering::SeqCst),
        9,
        "one initial load plus one load per serialized refresh"
    );
    assert_eq!(reload.generation(), 0);
}

#[test]
fn the_watch_baseline_is_taken_before_the_initial_load() {
    /// Writes new contents during its first load, after the baseline has to have been captured.
    struct WritingSource {
        path: PathBuf,
        loads: Arc<AtomicUsize>,
    }

    impl Source for WritingSource {
        fn load(&self) -> Result<Value, SourceError> {
            let load = self.loads.fetch_add(1, Ordering::SeqCst);
            if load == 0 {
                fs::write(&self.path, "port = 9090\n").map_err(|_| SourceError::unavailable())?;
            }
            let text = fs::read_to_string(&self.path).map_err(|_| SourceError::unavailable())?;
            let port: i64 = text
                .trim()
                .strip_prefix("port =")
                .ok_or_else(SourceError::invalid)?
                .trim()
                .parse()
                .map_err(|_| SourceError::invalid())?;
            Ok(Value::map([("port", Value::from(port))]))
        }

        fn watch_paths(&self) -> Vec<PathBuf> {
            vec![self.path.clone()]
        }
    }

    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("baseline.txt");
    fs::write(&path, "port = 8080\n").expect("the fixture is writable");
    let loads = Arc::new(AtomicUsize::new(0));
    let reload = Builder::new()
        .source(
            "file",
            WritingSource {
                path: path.clone(),
                loads: Arc::clone(&loads),
            },
        )
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(reload.config().get::<i64>("port").unwrap(), 9090);
    assert_eq!(reload.watched_path_count(), 1);

    // The first load replaced the file. Because the baseline was captured before that load, the
    // poll has to observe a difference and load again. The merged tree is identical, so the
    // outcome is Unchanged, but the load counter proves the baseline ordering.
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "a baseline captured after the initial load would never see this change"
    );
}

#[test]
fn start_watching_refuses_a_holder_with_no_watched_file() {
    let reload = Builder::new()
        .memory("defaults", value(1))
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(
        reload
            .start_watching(Duration::from_millis(10))
            .expect_err("nothing is watchable"),
        WatchError::NoWatchableSources
    );
}

#[test]
fn reload_debug_reports_no_value() {
    const SECRET: &str = "fixture-secret-9f3a2c";
    let reload = Builder::new()
        .memory("defaults", Value::map([("token", Value::from(SECRET))]))
        .build_reloadable()
        .expect("the fixture builds");
    let subscription = reload.subscribe();
    for text in [format!("{reload:?}"), format!("{subscription:?}")] {
        assert!(!text.contains(SECRET), "{text}");
    }
}

#[cfg(feature = "toml")]
mod toml_file {
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicI64, Ordering},
        },
        time::Duration,
    };

    use neotales_config::{Builder, RefreshOutcome, SourceError, TomlFile, Value, WatchError};

    use super::TIMEOUT;

    fn fixture(text: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("app.toml");
        fs::write(&path, text).expect("the fixture is writable");
        (directory, path)
    }

    fn reload(path: &std::path::Path) -> neotales_config::Reload {
        Builder::new()
            .toml_file("app", TomlFile::new(path))
            .build_reloadable()
            .expect("the fixture builds")
    }

    #[test]
    fn a_failed_reload_keeps_value_and_provenance_then_recovers() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let reload = reload(&path);
        let before = reload.config();
        assert_eq!(before.get::<u16>("http.port").unwrap(), 8080);
        assert_eq!(before.origin("http.port").unwrap(), "app");
        assert_eq!(reload.generation(), 0);

        fs::write(&path, "[http]\nport = fixture-secret-9f3a2c\n").unwrap();
        let failed = reload.refresh();
        assert!(matches!(failed, RefreshOutcome::Failed(_)), "{failed:?}");
        let error = failed.error().expect("the failure is reported").clone();
        assert!(error.is_source_failure(), "{error}");
        assert_eq!(reload.generation(), 0, "a failure publishes nothing");
        assert!(reload.last_error().is_some());

        // The previous snapshot, value and provenance together, is still current.
        let current = reload.config();
        assert_eq!(current.get::<u16>("http.port").unwrap(), 8080);
        assert_eq!(current.origin("http.port").unwrap(), "app");
        assert_eq!(before.get::<u16>("http.port").unwrap(), 8080);

        // A pending failure is retried on the next poll even though the bytes did not change.
        assert!(reload.refresh_if_changed().is_failed());
        assert!(reload.last_error().is_some());
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8080);

        // Recovery needs only a content change; no second metadata change is required.
        fs::write(&path, "[http]\nport = 9090\n").unwrap();
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
        assert_eq!(reload.last_error(), None);
        assert_eq!(reload.generation(), 1);
        let current = reload.config();
        assert_eq!(current.get::<u16>("http.port").unwrap(), 9090);
        assert_eq!(current.origin("http.port").unwrap(), "app");
        assert_eq!(before.get::<u16>("http.port").unwrap(), 8080);
    }

    #[test]
    fn a_pending_failure_from_another_source_retries_without_a_file_change() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let file_before = fs::read(&path).unwrap();
        let available = Arc::new(AtomicBool::new(true));
        let counter = Arc::new(AtomicI64::new(10));
        let source_available = Arc::clone(&available);
        let source_counter = Arc::clone(&counter);
        let reload = Builder::new()
            .toml_file("app", TomlFile::new(&path))
            .source("flaky", move || {
                if !source_available.load(Ordering::SeqCst) {
                    return Err(SourceError::unavailable());
                }
                Ok(Value::map([(
                    "flaky",
                    Value::from(source_counter.load(Ordering::SeqCst)),
                )]))
            })
            .build_reloadable()
            .expect("the fixture builds");
        assert_eq!(reload.config().get::<i64>("flaky").unwrap(), 10);
        assert_eq!(reload.watched_path_count(), 1);

        // A manual refresh makes the other source's transient failure pending.
        available.store(false, Ordering::SeqCst);
        assert!(reload.refresh().is_failed());
        assert!(reload.last_error().is_some());
        // The last good value and its provenance remain published.
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8080);
        assert_eq!(reload.config().get::<i64>("flaky").unwrap(), 10);
        assert_eq!(reload.generation(), 0);

        // The watched bytes and metadata are untouched, yet the pending failure is retried
        // instead of being reported as an unchanged poll.
        assert_eq!(fs::read(&path).unwrap(), file_before);
        assert!(reload.refresh_if_changed().is_failed());

        // Recovery needs no second file write: the other source becomes available again.
        counter.store(20, Ordering::SeqCst);
        available.store(true, Ordering::SeqCst);
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
        assert_eq!(reload.last_error(), None);
        assert_eq!(reload.config().get::<i64>("flaky").unwrap(), 20);
        assert_eq!(fs::read(&path).unwrap(), file_before);
    }

    #[test]
    fn a_watcher_retries_a_failed_source_without_a_file_change() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let available = Arc::new(AtomicBool::new(true));
        let counter = Arc::new(AtomicI64::new(10));
        let source_available = Arc::clone(&available);
        let source_counter = Arc::clone(&counter);
        let reload = Builder::new()
            .toml_file("app", TomlFile::new(&path))
            .source("flaky", move || {
                if !source_available.load(Ordering::SeqCst) {
                    return Err(SourceError::unavailable());
                }
                Ok(Value::map([(
                    "flaky",
                    Value::from(source_counter.load(Ordering::SeqCst)),
                )]))
            })
            .build_reloadable()
            .expect("the fixture builds");

        // Fail once explicitly so the reload is pending before the watcher starts.
        available.store(false, Ordering::SeqCst);
        assert!(reload.refresh().is_failed());
        assert!(reload.last_error().is_some());

        let watcher = reload
            .start_watching(Duration::from_millis(10))
            .expect("a file source is watchable");

        // No file write: the poller retries the pending failure and recovers from the source.
        counter.store(20, Ordering::SeqCst);
        available.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + TIMEOUT;
        while reload.config().get::<i64>("flaky").unwrap() != 20 {
            assert!(
                std::time::Instant::now() < deadline,
                "the watcher did not retry the pending failure"
            );
            std::thread::yield_now();
        }
        watcher.stop();
        assert_eq!(reload.last_error(), None);
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8080);
    }

    #[test]
    fn a_deleted_file_fails_and_the_same_contents_restore_cleanly() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let reload = reload(&path);

        fs::remove_file(&path).unwrap();
        let failed = reload.refresh_if_changed();
        assert!(matches!(failed, RefreshOutcome::Failed(_)), "{failed:?}");
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8080);
        assert!(reload.last_error().is_some());

        // Recreating the original bytes is a polled change (missing to present), but the merged
        // tree matches the still-published one. It recovers without publishing or notifying.
        fs::write(&path, "[http]\nport = 8080\n").unwrap();
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
        assert_eq!(reload.last_error(), None);
        assert_eq!(reload.generation(), 0);

        // A later content change publishes normally.
        fs::write(&path, "[http]\nport = 8081\n").unwrap();
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8081);
    }

    #[test]
    fn an_atomic_replacement_with_the_same_size_is_detected() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let reload = reload(&path);

        let invalid = path.with_file_name("invalid.toml");
        fs::write(&invalid, "[http]\nport = 8x80\n").unwrap();
        fs::rename(&invalid, &path).unwrap();
        assert!(reload.refresh_if_changed().is_failed());

        let replacement = path.with_file_name("replacement.toml");
        fs::write(&replacement, "[http]\nport = 8081\n").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8081);
    }

    #[test]
    fn the_explicit_watcher_publishes_a_replaced_file_and_stops() {
        let (directory, path) = fixture("[http]\nport = 8080\n");
        let reload = reload(&path);
        let subscription = reload.subscribe();
        let watcher = reload
            .start_watching(Duration::from_millis(10))
            .expect("a file source is watchable");
        assert!(watcher.is_running());

        let replacement = directory.path().join("next.toml");
        fs::write(&replacement, "[http]\nport = 9090\n").unwrap();
        fs::rename(&replacement, &path).unwrap();

        let config = subscription
            .recv_timeout(TIMEOUT)
            .expect("the watcher publishes the replacement");
        assert_eq!(config.get::<u16>("http.port").unwrap(), 9090);
        assert_eq!(config.origin("http.port").unwrap(), "app");

        watcher.stop();
        // The holder remains usable for explicit refresh after the watcher is gone.
        fs::write(&path, "[http]\nport = 9091\n").unwrap();
        assert_eq!(reload.refresh(), RefreshOutcome::Reloaded);
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 9091);
    }

    #[test]
    fn a_watcher_records_a_failed_reload_and_keeps_polling() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let reload = reload(&path);
        let watcher = reload
            .start_watching(Duration::from_millis(10))
            .expect("a file source is watchable");

        fs::write(&path, "[http]\nport = broken\n").unwrap();
        let deadline = std::time::Instant::now() + TIMEOUT;
        while reload.last_error().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the watcher did not record the failure"
            );
            std::thread::yield_now();
        }
        assert_eq!(reload.config().get::<u16>("http.port").unwrap(), 8080);

        fs::write(&path, "[http]\nport = 9090\n").unwrap();
        let deadline = std::time::Instant::now() + TIMEOUT;
        while reload.config().get::<u16>("http.port").unwrap() != 9090 {
            assert!(
                std::time::Instant::now() < deadline,
                "the watcher did not recover"
            );
            std::thread::yield_now();
        }
        watcher.stop();
        assert_eq!(reload.last_error(), None);
    }

    #[test]
    fn a_zero_interval_is_refused() {
        let (_directory, path) = fixture("[http]\nport = 8080\n");
        let reload = reload(&path);
        assert_eq!(
            reload
                .start_watching(Duration::ZERO)
                .expect_err("a zero interval would spin"),
            WatchError::ZeroInterval
        );
    }
}

#[cfg(all(
    feature = "dotenv",
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]
#[test]
fn every_file_provider_reports_a_watched_path() {
    use std::path::Path;

    use neotales_config::{
        DotenvFile, IniFile, Json5File, JsonFile, JsoncFile, TomlFile, YamlFile,
    };

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/formats");
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dotenv = directory.path().join(".env");
    fs::write(&dotenv, "APP_port=8080\n").unwrap();

    let reload = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(&dotenv))
        .json_file("json", JsonFile::new(fixtures.join("defaults.json")))
        .json5_file("json5", Json5File::new(fixtures.join("defaults.json5")))
        .jsonc_file("jsonc", JsoncFile::new(fixtures.join("defaults.jsonc")))
        .toml_file("toml", TomlFile::new(fixtures.join("defaults.toml")))
        .yaml_file("yaml", YamlFile::new(fixtures.join("defaults.yaml")))
        .ini_file("ini", IniFile::new(fixtures.join("defaults.ini")))
        .build_reloadable()
        .expect("every fixture parses");
    assert_eq!(reload.watched_path_count(), 7);
}

#[cfg(feature = "dotenv")]
#[test]
fn a_dotenv_file_reports_its_path_and_reloads() {
    use neotales_config::DotenvFile;

    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join(".env");
    fs::write(&path, "APP_http.port=8080\n").unwrap();
    let reload = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(&path).prefix("APP_"))
        .build_reloadable()
        .expect("the fixture builds");
    assert_eq!(reload.watched_path_count(), 1);
    assert_eq!(reload.config().get::<String>("http.port").unwrap(), "8080");

    fs::write(&path, "APP_http.port=9090\n").unwrap();
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(reload.config().get::<String>("http.port").unwrap(), "9090");
}
