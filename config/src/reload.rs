//! A cloneable holder for the current snapshot, manual refreshes, and opt-in polling.
//!
//! The core crate builds one immutable [`Config`] and stops. This module, behind the `reload`
//! feature, keeps the resolved sources so the whole snapshot can be rebuilt:
//!
//! - [`Reload`] owns the sources, the latest immutable snapshot, the watched-file list, and the
//!   change generation. Cloning it is one `Arc` increment.
//! - [`Reload::refresh`] reloads **every** source, merges them again, and publishes the result
//!   atomically. A value tree and its provenance are one `Arc<Snapshot>`, so a reader can never
//!   observe a new value with an old origin or the reverse.
//! - [`Reload::refresh_if_changed`] does the same, but only when a watched file's contents
//!   changed since the last poll, an earlier reload failed, or a watched path cannot be
//!   fingerprinted. It is the polling primitive a caller can drive from its own event loop;
//!   [`Reload::start_watching`] is an explicit thread-based helper for callers that have no loop.
//! - [`Reload::subscribe`] returns a [`Subscription`] for change notifications. Every subscriber
//!   has its own generation, so one slow reader cannot consume or hide another reader's
//!   notification. Notifications coalesce: a subscriber that falls behind is told about the
//!   latest snapshot, not every intermediate one. A subscriber reads a generation and its
//!   snapshot as one atomic pair, so a publication that interleaves with the read can never make
//!   the same snapshot appear under two generations.
//!
//! No thread is created by the feature, by [`crate::Builder::build_reloadable`], or by any
//! refresh call. A thread exists only after an explicit [`Reload::start_watching`], and
//! [`Watcher`] stops and joins it on `stop()` or on drop.
//!
//! # Polling model and limitations
//!
//! The watcher is a polling watcher over the paths that sources report through
//! [`crate::Source::watch_paths`]. It is not an OS notification API (`inotify`, `kqueue`,
//! `ReadDirectoryChangesW`); this crate has no platform-specific dependency and works the same
//! everywhere.
//!
//! - Detection latency is up to the polling interval, plus the time the refresh itself takes.
//! - Each poll fingerprints every watched file by streaming it in fixed-size chunks. Memory is
//!   bounded by a single fixed-size chunk, not by the file size. Disk I/O is **not** bounded: the
//!   whole file is still read on every poll, so cost follows the total watched size, not the size
//!   of a change. A large file watched at a short interval is expensive. The fingerprint imposes no
//!   parser's or provider's read limit; those apply when a source loads.
//! - Only the paths a source reports are observed. The observed list is the union of what every
//!   registered source reports, so two sources may name the same path and it is polled once. A
//!   change in any watched path reloads **every** source, not only the one that reported it, which
//!   is why [`crate::Source::watch_paths`] and the providers' `reload_on_change` switch only
//!   remove one source's contribution to that list. A source whose contents also depend on
//!   something else, such as a credential store, does not report a change in that input, so a poll
//!   leaves the published snapshot alone. Call [`Reload::refresh`] after changing it.
//! - An absent path, an unreadable path, and a readable file are distinct observations. Only an
//!   actual [`std::io::ErrorKind::NotFound`] counts as absent, which is the one case an optional
//!   source accepts as missing. A path that exists but cannot be fingerprinted (a directory, a
//!   permission failure, a path whose parent is not a directory, a read that fails part way) has
//!   unknown contents: a poll attempts a load instead of comparing the fingerprint, so an
//!   unreadable path never passes for an unchanged one. The attempt is bounded by the polling
//!   interval, not a spin loop. Its result is decided by the sources and by snapshot equality, not
//!   by the fingerprint: a source that needs the path can fail, which reports the failure and
//!   keeps the previous good snapshot published, while a custom source that reports the path but
//!   does not read it can still load, and an equal snapshot returns [`RefreshOutcome::Unchanged`].
//! - A write in progress can be observed as a partial document. The refresh fails safely, the
//!   previous good snapshot stays published, [`Reload::last_error`] reports the failure, and the
//!   next poll retries even when the file's bytes did not change. Write a temporary file and
//!   rename it over the target to avoid a torn read; the retry then starts from the replacement.
//! - A failed reload is pending until a later refresh succeeds. The poller retries it on the next
//!   polling tick, whether the failure came from a watched file or from another source that was
//!   transiently unavailable. Retry is bounded by the polling interval, not a spin loop, and the
//!   last good snapshot remains published throughout.
//! - A change that produces byte-identical contents is not itself a change. The snapshot would be
//!   identical, so no refresh and no notification is produced. A pending failure is still retried.
//! - A path that is deleted is a change; a path that is recreated with new contents is another.
//!   Fingerprints use file contents rather than timestamps, so a replacement that preserves the
//!   size or the modification time is still detected.
//! - A source that reads no stable local file reports no path. Watching such a source has nothing
//!   to observe; use [`Reload::refresh`] when the caller knows its input changed. A file source
//!   with `reload_on_change(false)` contributes no path for the same reason, even though
//!   [`Reload::refresh`] still loads it and any other watched change loads it too.
//!
//! # Lifecycle and shutdown
//!
//! [`crate::Builder::build_reloadable`] performs exactly one initial load and captures the
//! watched-file baseline **before** that load, so a change made while the initial load runs is
//! caught by the next poll instead of being absorbed into the baseline.
//!
//! Every refresh is serialized by one lock. Concurrent manual refreshes, a manual refresh racing a
//! poll, and overlapping polls therefore run one at a time; the later one sees the snapshot the
//! earlier one published. Sources are loaded in registration order, and no source is merged until
//! all of them have loaded successfully.
//!
//! A [`Watcher`] owns exactly one thread. Dropping it (or calling [`Watcher::stop`]) sets the stop
//! flag, wakes the thread, and joins it, so a watcher does not outlive its handle. Starting two
//! watchers is allowed but redundant: both poll the same files and refreshes are serialized.
//! A [`Subscription`] holds only a weak reference to the holder, so it does not keep the sources
//! alive; once the last [`Reload`] and [`Watcher`] are gone, a blocked receiver reports
//! disconnection.
//!
//! # Errors
//!
//! A failed refresh returns [`RefreshOutcome::Failed`] with the same sanitized [`Error`] the
//! builder would return, and stores a copy for [`Reload::last_error`]. The error names a source
//! and a fixed reason kind. It never carries a value, a file body, parser text, or a path, and
//! its [`std::error::Error::source`] remains `None`. A watcher records the failure and keeps
//! running; it does not terminate the thread.
//!
//! # Plaintext lifetime
//!
//! A published snapshot holds the merged values in plaintext, including any secret a source
//! produced. [`Reload`] keeps only the newest snapshot, but an older snapshot stays alive as long
//! as a [`Config`] or [`RefreshOutcome`] clone references it. [`Subscription`] notifications do
//! not retain a queue of snapshots: they carry a generation, and `try_recv`/`recv_timeout` returns
//! a [`Config`] for the current snapshot when the generation advanced. A failed load's temporary
//! tree is dropped before the outcome is returned. Change detection reads file bytes to hash
//! them but stores only the digest, never the bytes. Do not log a [`Config`] or a value tree that
//! contains a secret.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//!
//! use neotales_config::{Builder, Value};
//!
//! let reload = Builder::new()
//!     .memory("defaults", Value::map([("port", Value::from(8080_i64))]))
//!     .build_reloadable()?;
//!
//! // Manual refresh, no watcher and no thread.
//! assert!(reload.refresh().is_unchanged());
//!
//! // A subscription receives a future published change, with a caller-chosen bound.
//! let subscription = reload.subscribe();
//! assert!(subscription.recv_timeout(Duration::from_millis(10)).is_err());
//! # Ok::<(), neotales_config::Error>(())
//! ```
//!
//! A file source additionally reports its path through [`crate::Source::watch_paths`], which is
//! what [`Reload::start_watching`] polls. The example in `examples/reload.rs` shows that flow with
//! a TOML file.

use std::{
    cell::Cell,
    fmt,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
        Weak,
        mpsc::{RecvError, RecvTimeoutError, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    config::{Config, Snapshot, load_roots, merge_roots},
    error::Error,
    source::ResolvedSource,
};

/// The result of one refresh attempt.
///
/// `Unchanged` means no new snapshot was published: either every source loaded and the merged
/// value tree, including its provenance, matched the currently published snapshot, or
/// [`Reload::refresh_if_changed`] found nothing to observe. `Reloaded` means a new snapshot was
/// published. `Failed` means a source could not be read, parsed, or represented; the previously
/// published snapshot is still current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshOutcome {
    /// The sources loaded, but produced the snapshot that is already published.
    Unchanged,
    /// A different snapshot was published atomically.
    Reloaded,
    /// The refresh failed and the previous snapshot remains published.
    Failed(Error),
}

impl RefreshOutcome {
    /// Whether this outcome published a new snapshot.
    #[must_use]
    pub const fn is_reloaded(&self) -> bool {
        matches!(self, Self::Reloaded)
    }

    /// Whether this outcome left the published snapshot alone because it was identical.
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }

    /// Whether the refresh failed.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// The sanitized error, when the refresh failed.
    #[must_use]
    pub const fn error(&self) -> Option<&Error> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Unchanged | Self::Reloaded => None,
        }
    }
}

/// A cloneable holder for the current snapshot and the sources that produce it.
///
/// Built by [`crate::Builder::build_reloadable`]. Cloning is one `Arc` reference-count increment;
/// clones share the sources, the published snapshot, the subscribers, and the watch baseline.
///
/// [`fmt::Debug`] reports the number of sources, the number of watched paths, and the change
/// generation. It never prints a path, a value, or a document.
pub struct Reload {
    inner: Arc<Inner>,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the inner holder owns sources and a snapshot; only counts are reportable"
)]
impl fmt::Debug for Reload {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Reload")
            .field("sources", &self.inner.sources.len())
            .field("watched_paths", &self.inner.watch_paths.len())
            .field("generation", &self.inner.generation())
            .finish()
    }
}

impl Clone for Reload {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Reload {
    /// Creates a holder around an initial snapshot.
    ///
    /// `baseline` is captured before the initial load runs, so a change made during that load is
    /// observed by the first poll.
    pub(crate) fn new(
        sources: Vec<ResolvedSource>,
        delimiter: char,
        snapshot: Arc<Snapshot>,
        watch_paths: Vec<PathBuf>,
        baseline: Fingerprints,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                sources,
                delimiter,
                watch_paths,
                published: RwLock::new(Published {
                    generation: 0,
                    snapshot,
                }),
                refresh: Mutex::new(RefreshState {
                    observed: baseline,
                    pending: false,
                }),
                subscribers: Mutex::new(Vec::new()),
                last_error: Mutex::new(None),
            }),
        }
    }

    /// Returns the currently published snapshot.
    ///
    /// The returned [`Config`] is an independent clone: it keeps the old snapshot alive and keeps
    /// reading it even after a later refresh publishes a different one.
    #[must_use]
    pub fn config(&self) -> Config {
        self.inner.config()
    }

    /// Reloads every source and publishes the result when it differs.
    ///
    /// This does not consult the watched-file baseline; use [`Reload::refresh_if_changed`] for
    /// that. Every source is loaded, including a file source whose `reload_on_change` is `false`
    /// and a source that reports no path at all, whether or not the watched path list is empty.
    /// Concurrent calls are serialized. A successful load that produces an identical value tree
    /// and provenance returns [`RefreshOutcome::Unchanged`] and does not notify subscribers.
    ///
    /// # Errors
    ///
    /// A source failure is returned as [`RefreshOutcome::Failed`] and does not replace the
    /// published snapshot.
    #[must_use]
    pub fn refresh(&self) -> RefreshOutcome {
        self.inner.reload(false)
    }

    /// Reloads only when a watched file's contents changed since the last successful refresh or
    /// poll, an earlier reload failed, or a watched path cannot be fingerprinted.
    ///
    /// This is the polling primitive: it fingerprints the watched files, compares the result with
    /// the contents observed by the previous successful refresh, poll or initial load, and starts a
    /// load when they differ, when the previous reload failed, or when a fingerprint is unreadable.
    /// Apart from that choice it behaves like [`Reload::refresh`]. A load it starts reads **every**
    /// source, including one whose `reload_on_change` is `false`, because a change in any watched
    /// path can affect the merged snapshot. The watched list is a union: two sources may report the
    /// same path, and turning change watching off for one of them leaves the path watched when
    /// another source reports it.
    ///
    /// With no watched files it always returns [`RefreshOutcome::Unchanged`], because nothing
    /// observable can change; that includes a failure left pending by an earlier manual refresh,
    /// which needs [`Reload::refresh`] to retry. A change in an input that is not a watched path,
    /// such as a credential store, is invisible here and needs [`Reload::refresh`]. A file source
    /// with `reload_on_change(false)` contributes no path: a load triggered by another watched path
    /// still reads it, but a change to that file alone is not observed, whether or not other paths
    /// are watched, and needs [`Reload::refresh`].
    ///
    /// A watched path that is absent is not the same observation as one that exists but cannot be
    /// fingerprinted. Only an actual [`std::io::ErrorKind::NotFound`] counts as absent; a directory
    /// at the path, a permission failure, a parent that is not a directory, and a read error are
    /// unknown contents. While a watched path is in that state this method attempts a load rather
    /// than treating it as unchanged, so an unreadable path never passes for an unchanged one. The
    /// attempt's outcome is decided by the sources, not by the fingerprint: a source that needs the
    /// path may fail, which returns [`RefreshOutcome::Failed`] and keeps the previous snapshot
    /// published, while a custom source that reports the path but does not read it can still load.
    /// A successful load then follows the usual snapshot rule: an equal snapshot returns
    /// [`RefreshOutcome::Unchanged`] and publishes nothing, and only a different one is published
    /// as [`RefreshOutcome::Reloaded`]. Recovery is therefore a load attempt, not a publication:
    /// once the path becomes readable the next call loads, but it publishes only when the resulting
    /// snapshot differs from the published one.
    ///
    /// A failed reload stays pending until a later refresh succeeds. While it is pending, this
    /// method retries the load even if no watched file changed, so a transient failure in another
    /// source recovers without a second file write. The retry is bounded by the caller's polling
    /// interval, not a spin loop, and the last good snapshot remains published.
    ///
    /// # Errors
    ///
    /// A source failure is returned as [`RefreshOutcome::Failed`], and the previous snapshot
    /// remains published. The observed fingerprint is not advanced, so the next call retries.
    #[must_use]
    pub fn refresh_if_changed(&self) -> RefreshOutcome {
        if self.inner.watch_paths.is_empty() {
            return RefreshOutcome::Unchanged;
        }
        self.inner.reload(true)
    }

    /// Subscribes to future snapshot changes.
    ///
    /// The subscription starts at the current generation, so it observes changes published after
    /// this call. Each subscription has its own generation and is independent of every other
    /// subscription. Dropping it unregisters it.
    #[must_use]
    pub fn subscribe(&self) -> Subscription {
        // Hold the subscriber list while reading the current generation. A publication updates
        // the published pair before it tries to notify, so either this call sees the new
        // generation or the notifier waits and updates this subscriber; neither can miss it.
        let mut subscribers = lock(&self.inner.subscribers);
        let generation = self.inner.generation();
        let state = Arc::new(SubscriberState {
            signal: Mutex::new(Signal {
                generation,
                closed: false,
            }),
            ready: Condvar::new(),
        });
        subscribers.push(Arc::clone(&state));
        drop(subscribers);
        Subscription {
            inner: Arc::downgrade(&self.inner),
            state,
            seen: Cell::new(generation),
            #[cfg(test)]
            hook: None,
        }
    }

    /// Starts an explicit polling thread over the files the sources report.
    ///
    /// The thread waits `interval` between polls, calls [`Reload::refresh_if_changed`], and keeps
    /// running after a failed refresh. A pending failure is retried on the next poll after
    /// `interval`, even when no watched file changed. No thread is created unless this method is
    /// called. The returned [`Watcher`] stops and joins the thread on [`Watcher::stop`] or on drop.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError::NoWatchableSources`] when no registered source reports a path, and
    /// [`WatchError::ZeroInterval`] for a zero interval, which would spin.
    /// Returns [`WatchError::SpawnFailed`] when the operating system refuses to create the thread;
    /// the operating system's message is not forwarded.
    pub fn start_watching(&self, interval: Duration) -> Result<Watcher, WatchError> {
        if interval.is_zero() {
            return Err(WatchError::ZeroInterval);
        }
        if self.inner.watch_paths.is_empty() {
            return Err(WatchError::NoWatchableSources);
        }

        let inner = Arc::clone(&self.inner);
        let stop = Arc::new(StopFlag::default());
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name(String::from("neotales-config-watch"))
            .spawn(move || {
                let mut stopped = lock(&thread_stop.stopped);
                loop {
                    if *stopped {
                        break;
                    }
                    let result = thread_stop
                        .changed
                        .wait_timeout_while(stopped, interval, |stopped| !*stopped)
                        .unwrap_or_else(PoisonError::into_inner);
                    stopped = result.0;
                    if *stopped {
                        break;
                    }
                    drop(stopped);
                    // A failure stays pending on the holder, so the next tick retries it even if
                    // no watched byte changed. The poller therefore keeps running across both a
                    // torn document and a transiently unavailable source.
                    inner.reload(true);
                    stopped = lock(&thread_stop.stopped);
                }
            })
            .map_err(|_| WatchError::SpawnFailed)?;

        Ok(Watcher {
            stop,
            thread: Some(handle),
        })
    }

    /// The sanitized error from the most recent failed refresh, if any.
    ///
    /// A later successful refresh clears it. An [`RefreshOutcome::Unchanged`] from
    /// [`Reload::refresh`] also clears it, because the sources loaded successfully. An unchanged
    /// result from [`Reload::refresh_if_changed`] leaves it in place, because nothing was loaded;
    /// when no watched path exists, that call does not retry a pending failure, so use
    /// [`Reload::refresh`] for it.
    #[must_use]
    pub fn last_error(&self) -> Option<Error> {
        lock(&self.inner.last_error).clone()
    }

    /// The number of published snapshots so far.
    ///
    /// The initial snapshot is generation 0; each published change increments it by one. A no-op
    /// refresh and a failed refresh do not increment it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.generation()
    }

    /// The number of distinct files the polling watcher observes.
    ///
    /// The paths themselves are not exposed, because a path can name a host or a user.
    #[must_use]
    pub fn watched_path_count(&self) -> usize {
        self.inner.watch_paths.len()
    }
}

/// A change subscription with its own coalesced generation.
///
/// Create one with [`Reload::subscribe`]. It is [`Send`] but not [`Sync`], like a receiver in
/// [`std::sync::mpsc`]: one reader owns it and may move it to another thread.
///
/// Notifications are coalesced. If two snapshots are published before the subscriber reads, the
/// next `try_recv`/`recv`/`recv_timeout` returns the newest snapshot once. Every subscriber
/// coalesces independently, so a slow subscriber cannot hide a change from another subscriber.
pub struct Subscription {
    inner: Weak<Inner>,
    state: Arc<SubscriberState>,
    seen: Cell<u64>,
    /// A unit-test seam that runs while the published pair is read-locked, so an interleaving
    /// test can prove a publisher cannot slip between the generation and the snapshot.
    #[cfg(test)]
    hook: Option<Box<dyn Fn() + Send>>,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the subscription holds a holder reference and a generation; neither is reportable"
)]
impl fmt::Debug for Subscription {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.debug_struct("Subscription").finish_non_exhaustive()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            let mut subscribers = lock(&inner.subscribers);
            subscribers.retain(|state| !Arc::ptr_eq(state, &self.state));
        }
    }
}

impl Subscription {
    /// Returns the newest snapshot if a change was published since the last read.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] when the subscriber is up to date.
    /// Returns [`TryRecvError::Disconnected`] when the holder has been dropped.
    pub fn try_recv(&self) -> Result<Config, TryRecvError> {
        self.try_take()
    }

    /// Blocks until a change is published or the holder is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`RecvError`] when the holder has been dropped.
    pub fn recv(&self) -> Result<Config, RecvError> {
        loop {
            match self.try_take() {
                Ok(config) => return Ok(config),
                Err(TryRecvError::Disconnected) => return Err(RecvError),
                Err(TryRecvError::Empty) => {}
            }
            let mut signal = lock(&self.state.signal);
            while !signal.closed && signal.generation <= self.seen.get() {
                signal = self
                    .state
                    .ready
                    .wait(signal)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }

    /// Blocks for at most `timeout` waiting for a change.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when no change arrived in time, and
    /// [`RecvTimeoutError::Disconnected`] when the holder has been dropped.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Config, RecvTimeoutError> {
        let start = Instant::now();
        loop {
            match self.try_take() {
                Ok(config) => return Ok(config),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }
            let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
                return Err(RecvTimeoutError::Timeout);
            };
            if remaining.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            let signal = lock(&self.state.signal);
            if signal.closed {
                return Err(RecvTimeoutError::Disconnected);
            }
            if signal.generation > self.seen.get() {
                // A publish landed between the empty read and the wait; re-read instead of
                // sleeping until the timeout.
                drop(signal);
                continue;
            }
            let result = self
                .state
                .ready
                .wait_timeout(signal, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            drop(result.0);
        }
    }

    fn try_take(&self) -> Result<Config, TryRecvError> {
        let Some(inner) = self.inner.upgrade() else {
            return Err(TryRecvError::Disconnected);
        };
        // One lock covers both the generation and the snapshot. A publication is a single write
        // of the pair, so a reader can never set `seen` from one publication and return a
        // snapshot from another, which would deliver the same snapshot twice.
        let published = read(&inner.published);
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook();
        }
        if published.generation <= self.seen.get() {
            return Err(TryRecvError::Empty);
        }
        let generation = published.generation;
        let snapshot = Arc::clone(&published.snapshot);
        drop(published);
        self.seen.set(generation);
        Ok(Config::root(snapshot, inner.delimiter))
    }
}

/// Why a polling watcher could not be started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchError {
    /// No registered source reports a file path, so there is nothing to poll.
    NoWatchableSources,
    /// A zero polling interval would spin the thread.
    ZeroInterval,
    /// The operating system refused to create the polling thread.
    SpawnFailed,
}

impl fmt::Display for WatchError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWatchableSources => {
                output.write_str("no registered source reports a watched path")
            }
            Self::ZeroInterval => output.write_str("the polling interval must not be zero"),
            Self::SpawnFailed => output.write_str("the polling thread could not be started"),
        }
    }
}

impl std::error::Error for WatchError {}

/// A running polling watcher.
///
/// Owns exactly one thread. [`Watcher::stop`] and [`Drop`] set the stop flag, wake the thread, and
/// join it. A failed refresh is recorded on [`Reload::last_error`] and does not stop the thread.
///
/// [`fmt::Debug`] reports whether the thread is still running; it never prints a path or a value.
pub struct Watcher {
    stop: Arc<StopFlag>,
    thread: Option<JoinHandle<()>>,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the watcher owns a stop flag and a thread handle; only liveness is reportable"
)]
impl fmt::Debug for Watcher {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("Watcher")
            .field("running", &self.is_running())
            .finish()
    }
}

impl Watcher {
    /// Stops the polling thread and waits for it to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Whether the polling thread is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    fn shutdown(&mut self) {
        self.stop.stop();
        if let Some(thread) = self.thread.take() {
            // Nothing useful can be done with a thread that panicked while polling; the holder
            // and its last error remain available to the caller.
            let _ = thread.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The shared state behind every [`Reload`] clone.
struct Inner {
    sources: Vec<ResolvedSource>,
    delimiter: char,
    watch_paths: Vec<PathBuf>,
    /// The published generation and its snapshot, replaced together under one write lock.
    published: RwLock<Published>,
    refresh: Mutex<RefreshState>,
    subscribers: Mutex<Vec<Arc<SubscriberState>>>,
    last_error: Mutex<Option<Error>>,
}

/// One immutable snapshot together with the generation that published it.
struct Published {
    generation: u64,
    snapshot: Arc<Snapshot>,
}

impl Inner {
    fn config(&self) -> Config {
        let published = read(&self.published);
        Config::root(Arc::clone(&published.snapshot), self.delimiter)
    }

    fn generation(&self) -> u64 {
        read(&self.published).generation
    }

    /// Serialized reload of every source.
    fn reload(&self, only_if_changed: bool) -> RefreshOutcome {
        let mut state = lock(&self.refresh);

        // Capture the baseline under the refresh lock and before the load. A change made between
        // this fingerprint and the load is loaded now and seen as a difference on the next poll,
        // which is safe: it can only cause a redundant reload, never a missed one.
        let observed = Fingerprints::capture(&self.watch_paths);

        // A failed reload leaves `pending` set. Retry it even when no watched byte changed: the
        // failure may belong to another source, or to a torn read that does not alter the
        // fingerprint. The caller's polling interval bounds the retry rate.
        //
        // An unreadable watched path is never "unchanged": its contents are unknown, so comparing
        // it with the previous observation would hide a source that now fails. The load itself must
        // decide. `unknown` is checked before the comparison so this stays true even when the
        // previous observation is also unreadable.
        if only_if_changed && !state.pending && !observed.is_unknown() && state.observed == observed
        {
            return RefreshOutcome::Unchanged;
        }

        let roots = match load_roots(&self.sources) {
            Ok(roots) => roots,
            Err(error) => {
                // Do not advance the observed fingerprint. A later poll must load again, and the
                // published pair stays untouched.
                state.pending = true;
                *lock(&self.last_error) = Some(error.clone());
                return RefreshOutcome::Failed(error);
            }
        };
        let candidate = merge_roots(&self.sources, roots);

        // The load succeeded, so this fingerprint is now the baseline. Advancing it only here is
        // what makes a failed reload retryable; it also keeps the initial-load ordering contract.
        state.observed = observed;
        state.pending = false;

        {
            let current = read(&self.published);
            if current.snapshot.as_ref() == &candidate {
                drop(current);
                *lock(&self.last_error) = None;
                return RefreshOutcome::Unchanged;
            }
        }

        let generation = {
            let mut current = write(&self.published);
            let generation = current.generation + 1;
            *current = Published {
                generation,
                snapshot: Arc::new(candidate),
            };
            generation
        };
        *lock(&self.last_error) = None;

        self.notify(generation);
        RefreshOutcome::Reloaded
    }

    /// Marks every subscriber at `generation` and wakes it.
    fn notify(&self, generation: u64) {
        let subscribers = lock(&self.subscribers);
        for subscriber in subscribers.iter() {
            let mut signal = lock(&subscriber.signal);
            signal.generation = generation;
            subscriber.ready.notify_all();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let subscribers = match self.subscribers.get_mut() {
            Ok(subscribers) => subscribers,
            Err(poisoned) => poisoned.into_inner(),
        };
        for subscriber in subscribers.iter() {
            let mut signal = lock(&subscriber.signal);
            signal.closed = true;
            subscriber.ready.notify_all();
        }
    }
}

/// What the previous successful refresh observed in the watched files.
struct RefreshState {
    observed: Fingerprints,
    /// A prior reload failed and has not yet succeeded.
    pending: bool,
}

struct SubscriberState {
    signal: Mutex<Signal>,
    ready: Condvar,
}

struct Signal {
    generation: u64,
    closed: bool,
}

#[derive(Default)]
struct StopFlag {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl StopFlag {
    fn stop(&self) {
        {
            *lock(&self.stopped) = true;
        }
        self.changed.notify_all();
    }
}

/// One observation per watched path, in the same order as the path list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprints(Vec<Fingerprint>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fingerprint {
    /// The path does not exist: opening it reported [`io::ErrorKind::NotFound`]. This is the one
    /// state an optional source accepts as missing, so it can be compared safely.
    Absent,
    /// The path exists but could not be fingerprinted, or was read only partly: a directory at the
    /// path, a permission failure, a parent that is not a directory, or a read error. The contents
    /// are unknown, so this observation never proves that the path is unchanged.
    Unreadable,
    /// The contents, reduced to a process-local digest. The bytes are not retained.
    Present(u64),
}

impl Fingerprints {
    pub(crate) fn capture(paths: &[PathBuf]) -> Self {
        Self(paths.iter().map(|path| Fingerprint::of(path)).collect())
    }

    /// Whether any watched path exists but could not be read.
    ///
    /// Such a path is not a stable observation: its contents, and therefore a source's success,
    /// can differ from the previous poll without any comparable fingerprint changing. A poll must
    /// load instead of calling it unchanged.
    fn is_unknown(&self) -> bool {
        self.0.contains(&Fingerprint::Unreadable)
    }
}

impl Fingerprint {
    fn of(path: &Path) -> Self {
        // The open and read results decide. There is no existence check that a file could appear or
        // vanish between, and only NotFound is an absence.
        match File::open(path) {
            Ok(file) => match fingerprint_reader(file) {
                Ok(value) => Self::Present(value),
                Err(_) => Self::Unreadable,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::Absent,
            Err(_) => Self::Unreadable,
        }
    }
}

/// The size of one fingerprint read. Memory is bounded by this, not by the file size; the whole
/// file is still read.
const CHUNK_BYTES: usize = 16 * 1024;

/// Hashes a reader in fixed-size chunks.
///
/// The buffer is allocated once per call and never grows. This bounds memory for the fingerprint
/// of any file size; it does not bound the disk I/O, which still reads the whole file.
fn fingerprint_reader(mut reader: impl Read) -> io::Result<u64> {
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut digest = Digest::new();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(digest.finish()),
            Ok(read) => {
                digest.update(&buffer[..read]);
                digest.add_length(read);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// A deterministic 64-bit digest of file contents.
///
/// This is FNV-1a followed by a length mix, not a cryptographic hash. It never leaves the process,
/// is never printed by a public type, and is only compared with the previous poll. A collision
/// would mean one missed automatic poll, which is why the contents are read rather than trusting a
/// timestamp; a manual [`Reload::refresh`] does not depend on it at all.
struct Digest {
    hash: u64,
    length: u64,
}

impl Digest {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self {
            hash: Self::OFFSET,
            length: 0,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(Self::PRIME);
        }
    }

    fn add_length(&mut self, read: usize) {
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        self.length = self.length.saturating_add(read);
    }

    const fn finish(&self) -> u64 {
        (self.hash ^ self.length).wrapping_mul(Self::PRIME)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Read},
        sync::{
            Arc, Mutex,
            atomic::{AtomicI64, AtomicUsize, Ordering},
            mpsc::{self, RecvTimeoutError},
        },
        thread,
        time::Duration,
    };

    use crate::{Builder, RefreshOutcome, SourceError, Value};

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn value(number: i64) -> Value {
        Value::map([("value", Value::from(number))])
    }

    /// A publisher must not be able to change the snapshot between the moment a subscriber selects
    /// its generation and the moment it clones that generation's snapshot. The hook holds the
    /// pair's read guard open while a publisher tries to publish, so the interleaving that once
    /// delivered one snapshot under two generations is deterministic.
    #[test]
    fn a_publication_cannot_interleave_between_generation_and_snapshot() {
        let counter = Arc::new(AtomicI64::new(1));
        let source_counter = Arc::clone(&counter);
        let reload = Builder::new()
            .source("counter", move || {
                Ok::<_, SourceError>(value(source_counter.load(Ordering::SeqCst)))
            })
            .build_reloadable()
            .expect("the fixture builds");

        let mut subscription = reload.subscribe();
        counter.store(2, Ordering::SeqCst);
        assert!(reload.refresh().is_reloaded());

        // Pause the reader while it holds the published pair open, once.
        let (paused_tx, paused_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume = Arc::new(Mutex::new(Some(resume_rx)));
        let hook = move || {
            let receiver = resume.lock().expect("the hook lock is not poisoned").take();
            if let Some(receiver) = receiver {
                paused_tx.send(()).expect("the test listens for the pause");
                receiver.recv().expect("the test resumes the reader");
            }
        };
        subscription.hook = Some(Box::new(hook));

        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let (resume_reader_tx, resume_reader_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let first = subscription
                .try_recv()
                .expect("the first change is pending")
                .get::<i64>("value")
                .expect("the fixture value reads");
            first_tx
                .send(first)
                .expect("the test reads the first value");
            resume_reader_rx
                .recv()
                .expect("the test allows the second read");
            let second = subscription
                .try_recv()
                .map(|config| config.get::<i64>("value").expect("the fixture value reads"));
            second_tx
                .send(second)
                .expect("the test reads the second value");
        });

        paused_rx
            .recv_timeout(TIMEOUT)
            .expect("the reader reached its post-generation pause");

        counter.store(3, Ordering::SeqCst);
        // Keep a strong holder alive while the publisher (which owns the original) finishes, the
        // way a real process holds its `Reload` across subscriber reads.
        let _holder = reload.clone();
        let (published_tx, published_rx) = mpsc::channel();
        let publisher = thread::spawn(move || {
            let outcome = reload.refresh();
            let _ = published_tx.send(());
            outcome
        });

        // The reader holds the pair, so the publisher cannot complete until it resumes.
        assert_eq!(
            published_rx.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout),
            "a publication interleaved between the generation and its snapshot"
        );

        resume_tx.send(()).expect("the reader is paused");
        assert_eq!(
            first_rx.recv_timeout(TIMEOUT).expect("the reader returns"),
            2,
            "the first read must return the snapshot for the generation it observed"
        );
        assert!(
            publisher
                .join()
                .expect("the publisher does not panic")
                .is_reloaded(),
            "the publication succeeds once the reader releases the pair"
        );

        resume_reader_tx.send(()).expect("the reader is waiting");
        assert_eq!(
            second_rx.recv_timeout(TIMEOUT).expect("the reader returns"),
            Ok(3),
            "the next read delivers the new snapshot exactly once"
        );
        reader.join().expect("the reader does not panic");
    }

    #[test]
    fn fingerprinting_streams_in_bounded_chunks_and_covers_every_byte() {
        struct RecordingReader {
            data: Vec<u8>,
            position: usize,
            largest_request: Arc<AtomicUsize>,
        }

        impl Read for RecordingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.largest_request
                    .fetch_max(buffer.len(), Ordering::SeqCst);
                let remaining = &self.data[self.position..];
                // A reader may return less than the buffer holds; the hasher must keep going
                // without losing or duplicating a byte.
                let size = remaining.len().min(buffer.len()).min(7);
                buffer[..size].copy_from_slice(&remaining[..size]);
                self.position += size;
                Ok(size)
            }
        }

        let data: Vec<u8> = (0..super::CHUNK_BYTES * 3 + 7)
            .map(|index| u8::try_from(index % 251).expect("the remainder fits in u8"))
            .collect();
        let largest = Arc::new(AtomicUsize::new(0));
        let observed = super::fingerprint_reader(RecordingReader {
            data: data.clone(),
            position: 0,
            largest_request: Arc::clone(&largest),
        })
        .expect("an in-memory reader never fails");

        let mut expected = super::Digest::new();
        expected.update(&data);
        expected.add_length(data.len());
        assert_eq!(observed, expected.finish());
        assert_eq!(
            largest.load(Ordering::SeqCst),
            super::CHUNK_BYTES,
            "the fingerprint must use one fixed-size buffer"
        );
    }

    #[test]
    fn a_poll_attempts_a_load_while_a_watched_path_is_unreadable() {
        // A source that reports a path but does not read it in `load`. That is the shape that
        // isolates the fingerprint decision: the path's contents cannot be compared, so only the
        // load may decide whether the source still works.
        struct PathSource {
            path: std::path::PathBuf,
            failing: Arc<std::sync::atomic::AtomicBool>,
            loads: Arc<AtomicI64>,
            value: Arc<AtomicI64>,
        }

        impl crate::Source for PathSource {
            fn load(&self) -> Result<Value, SourceError> {
                self.loads.fetch_add(1, Ordering::SeqCst);
                if self.failing.load(Ordering::SeqCst) {
                    return Err(SourceError::unavailable());
                }
                Ok(value(self.value.load(Ordering::SeqCst)))
            }

            fn watch_paths(&self) -> Vec<std::path::PathBuf> {
                vec![self.path.clone()]
            }
        }

        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("watched");
        // A directory is an existing, unreadable watched path: not an absence.
        std::fs::create_dir(&path).expect("the directory fixture is creatable");

        let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let loads = Arc::new(AtomicI64::new(0));
        let source_value = Arc::new(AtomicI64::new(1));
        let reload = Builder::new()
            .source(
                "path",
                PathSource {
                    path: path.clone(),
                    failing: Arc::clone(&failing),
                    loads: Arc::clone(&loads),
                    value: Arc::clone(&source_value),
                },
            )
            .build_reloadable()
            .expect("the source loads even though the watched path is unreadable");

        // The previous successful observation is unreadable, which is exactly the state that a
        // naive fingerprint comparison would call unchanged forever.
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
        assert_eq!(
            loads.load(Ordering::SeqCst),
            2,
            "the poll loaded the source"
        );

        failing.store(true, Ordering::SeqCst);
        let outcome = reload.refresh_if_changed();
        assert!(
            outcome.is_failed(),
            "an unreadable watched path must not suppress the load: {outcome:?}"
        );
        assert_eq!(loads.load(Ordering::SeqCst), 3);
        assert!(reload.last_error().is_some());

        // The failure stays pending, and the unreadable observation keeps forcing a load even
        // after a failure is cleared by a successful one.
        failing.store(false, Ordering::SeqCst);
        assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Unchanged);
        assert_eq!(loads.load(Ordering::SeqCst), 4);
        assert!(reload.last_error().is_none());

        // Recovery: a readable path is a comparable observation again, and a new value publishes.
        std::fs::remove_dir(&path).expect("the directory fixture is removable");
        std::fs::write(&path, b"present").expect("the replacement is writable");
        source_value.store(2, Ordering::SeqCst);
        let outcome = reload.refresh_if_changed();
        assert!(outcome.is_reloaded(), "{outcome:?}");
        assert_eq!(
            reload
                .config()
                .get::<i64>("value")
                .expect("the value reads"),
            2
        );
    }

    #[test]
    fn absence_an_unreadable_path_and_a_present_empty_file_are_distinct() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let absent = directory.path().join("absent");
        let empty = directory.path().join("empty");
        std::fs::write(&empty, b"").expect("the empty fixture is writable");
        // A regular file where a directory is needed makes every path below it non-NotFound,
        // without depending on permissions or on the user the test runs as.
        let not_a_directory = directory.path().join("parent");
        std::fs::write(&not_a_directory, b"not a directory").expect("the fixture is writable");
        let beneath_a_file = not_a_directory.join("child");

        assert_eq!(
            super::Fingerprint::of(&absent),
            super::Fingerprint::Absent,
            "only a real not-found is an absence"
        );
        let unreadable_directory = super::Fingerprint::of(directory.path());
        assert_eq!(
            unreadable_directory,
            super::Fingerprint::Unreadable,
            "a directory is not an absence and is not a readable watched file"
        );
        assert_eq!(
            super::Fingerprint::of(&beneath_a_file),
            super::Fingerprint::Unreadable,
            "a non-directory parent is an unreadable path, not an absent one"
        );
        assert_ne!(unreadable_directory, super::Fingerprint::Absent);
        assert!(
            super::Fingerprints::capture(&[absent, beneath_a_file]).is_unknown(),
            "a capture that holds an unreadable path must force a load"
        );

        let present = super::Fingerprint::of(&empty);
        assert_eq!(present, super::Fingerprint::of(&empty));
        assert_ne!(present, super::Fingerprint::Absent);
        assert_ne!(present, super::Fingerprint::Unreadable);
        assert!(
            !super::Fingerprints::capture(&[empty]).is_unknown(),
            "a readable file is a comparable observation"
        );
    }
}
