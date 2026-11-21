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
//!   changed since the last poll. It is the polling primitive a caller can drive from its own
//!   event loop; [`Reload::start_watching`] is an explicit thread-based helper for callers that
//!   have no loop.
//! - [`Reload::subscribe`] returns a [`Subscription`] for change notifications. Every subscriber
//!   has its own generation, so one slow reader cannot consume or hide another reader's
//!   notification. Notifications coalesce: a subscriber that falls behind is told about the
//!   latest snapshot, not every intermediate one.
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
//! - Each poll reads every watched file in full to fingerprint its bytes. Cost is proportional to
//!   the total watched size, not to the size of a change. A large file watched at a short interval
//!   is expensive.
//! - A write in progress can be observed as a partial document. The refresh fails safely, the
//!   previous good snapshot stays published, [`Reload::last_error`] reports the failure, and the
//!   next poll retries when the bytes change again. Write a temporary file and rename it over the
//!   target to avoid a torn read.
//! - A change that produces byte-identical contents is not a change. The snapshot would be
//!   identical, so no refresh and no notification is produced.
//! - A path that is deleted is a change; a path that is recreated with new contents is another.
//!   Fingerprints use file contents rather than timestamps, so a replacement that preserves the
//!   size or the modification time is still detected.
//! - A source that reads no stable local file reports no path. Watching such a source has nothing
//!   to observe; use [`Reload::refresh`] when the caller knows its input changed.
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
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
        Weak,
        atomic::{AtomicU64, Ordering},
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
/// `Unchanged` means every source loaded and the merged value tree, including its provenance,
/// matched the currently published snapshot. `Reloaded` means a new snapshot was published.
/// `Failed` means a source could not be read, parsed, or represented; the previously published
/// snapshot is still current.
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
            .field("generation", &self.inner.generation.load(Ordering::Relaxed))
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
                snapshot: RwLock::new(snapshot),
                refresh: Mutex::new(RefreshState { observed: baseline }),
                subscribers: Mutex::new(Vec::new()),
                generation: AtomicU64::new(0),
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
    /// that. Concurrent calls are serialized. A successful load that produces an identical value
    /// tree and provenance returns [`RefreshOutcome::Unchanged`] and does not notify subscribers.
    ///
    /// # Errors
    ///
    /// A source failure is returned as [`RefreshOutcome::Failed`] and does not replace the
    /// published snapshot.
    #[must_use]
    pub fn refresh(&self) -> RefreshOutcome {
        self.inner.reload(false)
    }

    /// Reloads only when a watched file's contents changed since the last refresh or poll.
    ///
    /// This is the polling primitive: it fingerprints the watched files, compares the result with
    /// the contents observed by the previous refresh, poll or initial load, and otherwise behaves
    /// like [`Reload::refresh`]. With no watched files it always returns
    /// [`RefreshOutcome::Unchanged`], because nothing observable can change.
    ///
    /// # Errors
    ///
    /// A changed file that cannot be read or parsed is returned as [`RefreshOutcome::Failed`],
    /// and the failure is remembered so the same bytes are not retried on every poll. The
    /// previous snapshot remains published.
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
        let mut subscribers = lock(&self.inner.subscribers);
        let generation = self.inner.generation.load(Ordering::SeqCst);
        let state = Arc::new(SubscriberState {
            signal: Mutex::new(Signal {
                generation,
                closed: false,
            }),
            ready: Condvar::new(),
        });
        subscribers.push(Arc::clone(&state));
        Subscription {
            inner: Arc::downgrade(&self.inner),
            state,
            seen: Cell::new(generation),
        }
    }

    /// Starts an explicit polling thread over the files the sources report.
    ///
    /// The thread waits `interval` between polls, calls [`Reload::refresh_if_changed`], and keeps
    /// running after a failed refresh. No thread is created unless this method is called. The
    /// returned [`Watcher`] stops and joins the thread on [`Watcher::stop`] or on drop.
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
                    // The failure is stored on the holder; the poller keeps running so a later
                    // valid document can recover without restarting the watcher.
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
    /// [`Reload::refresh`] also clears it, because the sources loaded successfully; an unchanged
    /// result from [`Reload::refresh_if_changed`] leaves it in place, because nothing was loaded.
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
        self.inner.generation.load(Ordering::SeqCst)
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
        let signal = lock(&self.state.signal);
        if signal.closed {
            return Err(TryRecvError::Disconnected);
        }
        if signal.generation <= self.seen.get() {
            return Err(TryRecvError::Empty);
        }
        let generation = signal.generation;
        drop(signal);
        self.seen.set(generation);
        let Some(inner) = self.inner.upgrade() else {
            return Err(TryRecvError::Disconnected);
        };
        Ok(inner.config())
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
    snapshot: RwLock<Arc<Snapshot>>,
    refresh: Mutex<RefreshState>,
    subscribers: Mutex<Vec<Arc<SubscriberState>>>,
    generation: AtomicU64,
    last_error: Mutex<Option<Error>>,
}

impl Inner {
    fn config(&self) -> Config {
        let snapshot = read(&self.snapshot);
        Config::root(Arc::clone(&*snapshot), self.delimiter)
    }

    /// Serialized reload of every source.
    fn reload(&self, only_if_changed: bool) -> RefreshOutcome {
        let mut state = lock(&self.refresh);

        // Capture the baseline under the refresh lock and before the load. A change made between
        // this fingerprint and the load is loaded now and seen as a difference on the next poll,
        // which is safe: it can only cause a redundant reload, never a missed one.
        let observed = Fingerprints::capture(&self.watch_paths);
        if only_if_changed && state.observed == observed {
            return RefreshOutcome::Unchanged;
        }
        state.observed = observed;

        let roots = match load_roots(&self.sources) {
            Ok(roots) => roots,
            Err(error) => {
                *lock(&self.last_error) = Some(error.clone());
                return RefreshOutcome::Failed(error);
            }
        };
        let candidate = merge_roots(&self.sources, roots);

        {
            let current = read(&self.snapshot);
            if current.as_ref() == &candidate {
                drop(current);
                *lock(&self.last_error) = None;
                return RefreshOutcome::Unchanged;
            }
        }

        {
            let mut current = write(&self.snapshot);
            *current = Arc::new(candidate);
        }
        *lock(&self.last_error) = None;

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
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

/// What the previous refresh observed in the watched files.
struct RefreshState {
    observed: Fingerprints,
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

/// One digest per watched path, in the same order as the path list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprints(Vec<Fingerprint>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fingerprint {
    /// The path could not be read: absent, unreadable, or replaced by a directory.
    Missing,
    /// The contents, reduced to a process-local digest. The bytes are not retained.
    Present(u64),
}

impl Fingerprints {
    pub(crate) fn capture(paths: &[PathBuf]) -> Self {
        Self(paths.iter().map(|path| Fingerprint::of(path)).collect())
    }
}

impl Fingerprint {
    fn of(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => Self::Present(digest(&bytes)),
            Err(_) => Self::Missing,
        }
    }
}

/// A deterministic 64-bit digest of file contents.
///
/// This is FNV-1a followed by a length mix, not a cryptographic hash. It never leaves the process,
/// is never printed by a public type, and is only compared with the previous poll. A collision
/// would mean one missed automatic poll, which is why the contents are read rather than trusting a
/// timestamp; a manual [`Reload::refresh`] does not depend on it at all.
fn digest(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    hash ^= length;
    hash.wrapping_mul(PRIME)
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
