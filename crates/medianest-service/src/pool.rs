//! A small pool of worker threads whose results can be awaited.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread::JoinHandle;

use futures_channel::oneshot;

type Work = Box<dyn FnOnce() + Send + 'static>;

/// Runs closures on background threads.
#[derive(Debug)]
pub struct Pool {
    sender: std::sync::Mutex<Option<crossbeam_channel::Sender<Work>>>,
    threads: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

/// The pending result of work handed to a [`Pool`]. Await it, or call
/// [`Task::wait`] from a thread that may block.
#[derive(Debug)]
pub struct Task<T> {
    receiver: oneshot::Receiver<T>,
}

impl<T> Task<T> {
    /// Blocks the calling thread until the result is ready.
    pub fn wait(mut self) -> Option<T> {
        let waker =
            std::task::Waker::from(std::sync::Arc::new(ThreadWaker(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        loop {
            match Pin::new(&mut self.receiver).poll(&mut context) {
                Poll::Ready(result) => return result.ok(),
                Poll::Pending => std::thread::park(),
            }
        }
    }
}

struct ThreadWaker(std::thread::Thread);

impl std::task::Wake for ThreadWaker {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.unpark();
    }
}

impl<T> Future for Task<T> {
    /// `None` if the work was dropped because the pool shut down.
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(context).map(Result::ok)
    }
}

impl Pool {
    #[must_use]
    pub fn new(name: &str, size: usize) -> Self {
        let (sender, receiver) = crossbeam_channel::unbounded::<Work>();
        let threads = (0..size.max(1))
            .filter_map(|index| {
                let receiver = receiver.clone();
                std::thread::Builder::new()
                    .name(format!("{name}-{index}"))
                    .spawn(move || {
                        while let Ok(work) = receiver.recv() {
                            work();
                        }
                    })
                    .ok()
            })
            .collect();
        Self {
            sender: std::sync::Mutex::new(Some(sender)),
            threads: std::sync::Mutex::new(threads),
        }
    }

    /// Runs `work` on a pool thread.
    pub fn spawn<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        let (sender, receiver) = oneshot::channel();
        let queue = self
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(queue) = queue {
            let _ = queue.send(Box::new(move || {
                let _ = sender.send(work());
            }));
        }
        Task { receiver }
    }

    /// Stops accepting work and waits for queued work to finish.
    pub fn shutdown(&self) {
        self.sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let current = std::thread::current().id();
        let threads: Vec<_> = self
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect();
        for thread in threads {
            // A pool thread may itself close the library; never join oneself.
            if thread.thread().id() != current {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_work_and_returns_results() {
        let pool = Pool::new("test", 2);
        let tasks: Vec<_> = (0..10).map(|value| pool.spawn(move || value * 2)).collect();
        let results: Vec<i32> = tasks.into_iter().filter_map(Task::wait).collect();
        assert_eq!(results, (0..10).map(|v| v * 2).collect::<Vec<_>>());
    }
}
