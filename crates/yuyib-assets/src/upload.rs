//! Bounded main-thread publication for device-bound asset work.

use std::{array, collections::VecDeque, error::Error, fmt};

/// Importance used to order pending asset uploads.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AssetUploadPriority {
    /// The application cannot enter its ready state without this resource.
    Required,
    /// Visible or near-camera content.
    NearCamera,
    /// Explicit gameplay prefetch for an approaching zone or transition.
    Prefetch,
    /// Opportunistic work which must not delay visible content.
    #[default]
    Background,
}

impl AssetUploadPriority {
    const fn queue_index(self) -> usize {
        match self {
            Self::Required => 0,
            Self::NearCamera => 1,
            Self::Prefetch => 2,
            Self::Background => 3,
        }
    }
}

/// Limits retained device-bound work before it reaches the main thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetUploadQueueConfig {
    /// Maximum jobs retained across all priorities.
    pub max_pending_jobs: usize,
}

impl AssetUploadQueueConfig {
    /// Creates a bounded queue policy.
    ///
    /// # Errors
    ///
    /// Returns an error when `max_pending_jobs` is zero.
    pub const fn new(max_pending_jobs: usize) -> Result<Self, AssetUploadQueueConfigError> {
        if max_pending_jobs == 0 {
            return Err(AssetUploadQueueConfigError::ZeroPendingJobs);
        }
        Ok(Self { max_pending_jobs })
    }
}

impl Default for AssetUploadQueueConfig {
    fn default() -> Self {
        Self {
            max_pending_jobs: 1_024,
        }
    }
}

/// Invalid upload queue configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetUploadQueueConfigError {
    /// An always-full queue cannot accept work.
    ZeroPendingJobs,
}

impl fmt::Display for AssetUploadQueueConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("asset upload queue must retain at least one pending job")
    }
}

impl Error for AssetUploadQueueConfigError {}

/// Per-frame budget for main-thread device work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetUploadBudget {
    /// Maximum declared upload bytes processed in this frame.
    pub max_bytes: u64,
    /// Maximum jobs processed in this frame.
    pub max_jobs: usize,
}

impl AssetUploadBudget {
    /// Creates a non-zero per-frame budget.
    ///
    /// # Errors
    ///
    /// Returns an error when either limit is zero.
    pub const fn new(max_bytes: u64, max_jobs: usize) -> Result<Self, AssetUploadBudgetError> {
        if max_bytes == 0 {
            return Err(AssetUploadBudgetError::ZeroBytes);
        }
        if max_jobs == 0 {
            return Err(AssetUploadBudgetError::ZeroJobs);
        }
        Ok(Self {
            max_bytes,
            max_jobs,
        })
    }
}

/// Invalid per-frame upload budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetUploadBudgetError {
    /// A zero byte budget cannot upload a resource.
    ZeroBytes,
    /// A zero job budget cannot make progress.
    ZeroJobs,
}

impl fmt::Display for AssetUploadBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroBytes => formatter.write_str("asset upload byte budget must be non-zero"),
            Self::ZeroJobs => formatter.write_str("asset upload job budget must be non-zero"),
        }
    }
}

impl Error for AssetUploadBudgetError {}

/// Stable identifier for one queued main-thread upload.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssetUploadId(u64);

type UploadJob<Context, Output, Failure> =
    Box<dyn FnOnce(&mut Context) -> Result<Output, Failure> + Send + 'static>;

struct PendingUpload<Context, Output, Failure> {
    id: AssetUploadId,
    label: String,
    bytes: u64,
    upload: UploadJob<Context, Output, Failure>,
}

/// Result produced by one device-bound upload job.
#[derive(Debug)]
pub struct AssetUploadResult<Output, Failure> {
    /// Queue identifier assigned at submission.
    pub id: AssetUploadId,
    /// Diagnostic label supplied by the caller.
    pub label: String,
    /// Declared bytes charged against the frame budget.
    pub bytes: u64,
    /// Device-bound output or structured upload failure.
    pub result: Result<Output, Failure>,
}

/// Summary of one bounded queue drain.
#[derive(Debug)]
pub struct AssetUploadUpdate<Output, Failure> {
    /// Jobs executed during this frame.
    pub results: Vec<AssetUploadResult<Output, Failure>>,
    /// Declared upload bytes consumed in this frame.
    pub uploaded_bytes: u64,
    /// Jobs still waiting after this frame.
    pub remaining_jobs: usize,
    /// Highest-priority job could not fit in the remaining byte budget.
    pub blocked_by_byte_budget: bool,
}

/// Submitting a device-bound job failed without retaining its closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetUploadSubmitError {
    /// The bounded pending queue is full.
    Full,
    /// The monotonic upload identifier space was exhausted.
    IdentifierExhausted,
}

impl fmt::Display for AssetUploadSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => formatter.write_str("asset upload queue is full"),
            Self::IdentifierExhausted => formatter.write_str("asset upload identifier exhausted"),
        }
    }
}

impl Error for AssetUploadSubmitError {}

/// Priority-ordered queue for GPU/device resource creation on the main thread.
///
/// `Context` is supplied only to [`Self::process`], so queued closures cannot
/// retain a renderer/device borrow. Work is FIFO within one priority. If the
/// highest-priority job does not fit the remaining byte budget, lower-priority
/// work is not allowed to bypass it.
pub struct AssetUploadQueue<Context, Output, Failure> {
    config: AssetUploadQueueConfig,
    next_id: u64,
    pending: usize,
    queues: [VecDeque<PendingUpload<Context, Output, Failure>>; 4],
}

impl<Context, Output, Failure> AssetUploadQueue<Context, Output, Failure> {
    /// Creates an empty bounded upload queue.
    #[must_use]
    pub fn new(config: AssetUploadQueueConfig) -> Self {
        Self {
            config,
            next_id: 0,
            pending: 0,
            queues: array::from_fn(|_| VecDeque::new()),
        }
    }

    /// Queues one main-thread operation without waiting for capacity.
    ///
    /// # Errors
    ///
    /// Returns `Full` before retaining `upload`, or `IdentifierExhausted` after
    /// all representable monotonic identifiers have been assigned.
    pub fn try_enqueue<Upload>(
        &mut self,
        priority: AssetUploadPriority,
        label: impl Into<String>,
        bytes: u64,
        upload: Upload,
    ) -> Result<AssetUploadId, AssetUploadSubmitError>
    where
        Upload: FnOnce(&mut Context) -> Result<Output, Failure> + Send + 'static,
    {
        if self.pending >= self.config.max_pending_jobs {
            return Err(AssetUploadSubmitError::Full);
        }
        let id = AssetUploadId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(AssetUploadSubmitError::IdentifierExhausted)?;
        self.queues[priority.queue_index()].push_back(PendingUpload {
            id,
            label: label.into(),
            bytes,
            upload: Box::new(upload),
        });
        self.pending += 1;
        Ok(id)
    }

    /// Executes a priority-ordered prefix within the supplied frame budget.
    pub fn process(
        &mut self,
        context: &mut Context,
        budget: AssetUploadBudget,
    ) -> AssetUploadUpdate<Output, Failure> {
        let mut results = Vec::new();
        let mut uploaded_bytes = 0_u64;
        let mut blocked_by_byte_budget = false;

        'priorities: for queue in &mut self.queues {
            while results.len() < budget.max_jobs {
                let Some(next) = queue.front() else {
                    break;
                };
                let remaining_bytes = budget.max_bytes.saturating_sub(uploaded_bytes);
                if next.bytes > remaining_bytes {
                    blocked_by_byte_budget = true;
                    break 'priorities;
                }
                let Some(next) = queue.pop_front() else {
                    break;
                };
                uploaded_bytes = uploaded_bytes.saturating_add(next.bytes);
                self.pending -= 1;
                results.push(AssetUploadResult {
                    id: next.id,
                    label: next.label,
                    bytes: next.bytes,
                    result: (next.upload)(context),
                });
            }
            if results.len() >= budget.max_jobs {
                break;
            }
        }

        AssetUploadUpdate {
            results,
            uploaded_bytes,
            remaining_jobs: self.pending,
            blocked_by_byte_budget,
        }
    }

    /// Returns the number of retained jobs across all priorities.
    #[must_use]
    pub const fn pending_jobs(&self) -> usize {
        self.pending
    }
}

impl<Context, Output, Failure> Default for AssetUploadQueue<Context, Output, Failure> {
    fn default() -> Self {
        Self::new(AssetUploadQueueConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_fifo_and_frame_budgets_are_observable() {
        let mut queue = AssetUploadQueue::<Vec<&'static str>, &'static str, ()>::new(
            AssetUploadQueueConfig::new(4).expect("valid queue"),
        );
        queue
            .try_enqueue(AssetUploadPriority::Background, "background", 2, |log| {
                log.push("background");
                Ok("background")
            })
            .expect("space");
        queue
            .try_enqueue(AssetUploadPriority::Required, "required-a", 3, |log| {
                log.push("required-a");
                Ok("required-a")
            })
            .expect("space");
        queue
            .try_enqueue(AssetUploadPriority::Required, "required-b", 4, |log| {
                log.push("required-b");
                Ok("required-b")
            })
            .expect("space");

        let mut log = Vec::new();
        let first = queue.process(
            &mut log,
            AssetUploadBudget::new(5, 3).expect("valid budget"),
        );
        assert_eq!(log, ["required-a"]);
        assert_eq!(first.uploaded_bytes, 3);
        assert_eq!(first.remaining_jobs, 2);
        assert!(first.blocked_by_byte_budget);

        let second = queue.process(
            &mut log,
            AssetUploadBudget::new(8, 2).expect("valid budget"),
        );
        assert_eq!(log, ["required-a", "required-b", "background"]);
        assert_eq!(second.uploaded_bytes, 6);
        assert_eq!(second.remaining_jobs, 0);
    }

    #[test]
    fn full_queue_rejects_without_executing_or_retaining_job() {
        let mut queue = AssetUploadQueue::<(), (), ()>::new(
            AssetUploadQueueConfig::new(1).expect("valid queue"),
        );
        queue
            .try_enqueue(AssetUploadPriority::Required, "first", 1, |()| Ok(()))
            .expect("first fits");
        assert_eq!(
            queue.try_enqueue(AssetUploadPriority::Required, "second", 1, |()| Ok(())),
            Err(AssetUploadSubmitError::Full)
        );
        assert_eq!(queue.pending_jobs(), 1);
    }
}
