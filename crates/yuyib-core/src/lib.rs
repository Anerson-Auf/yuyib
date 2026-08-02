//! Minimal, platform-independent runtime primitives.
//!
//! This crate owns no window, renderer, ECS world, or background executor.
//! Higher-level crates compose those capabilities explicitly.

#![forbid(unsafe_code)]

mod events;
mod time;

pub use events::FrameEvents;
pub use time::{FrameClock, FrameInfo};

/// Lifecycle signal emitted by a runtime host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    /// The host has started its frame loop.
    Started,
    /// A subsystem requested an orderly shutdown.
    ExitRequested,
}

/// Platform-neutral state shared by future application and game hosts.
///
/// A host calls [`Self::begin_frame`] exactly once before it runs its schedules.
/// Events sent during a frame become observable in the next frame. This avoids
/// iterator invalidation and makes event delivery deterministic within a frame.
#[derive(Debug)]
pub struct Runtime {
    clock: FrameClock,
    events: FrameEvents<RuntimeEvent>,
    exit_requested: bool,
}

impl Runtime {
    /// Creates an inert runtime. The first call to [`Self::begin_frame`] emits
    /// [`RuntimeEvent::Started`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: FrameClock::new(),
            events: FrameEvents::new(),
            exit_requested: false,
        }
    }

    /// Starts a new frame and returns its stable timing snapshot.
    pub fn begin_frame(&mut self) -> FrameInfo {
        let info = self.clock.advance();
        if info.index == 0 {
            self.events.send(RuntimeEvent::Started);
        }
        self.events.advance_frame();
        info
    }

    /// Queues an orderly exit request for the following frame.
    pub fn request_exit(&mut self) {
        if !self.exit_requested {
            self.exit_requested = true;
            self.events.send(RuntimeEvent::ExitRequested);
        }
    }

    /// Returns whether orderly shutdown has been requested.
    #[must_use]
    pub const fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Gives read-only access to lifecycle events visible in the current frame.
    #[must_use]
    pub const fn events(&self) -> &FrameEvents<RuntimeEvent> {
        &self.events
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}
