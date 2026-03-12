//! Filesystem watcher with per-file debouncing.
//!
//! Uses the `notify` crate for cross-platform file watching. Events are
//! debounced per-file (configurable, default 300ms) via a background drain
//! thread, then forwarded as `FileEvent` messages for incremental rescan.

use crate::core::snapshot::FileEvent;
use crossbeam_channel::Sender;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Handle to a running file watcher — dropping it stops watching.
/// Sets the shutdown flag on drop and joins the drain thread to prevent
/// stale events from being injected after the watcher is dropped.
pub struct WatcherHandle {
    /// The underlying filesystem watcher (kept alive while this handle exists)
    _watcher: RecommendedWatcher,
    /// Flag to signal the drain thread to exit
    shutdown: Arc<AtomicBool>,
    /// Background thread that debounces and forwards events
    drain_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Join drain thread to ensure no stale events leak after drop
        if let Some(handle) = self.drain_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Return true if the path should be filtered out based on hardcoded ignore lists.
/// Checks every segment of the relative path against the scanner's IGNORED_DIRS,
/// which is the canonical list shared with the scanner itself.
fn should_skip_path_hardcoded(rel: &str) -> bool {
    use crate::analysis::scanner::common::should_ignore_dir;
    for segment in rel.split('/') {
        if segment.is_empty() {
            continue;
        }
        if should_ignore_dir(segment) {
            return true;
        }
    }
    false
}

/// Build a gitignore matcher from the project's .gitignore and .git/info/exclude.
/// Falls back to an empty matcher if no gitignore files exist or parsing fails.
fn build_gitignore(root: &Path) -> ignore::gitignore::Gitignore {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    if let Some(err) = builder.add(root.join(".gitignore")) {
        eprintln!("[watcher] .gitignore parse warning: {}", err);
    }
    if let Some(err) = builder.add(root.join(".git/info/exclude")) {
        eprintln!("[watcher] .git/info/exclude parse warning: {}", err);
    }
    match builder.build() {
        Ok(gi) => gi,
        Err(e) => {
            eprintln!("[watcher] gitignore build error: {}, using empty matcher", e);
            ignore::gitignore::GitignoreBuilder::new(root).build().unwrap()
        }
    }
}

/// Map a `notify::EventKind` to a static event kind string.
fn event_kind_str(kind: notify::EventKind) -> &'static str {
    match kind {
        notify::EventKind::Create(_) => "create",
        notify::EventKind::Remove(_) => "remove",
        notify::EventKind::Modify(_) => "modify",
        _ => "modify",
    }
}

/// Determine whether the event path represents a directory.
/// Uses event metadata first; falls back to filesystem check for non-remove events
/// (removed paths no longer exist on disk). [ref:93cf32d4]
fn is_dir_event(kind: notify::EventKind, path: &Path) -> bool {
    matches!(
        kind,
        notify::EventKind::Create(notify::event::CreateKind::Folder)
            | notify::EventKind::Remove(notify::event::RemoveKind::Folder)
    ) || (!matches!(kind, notify::EventKind::Remove(_)) && path.is_dir())
}

/// Insert an event into the pending map, recovering from a poisoned mutex.
fn insert_pending(
    pending: &Mutex<HashMap<String, (Instant, bool, &'static str)>>,
    rel_str: String,
    is_dir: bool,
    kind_str: &'static str,
) {
    match pending.lock() {
        Ok(mut map) => {
            map.insert(rel_str, (Instant::now(), is_dir, kind_str));
        }
        Err(poisoned) => {
            eprintln!("[watcher] mutex poisoned in callback, recovering");
            poisoned
                .into_inner()
                .insert(rel_str, (Instant::now(), is_dir, kind_str));
        }
    }
}

/// Drain entries from `pending` that have been stable longer than `debounce`.
/// Returns a vec of (relative_path, is_dir, event_kind) tuples.
fn drain_expired(
    pending: &Mutex<HashMap<String, (Instant, bool, &'static str)>>,
    debounce: Duration,
) -> Vec<(String, bool, &'static str)> {
    let mut to_emit = Vec::new();
    let mut map = match pending.lock() {
        Ok(m) => m,
        Err(poisoned) => {
            eprintln!("watcher-drain: mutex poisoned, recovering");
            poisoned.into_inner()
        }
    };
    let now = Instant::now();
    map.retain(|rel, (t, is_dir, kind)| {
        if now.duration_since(*t) >= debounce {
            to_emit.push((rel.clone(), *is_dir, *kind));
            false
        } else {
            true
        }
    });
    to_emit
}

/// Build a `FileEvent` from debounced event data.
fn build_file_event(rel: &str, is_dir: bool, event_kind: &str) -> FileEvent {
    FileEvent {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|e| {
                eprintln!("[watcher] system clock before epoch: {}", e);
                std::time::Duration::ZERO
            })
            .as_secs_f64(),
        kind: event_kind.to_string(),
        path: rel.to_string(),
        is_dir,
        diff: None,
        adds: None,
        dels: None,
    }
}

/// Send a `FileEvent` on the channel with a 100ms timeout.
/// Returns `true` if the drain loop should continue, `false` if the
/// receiver is disconnected and the loop should exit.
fn send_event(tx: &Sender<FileEvent>, fe: FileEvent, rel: &str) -> bool {
    match tx.send_timeout(fe, Duration::from_millis(100)) {
        Ok(()) => true,
        Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
            eprintln!(
                "watcher-drain: channel full after 100ms, dropping event for {}",
                rel
            );
            true
        }
        Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
            false // main thread gone, exit cleanly
        }
    }
}

/// Start watching `root` recursively.  Debounces events per-file (300ms)
/// and sends `FileEvent` on the provided channel.
/// Returns a handle; drop it to stop the watcher.
///
/// Uses manual debounce via a background drain thread to avoid
/// depending on notify-debouncer-mini's specific API shape.
///
/// Filters events against .gitignore (parsed at startup) and the scanner's
/// IGNORED_DIRS list to prevent gitignored paths (e.g. .gradle-home with an
/// active daemon) from triggering infinite rescan loops.
pub fn start_watcher(
    root: &str,
    tx: Sender<FileEvent>,
    debounce_ms: u64,
) -> Result<WatcherHandle, notify::Error> {
    let root_path = PathBuf::from(root);

    // Build gitignore matcher once — used in the callback to filter ignored paths
    let gitignore = build_gitignore(&root_path);

    // Pending events keyed by relative path, value = (last event time, is_dir, event kind)
    type PendingMap = HashMap<String, (Instant, bool, &'static str)>;
    let pending: Arc<Mutex<PendingMap>> =
        Arc::new(Mutex::new(HashMap::new()));
    let shutdown = Arc::new(AtomicBool::new(false));

    let pending_w = Arc::clone(&pending);
    let root_for_cb = root_path.clone();

    // Raw notify watcher — feeds into pending map
    // Bug #9: preserve event kind (create/modify/remove) from notify
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let event = match res {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[watcher] notify error: {}", e);
                    return;
                }
            };
            let kind_str = event_kind_str(event.kind);
            for path in &event.paths {
                if let Ok(rel) = path.strip_prefix(&root_for_cb) {
                    let rel_str = rel.to_string_lossy().to_string();
                    if rel_str.is_empty() || should_skip_path_hardcoded(&rel_str) {
                        continue;
                    }
                    let is_dir = is_dir_event(event.kind, path);
                    // Skip non-create/remove events on directories — modify and
                    // access events on dirs are noise from inotify watch setup
                    // and metadata changes (especially on CoW filesystems like
                    // btrfs). We get separate events for actual file changes.
                    if is_dir && !matches!(event.kind,
                        notify::EventKind::Create(_) | notify::EventKind::Remove(_))
                    {
                        continue;
                    }
                    // Check .gitignore — matched_path_or_any_parents also covers
                    // files inside gitignored directories (e.g. .gradle-home/*)
                    if gitignore.matched_path_or_any_parents(&rel_str, is_dir).is_ignore() {
                        continue;
                    }
                    insert_pending(&pending_w, rel_str, is_dir, kind_str);
                }
            }
        },
        Config::default(),
    )?;

    watcher.watch(Path::new(root), RecursiveMode::Recursive)?;

    // Drain thread: every 200ms, flush entries older than 300ms.
    // Checks shutdown flag to exit cleanly when WatcherHandle is dropped.
    let pending_d = Arc::clone(&pending);
    let shutdown_d = Arc::clone(&shutdown);
    let drain_handle = std::thread::Builder::new()
        .name("watcher-drain".into())
        .spawn(move || {
            let debounce = Duration::from_millis(debounce_ms);
            let poll_interval = Duration::from_millis(50);
            loop {
                std::thread::sleep(poll_interval);
                if shutdown_d.load(Ordering::Acquire) {
                    return;
                }
                let to_emit = drain_expired(&pending_d, debounce);
                for (rel, is_dir, event_kind) in to_emit {
                    let fe = build_file_event(&rel, is_dir, event_kind);
                    if !send_event(&tx, fe, &rel) {
                        return;
                    }
                }
            }
        })
        .map_err(|e| notify::Error::generic(&format!("failed to spawn watcher-drain thread: {}", e)))?;

    Ok(WatcherHandle { _watcher: watcher, shutdown, drain_thread: Some(drain_handle) })
}
