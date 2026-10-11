//! Long-running work with progress, pause and cancel.
//!
//! Jobs run in two lanes. The foreground lane carries what people ask for:
//! imports, scans, verification, exports. The background lane carries
//! maintenance, which steps aside while foreground work is running.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use medianest_catalog::{JobState as HistoryState, Writer};
use medianest_core::now_ms;
use medianest_vault::{Control, Progress};
use serde::Serialize;

use crate::events::{Event, EventBus};
use crate::{Error, Result};

/// Identifies a job for the lifetime of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct JobId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    #[must_use]
    pub const fn is_finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Lane {
    Foreground,
    Background,
}

/// A point-in-time view of a job, safe to show in the interface.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JobSnapshot {
    pub id: JobId,
    /// Stable machine name, such as `import` or `verify`.
    pub kind: &'static str,
    /// What the job does, for people: "Importing 240 photos".
    pub title: String,
    pub lane: Lane,
    pub state: JobState,
    pub done: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// The item being worked on.
    pub current: String,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    /// Outcome in one sentence, once finished.
    pub summary: String,
    pub error: Option<String>,
}

impl JobSnapshot {
    /// Fraction complete, when the amount of work is known.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        if self.bytes_total > 0 {
            Some((self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0))
        } else if self.total > 0 {
            Some((self.done as f64 / self.total as f64).clamp(0.0, 1.0))
        } else {
            None
        }
    }

    /// Estimated milliseconds remaining, once enough progress was made to judge.
    #[must_use]
    pub fn remaining_ms(&self) -> Option<i64> {
        let fraction = self.fraction()?;
        if !(0.02..1.0).contains(&fraction) || self.state != JobState::Running {
            return None;
        }
        let elapsed = now_ms() - self.started_ms;
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        Some(((elapsed as f64) * (1.0 - fraction) / fraction) as i64)
    }
}

/// Progress shared between a job and the callbacks of the operations it runs.
struct ProgressState {
    snapshot: Arc<Mutex<JobSnapshot>>,
    events: Arc<EventBus>,
    last_emit: Mutex<Instant>,
}

impl ProgressState {
    fn emit(&self, force: bool) {
        let mut last = lock(&self.last_emit);
        if force || last.elapsed() >= Duration::from_millis(100) {
            *last = Instant::now();
            drop(last);
            let snapshot = lock(&self.snapshot).clone();
            self.events.emit(Event::Job(snapshot));
        }
    }

    fn update(&self, force: bool, change: impl FnOnce(&mut JobSnapshot)) {
        change(&mut lock(&self.snapshot));
        self.emit(force);
    }
}

/// Handed to running jobs for reporting progress and checking for cancellation.
pub struct JobContext {
    progress: Arc<ProgressState>,
    control: Control,
}

impl std::fmt::Debug for JobContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobContext").finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl JobContext {
    /// The cancellation, pause and progress handle to pass to vault operations.
    #[must_use]
    pub fn control(&self) -> &Control {
        &self.control
    }

    /// Fails with [`Error::Cancelled`] if the job was cancelled; waits while paused.
    pub fn checkpoint(&self) -> Result<()> {
        self.control.checkpoint().map_err(|_| Error::Cancelled)
    }

    pub fn set_total(&self, total: u64) {
        self.progress
            .update(true, |snapshot| snapshot.total = total);
    }

    pub fn set_bytes_total(&self, bytes: u64) {
        self.progress
            .update(true, |snapshot| snapshot.bytes_total = bytes);
    }

    pub fn set_title(&self, title: impl Into<String>) {
        let title = title.into();
        self.progress
            .update(true, |snapshot| snapshot.title = title);
    }

    /// One more item finished.
    pub fn advance(&self) {
        self.progress.update(false, |snapshot| snapshot.done += 1);
    }

    pub fn set_current(&self, label: impl Into<String>) {
        let label = label.into();
        self.progress
            .update(false, |snapshot| snapshot.current = label);
    }
}

type JobFn = Box<dyn FnOnce(&JobContext) -> Result<String> + Send>;

struct Queued {
    id: JobId,
    run: JobFn,
}

struct LaneQueue {
    items: Mutex<VecDeque<Queued>>,
    ready: Condvar,
}

/// A known job: its live snapshot, its control handle, and whether the user paused it.
type Entry = (Arc<Mutex<JobSnapshot>>, Control, bool);

struct Shared {
    events: Arc<EventBus>,
    writer: Arc<Mutex<Writer>>,
    jobs: Mutex<Vec<Entry>>,
    lanes: [LaneQueue; 2],
    foreground_busy: AtomicUsize,
    stopping: std::sync::atomic::AtomicBool,
    next_id: AtomicU64,
}

/// Queues and runs jobs.
pub struct JobManager {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for JobManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobManager").finish_non_exhaustive()
    }
}

impl JobManager {
    pub(crate) fn new(events: Arc<EventBus>, writer: Arc<Mutex<Writer>>) -> Self {
        let shared = Arc::new(Shared {
            events,
            writer,
            jobs: Mutex::new(Vec::new()),
            lanes: [
                LaneQueue {
                    items: Mutex::new(VecDeque::new()),
                    ready: Condvar::new(),
                },
                LaneQueue {
                    items: Mutex::new(VecDeque::new()),
                    ready: Condvar::new(),
                },
            ],
            foreground_busy: AtomicUsize::new(0),
            stopping: std::sync::atomic::AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        });
        let threads = [Lane::Foreground, Lane::Background]
            .into_iter()
            .filter_map(|lane| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("jobs-{lane:?}").to_lowercase())
                    .spawn(move || run_lane(&shared, lane))
                    .ok()
            })
            .collect();
        Self {
            shared,
            threads: Mutex::new(threads),
        }
    }

    /// Queues a job. `run` returns a one-sentence summary on success.
    pub fn submit(
        &self,
        kind: &'static str,
        title: impl Into<String>,
        lane: Lane,
        run: impl FnOnce(&JobContext) -> Result<String> + Send + 'static,
    ) -> JobId {
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let snapshot = JobSnapshot {
            id,
            kind,
            title: title.into(),
            lane,
            state: JobState::Queued,
            done: 0,
            total: 0,
            bytes_done: 0,
            bytes_total: 0,
            current: String::new(),
            started_ms: now_ms(),
            finished_ms: None,
            summary: String::new(),
            error: None,
        };
        {
            let mut jobs = lock(&self.shared.jobs);
            jobs.push((
                Arc::new(Mutex::new(snapshot.clone())),
                Control::new(),
                false,
            ));
            // Keep the history of finished jobs short.
            let finished: Vec<usize> = jobs
                .iter()
                .enumerate()
                .filter(|(_, (job, _, _))| lock(job).state.is_finished())
                .map(|(index, _)| index)
                .collect();
            if finished.len() > 30 {
                for index in finished[..finished.len() - 30].iter().rev() {
                    jobs.remove(*index);
                }
            }
        }
        self.shared.events.emit(Event::Job(snapshot));
        let queue = &self.shared.lanes[lane_index(lane)];
        lock(&queue.items).push_back(Queued {
            id,
            run: Box::new(run),
        });
        queue.ready.notify_one();
        id
    }

    fn find(&self, id: JobId) -> Option<(Arc<Mutex<JobSnapshot>>, Control)> {
        lock(&self.shared.jobs)
            .iter()
            .find(|(job, _, _)| lock(job).id == id)
            .map(|(job, control, _)| (Arc::clone(job), control.clone()))
    }

    /// Asks a job to stop at the next safe point.
    pub fn cancel(&self, id: JobId) {
        if let Some((_, control)) = self.find(id) {
            control.cancel();
        }
    }

    pub fn set_paused(&self, id: JobId, paused: bool) {
        let snapshot = {
            let mut jobs = lock(&self.shared.jobs);
            let Some((job, control, user_paused)) =
                jobs.iter_mut().find(|(job, _, _)| lock(job).id == id)
            else {
                return;
            };
            *user_paused = paused;
            let lane = lock(job).lane;
            control.set_paused(paused || background_should_yield(&self.shared, lane));
            let mut snapshot = lock(job);
            if snapshot.state == JobState::Running || snapshot.state == JobState::Paused {
                snapshot.state = if paused {
                    JobState::Paused
                } else {
                    JobState::Running
                };
            }
            snapshot.clone()
        };
        self.shared.events.emit(Event::Job(snapshot));
    }

    /// Current and recently finished jobs, oldest first.
    #[must_use]
    pub fn snapshots(&self) -> Vec<JobSnapshot> {
        lock(&self.shared.jobs)
            .iter()
            .map(|(job, _, _)| lock(job).clone())
            .collect()
    }

    #[must_use]
    pub fn snapshot(&self, id: JobId) -> Option<JobSnapshot> {
        self.find(id).map(|(job, _)| lock(&job).clone())
    }

    /// Whether any job is queued or running.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.snapshots().iter().any(|job| !job.state.is_finished())
    }

    /// Cancels everything and waits for the lanes to stop.
    pub(crate) fn shutdown(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        for (_, control, _) in lock(&self.shared.jobs).iter() {
            control.cancel();
        }
        for lane in &self.shared.lanes {
            lane.ready.notify_all();
        }
        let threads: Vec<_> = lock(&self.threads).drain(..).collect();
        for thread in threads {
            let _ = thread.join();
        }
    }
}

impl Drop for JobManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

const fn lane_index(lane: Lane) -> usize {
    match lane {
        Lane::Foreground => 0,
        Lane::Background => 1,
    }
}

fn background_should_yield(shared: &Shared, lane: Lane) -> bool {
    lane == Lane::Background && shared.foreground_busy.load(Ordering::SeqCst) > 0
}

/// Pauses or resumes background jobs as foreground work starts and ends.
fn update_yielding(shared: &Shared) {
    for (job, control, user_paused) in lock(&shared.jobs).iter() {
        let lane = lock(job).lane;
        if lane == Lane::Background {
            control.set_paused(*user_paused || background_should_yield(shared, lane));
        }
    }
}

fn run_lane(shared: &Arc<Shared>, lane: Lane) {
    let queue = &shared.lanes[lane_index(lane)];
    loop {
        let next = {
            let mut items = lock(&queue.items);
            loop {
                if shared.stopping.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(item) = items.pop_front() {
                    break item;
                }
                items = queue
                    .ready
                    .wait(items)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some((snapshot, control)) = lock(&shared.jobs)
            .iter()
            .find(|(job, _, _)| lock(job).id == next.id)
            .map(|(job, control, _)| (Arc::clone(job), control.clone()))
        else {
            continue;
        };
        if lane == Lane::Foreground {
            shared.foreground_busy.fetch_add(1, Ordering::SeqCst);
            update_yielding(shared);
        } else {
            control.set_paused(background_should_yield(shared, lane));
        }
        let (kind, title) = {
            let mut snapshot = lock(&snapshot);
            snapshot.state = JobState::Running;
            snapshot.started_ms = now_ms();
            (snapshot.kind, snapshot.title.clone())
        };
        let history = (lane == Lane::Foreground)
            .then(|| lock(&shared.writer).start_job(kind, &title).ok())
            .flatten();
        let progress = Arc::new(ProgressState {
            snapshot: Arc::clone(&snapshot),
            events: Arc::clone(&shared.events),
            last_emit: Mutex::new(Instant::now()),
        });
        let sink = Arc::clone(&progress);
        let context = JobContext {
            progress: Arc::clone(&progress),
            control: control.clone().with_progress(move |step| match step {
                Progress::Bytes(bytes) => {
                    sink.update(false, |snapshot| snapshot.bytes_done += bytes)
                }
                Progress::Item { label } => sink.update(false, |snapshot| snapshot.current = label),
                Progress::Done => {}
            }),
        };
        tracing::info!(job = next.id.0, kind, %title, "job started");
        progress.emit(true);
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (next.run)(&context)))
                .unwrap_or_else(|_| Err(Error::Invalid("the job stopped unexpectedly".into())));
        let finished = {
            let mut snapshot = lock(&snapshot);
            snapshot.finished_ms = Some(now_ms());
            snapshot.current.clear();
            match &outcome {
                Ok(summary) => {
                    snapshot.state = JobState::Completed;
                    snapshot.summary.clone_from(summary);
                }
                Err(error) if error.is_cancelled() || control.is_cancelled() => {
                    snapshot.state = JobState::Cancelled;
                    snapshot.summary = "Stopped".into();
                }
                Err(error) => {
                    snapshot.state = JobState::Failed;
                    snapshot.error = Some(error.to_string());
                    snapshot.summary = error.to_string();
                }
            }
            snapshot.clone()
        };
        match &outcome {
            Ok(_) => {
                tracing::info!(job = next.id.0, kind, summary = %finished.summary, "job finished")
            }
            Err(error) => tracing::warn!(job = next.id.0, kind, %error, "job did not finish"),
        }
        if let Some(history) = history {
            let state = match finished.state {
                JobState::Completed => HistoryState::Completed,
                JobState::Cancelled => HistoryState::Cancelled,
                _ => HistoryState::Failed,
            };
            let _ = lock(&shared.writer).finish_job(history, state, &finished.summary);
        }
        if lane == Lane::Foreground {
            shared.foreground_busy.fetch_sub(1, Ordering::SeqCst);
            update_yielding(shared);
        }
        shared.events.emit(Event::Job(finished));
    }
}
