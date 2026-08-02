//! Bounded CPU background tasks without an async runtime or global singleton.
//!
//! `TaskPool` owns a fixed number of worker threads and a bounded submission
//! queue. It is suited to caller-owned CPU work such as parsing, asset
//! preparation, or offline calculation. It is not an async runtime: Tokio or
//! another transport-specific runtime remains an optional integration layer.
//!
//! Task panics are caught and returned as `TaskError::Panic`, so a user job does
//! not terminate its worker. Shutdown closes the queue, drains accepted jobs,
//! and joins every worker deterministically. Drop performs the same blocking
//! operation. There is no cancellation preemption: shutdown can block forever
//! if a user job never returns.

#![forbid(unsafe_code)]

use std::{
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
};

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Fixed bounded worker-pool configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskPoolConfig {
    workers: usize,
    queue_capacity: usize,
}

impl TaskPoolConfig {
    /// Creates a pool configuration with positive worker and queue counts.
    ///
    /// # Errors
    ///
    /// Returns `TaskPoolConfigError` for zero workers or queue capacity.
    pub const fn new(workers: usize, queue_capacity: usize) -> Result<Self, TaskPoolConfigError> {
        if workers == 0 {
            return Err(TaskPoolConfigError::ZeroWorkers);
        }
        if queue_capacity == 0 {
            return Err(TaskPoolConfigError::ZeroQueueCapacity);
        }
        Ok(Self {
            workers,
            queue_capacity,
        })
    }

    /// Returns fixed worker count.
    #[must_use]
    pub const fn workers(self) -> usize {
        self.workers
    }

    /// Returns bounded queued-job capacity.
    #[must_use]
    pub const fn queue_capacity(self) -> usize {
        self.queue_capacity
    }
}

impl Default for TaskPoolConfig {
    fn default() -> Self {
        Self {
            workers: 1,
            queue_capacity: 256,
        }
    }
}

/// Invalid `TaskPoolConfig` input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskPoolConfigError {
    /// At least one worker is required.
    ZeroWorkers,
    /// The task submission queue must have at least one slot.
    ZeroQueueCapacity,
}

impl fmt::Display for TaskPoolConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroWorkers => formatter.write_str("task pool requires at least one worker"),
            Self::ZeroQueueCapacity => {
                formatter.write_str("task pool queue capacity must be non-zero")
            }
        }
    }
}

impl Error for TaskPoolConfigError {}

/// Failure while constructing worker threads.
#[derive(Debug)]
pub enum TaskPoolCreateError {
    /// The operating system rejected a worker-thread creation request.
    WorkerSpawn(std::io::Error),
}

impl fmt::Display for TaskPoolCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerSpawn(error) => write!(formatter, "could not create task worker: {error}"),
        }
    }
}

impl Error for TaskPoolCreateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WorkerSpawn(error) => Some(error),
        }
    }
}

/// Failure while submitting work to a `TaskPool`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskSpawnError {
    /// The bounded queue contains its configured maximum number of waiting jobs.
    Full {
        /// Configured queue capacity.
        capacity: usize,
    },
    /// The pool has closed submission or completed shutdown.
    Closed,
}

impl fmt::Display for TaskSpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { capacity } => {
                write!(formatter, "task queue is full (capacity {capacity})")
            }
            Self::Closed => formatter.write_str("task pool is closed"),
        }
    }
}

impl Error for TaskSpawnError {}

/// Structured task result failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskError {
    /// User code panicked; the worker caught it and remains available.
    Panic,
    /// No result can arrive because a worker ended before result delivery.
    Cancelled,
    /// The task result was already retrieved by `try_take`.
    AlreadyTaken,
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panic => formatter.write_str("task panicked"),
            Self::Cancelled => formatter.write_str("task was cancelled before result delivery"),
            Self::AlreadyTaken => formatter.write_str("task result was already taken"),
        }
    }
}

impl Error for TaskError {}

/// Typed result of one Task.
pub type TaskResult<T> = Result<T, TaskError>;

/// Typed one-shot result handle returned by task submission.
///
/// A Task does not cancel its job when dropped. Dropping only abandons result
/// observation; the worker still executes an accepted job.
pub struct Task<T> {
    receiver: Receiver<TaskResult<T>>,
    taken: bool,
}

impl<T> Task<T> {
    /// Blocks until the task completes or result delivery becomes impossible.
    ///
    /// # Errors
    ///
    /// Returns `TaskError::Panic` for caught user panic, `TaskError::Cancelled`
    /// when no result can arrive, or `TaskError::AlreadyTaken` after `try_take`
    /// already observed the final result.
    pub fn join(mut self) -> TaskResult<T> {
        if self.taken {
            return Err(TaskError::AlreadyTaken);
        }
        self.taken = true;
        self.receiver.recv().unwrap_or(Err(TaskError::Cancelled))
    }

    /// Attempts to take the completed result without blocking.
    ///
    /// A successful result is removed from this handle. Callers can retry after
    /// None; a later call after Some returns `TaskError::AlreadyTaken`.
    ///
    /// # Errors
    ///
    /// Returns `TaskError::AlreadyTaken` after a result was consumed. User panic
    /// and cancellation are represented inside the returned `TaskResult`.
    pub fn try_take(&mut self) -> Result<Option<TaskResult<T>>, TaskError> {
        if self.taken {
            return Err(TaskError::AlreadyTaken);
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                self.taken = true;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.taken = true;
                Ok(Some(Err(TaskError::Cancelled)))
            }
        }
    }
}

/// Failure while closing and joining a `TaskPool`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskPoolShutdownError {
    /// A worker panicked outside the protected user-job boundary.
    WorkerPanic,
}

impl fmt::Display for TaskPoolShutdownError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerPanic => formatter.write_str("task worker panicked during shutdown"),
        }
    }
}

impl Error for TaskPoolShutdownError {}

/// Fixed-worker, bounded-queue CPU task pool.
///
/// Closing submission drops the sole sender and lets workers drain accepted
/// jobs before exiting. Shutdown and Drop join workers, so they block until all
/// accepted user jobs finish. There is no forceful task cancellation.
pub struct TaskPool {
    sender: Option<SyncSender<Job>>,
    workers: Vec<JoinHandle<()>>,
    config: TaskPoolConfig,
}

impl TaskPool {
    /// Starts a fixed number of worker threads for this explicitly owned pool.
    ///
    /// # Errors
    ///
    /// Returns `TaskPoolCreateError` if the operating system rejects worker creation.
    pub fn new(config: TaskPoolConfig) -> Result<Self, TaskPoolCreateError> {
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers: Vec<JoinHandle<()>> = Vec::with_capacity(config.workers);
        for worker_index in 0..config.workers {
            let receiver = Arc::clone(&receiver);
            let worker = match thread::Builder::new()
                .name(format!("yuyib-task-{worker_index}"))
                .spawn(move || worker_loop(&receiver))
            {
                Ok(worker) => worker,
                Err(error) => {
                    drop(sender);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(TaskPoolCreateError::WorkerSpawn(error));
                }
            };
            workers.push(worker);
        }
        Ok(Self {
            sender: Some(sender),
            workers,
            config,
        })
    }

    /// Returns the immutable bounded configuration.
    #[must_use]
    pub const fn config(&self) -> TaskPoolConfig {
        self.config
    }

    /// Returns whether task submission has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_none()
    }

    /// Submits one typed task, blocking only while the bounded queue is full.
    ///
    /// This does not run an async executor. Do not hold application locks while
    /// calling this method, because a full queue waits for a worker to dequeue.
    ///
    /// # Errors
    ///
    /// Returns `TaskSpawnError::Closed` when shutdown has begun.
    pub fn spawn<T, F>(&self, task: F) -> Result<Task<T>, TaskSpawnError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let sender = self.sender.as_ref().ok_or(TaskSpawnError::Closed)?;
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        sender
            .send(task_job(task, result_sender))
            .map_err(|_| TaskSpawnError::Closed)?;
        Ok(Task {
            receiver: result_receiver,
            taken: false,
        })
    }

    /// Submits one typed task without waiting for queue capacity.
    ///
    /// # Errors
    ///
    /// Returns `TaskSpawnError::Full` when all queue slots are occupied, or
    /// `TaskSpawnError::Closed` after close or shutdown.
    pub fn try_spawn<T, F>(&self, task: F) -> Result<Task<T>, TaskSpawnError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let sender = self.sender.as_ref().ok_or(TaskSpawnError::Closed)?;
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        match sender.try_send(task_job(task, result_sender)) {
            Ok(()) => Ok(Task {
                receiver: result_receiver,
                taken: false,
            }),
            Err(TrySendError::Full(_)) => Err(TaskSpawnError::Full {
                capacity: self.config.queue_capacity,
            }),
            Err(TrySendError::Disconnected(_)) => Err(TaskSpawnError::Closed),
        }
    }

    /// Closes submission and lets workers finish already accepted work.
    ///
    /// Returns true only for the first close call. Use shutdown to close and
    /// then block until every worker has exited.
    pub fn close(&mut self) -> bool {
        self.sender.take().is_some()
    }

    /// Closes submission, drains accepted work, and joins every worker.
    ///
    /// This blocks until user jobs return. It cannot preempt an executing job.
    ///
    /// # Errors
    ///
    /// Returns `TaskPoolShutdownError` if a worker panicked outside protected job execution.
    pub fn shutdown(mut self) -> Result<(), TaskPoolShutdownError> {
        self.close();
        self.join_workers()
    }

    fn join_workers(&mut self) -> Result<(), TaskPoolShutdownError> {
        let mut failure = None;
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                failure.get_or_insert(TaskPoolShutdownError::WorkerPanic);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for TaskPool {
    fn drop(&mut self) {
        self.close();
        let _ = self.join_workers();
    }
}

fn task_job<T, F>(task: F, sender: SyncSender<TaskResult<T>>) -> Job
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    Box::new(move || {
        let result = catch_unwind(AssertUnwindSafe(task)).map_err(|_| TaskError::Panic);
        let _ = sender.send(result);
    })
}

fn worker_loop(receiver: &Arc<Mutex<Receiver<Job>>>) {
    loop {
        let result = {
            let receiver = receiver
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            receiver.recv()
        };
        let Ok(job) = result else {
            return;
        };
        let _ = catch_unwind(AssertUnwindSafe(job));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, mpsc};

    use super::*;

    fn pool(workers: usize, queue_capacity: usize) -> TaskPool {
        TaskPool::new(TaskPoolConfig::new(workers, queue_capacity).expect("valid pool config"))
            .expect("worker threads must start")
    }

    #[test]
    fn configuration_is_validated() {
        assert_eq!(
            TaskPoolConfig::new(0, 1),
            Err(TaskPoolConfigError::ZeroWorkers)
        );
        assert_eq!(
            TaskPoolConfig::new(1, 0),
            Err(TaskPoolConfigError::ZeroQueueCapacity)
        );
    }

    #[test]
    fn try_spawn_reports_full_queue_without_sleeping() {
        let pool = pool(1, 1);
        let gate = Arc::new(Barrier::new(2));
        let (started_sender, started_receiver) = mpsc::channel();
        let task_gate = Arc::clone(&gate);
        let first = pool
            .spawn(move || {
                started_sender.send(()).expect("test observer must exist");
                task_gate.wait();
                1
            })
            .expect("first task must submit");
        started_receiver
            .recv()
            .expect("worker must start first task");
        let second = pool.try_spawn(|| 2).expect("one queue slot must fit");
        assert!(matches!(
            pool.try_spawn(|| 3),
            Err(TaskSpawnError::Full { capacity: 1 })
        ));
        gate.wait();
        assert_eq!(first.join(), Ok(1));
        assert_eq!(second.join(), Ok(2));
        pool.shutdown().expect("worker must shut down");
    }

    #[test]
    fn panic_is_structured_and_worker_continues() {
        let pool = pool(1, 2);
        let panic_task = pool
            .spawn(|| -> usize { panic!("test user panic") })
            .expect("panic task must submit");
        let next = pool.spawn(|| 7).expect("next task must submit");

        assert_eq!(panic_task.join(), Err(TaskError::Panic));
        assert_eq!(next.join(), Ok(7));
        pool.shutdown().expect("worker must remain alive");
    }

    #[test]
    fn try_take_is_nonblocking_and_consumes_result_once() {
        let pool = pool(1, 1);
        let gate = Arc::new(Barrier::new(2));
        let (started_sender, started_receiver) = mpsc::channel();
        let task_gate = Arc::clone(&gate);
        let mut task = pool
            .spawn(move || {
                started_sender.send(()).expect("test observer must exist");
                task_gate.wait();
                9
            })
            .expect("task must submit");
        started_receiver.recv().expect("worker must start task");
        assert_eq!(task.try_take(), Ok(None));
        gate.wait();

        let result = loop {
            match task.try_take() {
                Ok(Some(result)) => break result,
                Ok(None) => std::thread::yield_now(),
                Err(error) => panic!("unexpected task error: {error}"),
            }
        };
        assert_eq!(result, Ok(9));
        assert_eq!(task.try_take(), Err(TaskError::AlreadyTaken));
        pool.shutdown().expect("worker must shut down");
    }

    #[test]
    fn close_rejects_new_work_and_shutdown_drains_accepted_work() {
        let mut pool = pool(1, 1);
        let task = pool.spawn(|| 11).expect("task must submit");
        assert!(pool.close());
        assert!(!pool.close());
        assert!(pool.is_closed());
        assert!(matches!(pool.try_spawn(|| 12), Err(TaskSpawnError::Closed)));
        assert_eq!(task.join(), Ok(11));
        pool.shutdown()
            .expect("accepted work must drain before join");
    }
}
