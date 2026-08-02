//! Deterministic frame-boundary event delivery.

/// Events which are written in one frame and observed in the next.
///
/// This is intentionally a small primitive. Domain event routing, persistence,
/// replication and cross-thread queues belong to opt-in higher-level modules.
#[derive(Debug)]
pub struct FrameEvents<E> {
    current: Vec<E>,
    next: Vec<E>,
}

impl<E> FrameEvents<E> {
    /// Creates an empty event buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: Vec::new(),
            next: Vec::new(),
        }
    }

    /// Queues an event to be observed in the following frame.
    pub fn send(&mut self, event: E) {
        self.next.push(event);
    }

    /// Makes queued events visible and clears events from the prior frame.
    pub fn advance_frame(&mut self) {
        self.current.clear();
        std::mem::swap(&mut self.current, &mut self.next);
    }

    /// Iterates over events visible in the current frame.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &E> {
        self.current.iter()
    }

    /// Returns the number of events visible in the current frame.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.current.len()
    }

    /// Returns true if no events are visible in the current frame.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.current.is_empty()
    }
}

impl<E> Default for FrameEvents<E> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FrameEvents;

    #[test]
    fn events_become_visible_on_the_next_frame() {
        let mut events = FrameEvents::new();
        events.send("opened");
        assert!(events.is_empty());

        events.advance_frame();
        assert_eq!(events.iter().copied().collect::<Vec<_>>(), ["opened"]);

        events.advance_frame();
        assert!(events.is_empty());
    }
}
