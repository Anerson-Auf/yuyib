//! Editor adapter for [`yuyib_scene_interaction::SceneInteractionBridge`].

use yuyib_authoring::Revision;
use yuyib_scene_interaction::{
    BridgeCapabilities, SceneInteractionBatchResult, SceneInteractionBridge,
    SceneInteractionIntent, editor_capabilities,
};

use crate::scene_authoring::{SceneMutationError, SceneSession};

/// Document-backed bridge: intents → one undoable [`SceneSession`] transaction.
pub struct EditorDocumentBridge<'session> {
    session: &'session mut SceneSession,
    expected_revision: u64,
    /// Authoring revision after the last successful batch.
    pub revision: Option<Revision>,
    /// Last batch result (signals + counts).
    pub last_batch: Option<SceneInteractionBatchResult>,
}

impl<'session> EditorDocumentBridge<'session> {
    /// Borrows the open scene session at `expected_revision`.
    pub fn new(session: &'session mut SceneSession, expected_revision: u64) -> Self {
        Self {
            session,
            expected_revision,
            revision: None,
            last_batch: None,
        }
    }
}

impl SceneInteractionBridge for EditorDocumentBridge<'_> {
    type Error = SceneMutationError;

    fn capabilities(&self) -> BridgeCapabilities {
        editor_capabilities()
    }

    fn apply_intents(
        &mut self,
        intents: &[SceneInteractionIntent],
    ) -> Result<SceneInteractionBatchResult, Self::Error> {
        let (revision, batch) = self
            .session
            .apply_interaction_intents(self.expected_revision, intents)?;
        self.expected_revision = revision.get();
        self.revision = Some(revision);
        self.last_batch = Some(batch.clone());
        Ok(batch)
    }
}
