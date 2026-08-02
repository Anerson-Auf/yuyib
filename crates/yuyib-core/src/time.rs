//! Frame timing with no dependency on a windowing or rendering backend.

use std::time::{Duration, Instant};

/// Immutable timing snapshot for one runtime frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameInfo {
    /// Zero-based frame number.
    pub index: u64,
    /// Time elapsed since the previous frame. The first frame has zero delta.
    pub delta: Duration,
    /// Time elapsed since the first call to [`FrameClock::advance`].
    pub elapsed: Duration,
}

/// Produces frame timing snapshots from [`Instant`].
#[derive(Debug)]
pub struct FrameClock {
    started_at: Option<Instant>,
    previous_at: Option<Instant>,
    index: u64,
}

impl FrameClock {
    /// Creates a clock that starts with its first frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started_at: None,
            previous_at: None,
            index: 0,
        }
    }

    /// Records the next frame using the current monotonic instant.
    pub fn advance(&mut self) -> FrameInfo {
        let now = Instant::now();
        let started_at = *self.started_at.get_or_insert(now);
        let previous_at = self.previous_at.replace(now);
        let info = FrameInfo {
            index: self.index,
            delta: previous_at.map_or(Duration::ZERO, |previous| now - previous),
            elapsed: now - started_at,
        };
        self.index = self.index.saturating_add(1);
        info
    }
}

impl Default for FrameClock {
    fn default() -> Self {
        Self::new()
    }
}
