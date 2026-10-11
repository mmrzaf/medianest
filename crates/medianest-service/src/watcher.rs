//! Watching linked folders for new and changed files.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use medianest_core::{SourceId, SourceKind, SourceStatus};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::jobs::Lane;
use crate::{Inner, Library};

/// Changes are collected until the folders have been quiet this long.
const QUIET: Duration = Duration::from_secs(4);

pub(crate) struct Watcher {
    _watcher: RecommendedWatcher,
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

fn debounce(
    library: &Weak<Inner>,
    roots: &[(SourceId, PathBuf)],
    changes: &Receiver<PathBuf>,
    stop: &Receiver<()>,
) {
    let mut pending: HashSet<SourceId> = HashSet::new();
    let mut last_change = Instant::now();
    loop {
        if stop.try_recv().is_ok() {
            return;
        }
        match changes.recv_timeout(Duration::from_millis(500)) {
            Ok(path) => {
                if let Some((source, _)) = roots.iter().find(|(_, root)| path.starts_with(root)) {
                    pending.insert(*source);
                    last_change = Instant::now();
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !pending.is_empty() && last_change.elapsed() >= QUIET {
            let Some(inner) = library.upgrade() else {
                return;
            };
            let library = Library { inner };
            if library.is_closed() {
                return;
            }
            let busy = library
                .jobs()
                .iter()
                .any(|job| job.kind == "scan" && !job.state.is_finished());
            if !busy {
                for source in pending.drain() {
                    tracing::debug!(source = %source, "folder changed; rescanning");
                    library.scan(Some(source), Lane::Background);
                }
            }
        }
    }
}

impl Library {
    /// Starts or restarts watching linked folders, per the settings.
    pub(crate) fn start_watching(&self) {
        let mut slot = self
            .inner
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.take();
        if !self.settings().watch_folders || self.is_closed() {
            return;
        }
        let roots: Vec<(SourceId, PathBuf)> = self
            .sources()
            .unwrap_or_default()
            .into_iter()
            .filter(|source| {
                source.kind == SourceKind::Linked
                    && source.watch
                    && source.status == SourceStatus::Available
            })
            .map(|source| (source.id, source.path.to_path_buf()))
            .filter(|(_, path)| path.is_dir())
            .collect();
        if roots.is_empty() {
            return;
        }
        let (changes_tx, changes) = channel::<PathBuf>();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                if matches!(event.kind, notify::EventKind::Access(_)) {
                    return;
                }
                for path in event.paths {
                    let _ = changes_tx.send(path);
                }
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                tracing::warn!(%error, "folder watching is unavailable");
                return;
            }
        };
        for (_, root) in &roots {
            if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                // Usually the inotify watch limit; rescans still find changes.
                tracing::warn!(folder = %root.display(), %error, "could not watch folder");
            }
        }
        let (stop, stop_rx) = channel();
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let thread = std::thread::Builder::new()
            .name("folder-watch".into())
            .spawn(move || debounce(&weak, &roots, &changes, &stop_rx))
            .ok();
        *slot = Some(Watcher {
            _watcher: watcher,
            stop,
            thread,
        });
    }
}
