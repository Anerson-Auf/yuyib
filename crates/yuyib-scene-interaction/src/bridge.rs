//! Bridge trait and batch results.

use crate::{
    capabilities::BridgeCapabilities,
    intent::SceneInteractionIntent,
    signal::SceneInteractionSignal,
};

/// Outcome of applying one or more intents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneInteractionBatchResult {
    /// Number of intents in the submitted batch.
    pub submitted: usize,
    /// Intents that mutated state or queued a signal (skips identical field writes).
    pub applied: usize,
    /// Signals produced by `EmitSignal` (host must drain).
    pub signals: Vec<SceneInteractionSignal>,
}

impl SceneInteractionBatchResult {
    /// Empty successful batch.
    #[must_use]
    pub const fn empty(submitted: usize) -> Self {
        Self {
            submitted,
            applied: 0,
            signals: Vec::new(),
        }
    }
}

/// Adapter that applies intents in one host context (Editor document or Play World).
pub trait SceneInteractionBridge {
    /// Adapter-specific failure.
    type Error;

    /// Declares which intents this adapter accepts.
    fn capabilities(&self) -> BridgeCapabilities;

    /// Applies a single intent.
    ///
    /// # Errors
    ///
    /// Returns when the entity/schema cannot be resolved or the mutation is rejected.
    fn apply_intent(
        &mut self,
        intent: SceneInteractionIntent,
    ) -> Result<SceneInteractionBatchResult, Self::Error> {
        self.apply_intents(std::slice::from_ref(&intent))
    }

    /// Applies many intents and returns a batch result (including drained signals).
    ///
    /// Editor adapters should commit **one** undoable transaction for the batch.
    ///
    /// # Errors
    ///
    /// Stops at the first intent failure unless the adapter documents otherwise.
    fn apply_intents(
        &mut self,
        intents: &[SceneInteractionIntent],
    ) -> Result<SceneInteractionBatchResult, Self::Error>;
}
