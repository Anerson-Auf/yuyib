use std::{any::Any, error::Error, fmt};

use serde::{Deserialize, Serialize};

/// Monotonic document revision used for optimistic conflict detection.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    /// Initial revision of a document without committed command history.
    pub const INITIAL: Self = Self(0);

    /// Creates a revision restored from document/session metadata.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric revision.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Failure reported by one document command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandError {
    message: String,
}

impl CommandError {
    /// Creates a command failure with a user-facing explanation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the failure explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CommandError {}

/// Reversible mutation of a generic authored document.
///
/// Commands should retain both the before and after values needed to replay
/// themselves. They must not mutate external state that cannot be compensated.
pub trait DocumentCommand<D>: Any + Send {
    /// Human-facing command label.
    fn label(&self) -> &str;

    /// Applies or reapplies the mutation.
    ///
    /// # Errors
    ///
    /// Returns a domain-specific validation or mutation failure.
    fn apply(&mut self, document: &mut D) -> Result<(), CommandError>;

    /// Reverts the mutation.
    ///
    /// # Errors
    ///
    /// Returns a domain-specific validation or mutation failure.
    fn revert(&mut self, document: &mut D) -> Result<(), CommandError>;

    /// Exposes concrete command type for opt-in merge implementations.
    fn as_any(&self) -> &dyn Any;

    /// Coalesces a newer, already-applied command into this history entry.
    ///
    /// Implementations return `true` only when undoing the updated command will
    /// restore the state before both commands. The default refuses merging.
    fn merge_applied(&mut self, _newer: &dyn DocumentCommand<D>) -> bool {
        false
    }
}

/// Named atomic group of reversible document commands.
pub struct CommandTransaction<D> {
    label: String,
    merge_key: Option<String>,
    commands: Vec<Box<dyn DocumentCommand<D>>>,
}

impl<D: 'static> CommandTransaction<D> {
    /// Creates an empty transaction.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            merge_key: None,
            commands: Vec::new(),
        }
    }

    /// Adds a key used to coalesce consecutive compatible single-command edits.
    ///
    /// A typical key is `entity-guid/component-id/field-name`. Multi-command
    /// transactions remain separate to preserve atomic semantics.
    #[must_use]
    pub fn with_merge_key(mut self, merge_key: impl Into<String>) -> Self {
        self.merge_key = Some(merge_key.into());
        self
    }

    /// Appends a command in application order.
    #[must_use]
    pub fn push(mut self, command: impl DocumentCommand<D> + 'static) -> Self {
        self.commands.push(Box::new(command));
        self
    }

    /// Returns the transaction label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the number of atomic mutations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Reports whether no mutations have been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn apply_staged(&mut self, document: &mut D) -> Result<(), TransactionError> {
        for index in 0..self.commands.len() {
            if let Err(error) = self.commands[index].apply(document) {
                return Err(TransactionError::Apply {
                    index,
                    command: self.commands[index].label().to_owned(),
                    error,
                });
            }
        }
        Ok(())
    }

    fn revert_staged(&mut self, document: &mut D) -> Result<(), TransactionError> {
        for index in (0..self.commands.len()).rev() {
            if let Err(error) = self.commands[index].revert(document) {
                return Err(TransactionError::Revert {
                    index,
                    command: self.commands[index].label().to_owned(),
                    error,
                });
            }
        }
        Ok(())
    }

    fn try_coalesce(&mut self, newer: &Self) -> bool {
        self.commands.len() == 1
            && newer.commands.len() == 1
            && self.merge_key.is_some()
            && self.merge_key == newer.merge_key
            && self.commands[0].merge_applied(newer.commands[0].as_ref())
    }
}

/// Transaction, undo, redo, or revision failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionError {
    /// The caller edited a stale document revision.
    RevisionConflict {
        /// Revision supplied by the caller.
        expected: Revision,
        /// Current document revision.
        actual: Revision,
    },
    /// A transaction contained no commands.
    EmptyTransaction,
    /// Revision counter exhausted its representation.
    RevisionOverflow,
    /// Forward application failed; the staged document was discarded.
    Apply {
        /// Zero-based failing command index.
        index: usize,
        /// Failing command label.
        command: String,
        /// Original command failure.
        error: CommandError,
    },
    /// Undo failed; the staged document was discarded and history was poisoned.
    Revert {
        /// Zero-based failing command index.
        index: usize,
        /// Failing command label.
        command: String,
        /// Original command failure.
        error: CommandError,
    },
    /// A prior undo/redo failure may have mutated command-internal state.
    Poisoned {
        /// Original failure retained for diagnostics.
        reason: String,
    },
    /// No committed transaction is available to undo.
    NothingToUndo,
    /// No reverted transaction is available to redo.
    NothingToRedo,
}

impl fmt::Display for TransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "document revision conflict: expected {}, actual {}",
                expected.get(),
                actual.get()
            ),
            Self::EmptyTransaction => formatter.write_str("cannot commit an empty transaction"),
            Self::RevisionOverflow => formatter.write_str("document revision counter overflowed"),
            Self::Apply {
                index,
                command,
                error,
            } => write_transaction_failure(formatter, "apply", *index, command, error),
            Self::Revert {
                index,
                command,
                error,
            } => write_transaction_failure(formatter, "revert", *index, command, error),
            Self::Poisoned { reason } => {
                write!(
                    formatter,
                    "command history is poisoned after a failed replay: {reason}"
                )
            }
            Self::NothingToUndo => formatter.write_str("no transaction to undo"),
            Self::NothingToRedo => formatter.write_str("no transaction to redo"),
        }
    }
}

fn write_transaction_failure(
    formatter: &mut fmt::Formatter<'_>,
    operation: &str,
    index: usize,
    command: &str,
    error: &CommandError,
) -> fmt::Result {
    write!(
        formatter,
        "failed to {operation} command {index} ({command}): {error}"
    )
}

impl Error for TransactionError {}

/// Revision-aware undo/redo history for one authored document.
pub struct CommandHistory<D> {
    revision: Revision,
    undo: Vec<CommandTransaction<D>>,
    redo: Vec<CommandTransaction<D>>,
    poisoned: Option<String>,
}

impl<D: Clone + 'static> Default for CommandHistory<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: Clone + 'static> CommandHistory<D> {
    /// Creates empty history at [`Revision::INITIAL`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            revision: Revision::INITIAL,
            undo: Vec::new(),
            redo: Vec::new(),
            poisoned: None,
        }
    }

    /// Creates empty history at a restored revision.
    #[must_use]
    pub const fn at_revision(revision: Revision) -> Self {
        Self {
            revision,
            undo: Vec::new(),
            redo: Vec::new(),
            poisoned: None,
        }
    }

    /// Returns the current monotonic revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns the number of undoable transaction groups.
    #[must_use]
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Returns the number of redoable transaction groups.
    #[must_use]
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Reports whether a failed undo/redo made command-internal state unsafe to reuse.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }

    /// Applies and records an atomic transaction at an expected revision.
    ///
    /// Consecutive compatible single-command edits with the same merge key are
    /// coalesced into one undo step. Every accepted edit still advances the
    /// revision, so external file/session conflicts remain detectable.
    ///
    /// # Errors
    ///
    /// Rejects stale revisions, empty transactions, command failures, failed
    /// compensation, or revision overflow.
    pub fn commit(
        &mut self,
        document: &mut D,
        expected_revision: Revision,
        mut transaction: CommandTransaction<D>,
    ) -> Result<Revision, TransactionError> {
        self.require_healthy()?;
        self.require_revision(expected_revision)?;
        if transaction.is_empty() {
            return Err(TransactionError::EmptyTransaction);
        }
        let next_revision = self
            .revision
            .next()
            .ok_or(TransactionError::RevisionOverflow)?;
        let mut staged = document.clone();
        transaction.apply_staged(&mut staged)?;
        *document = staged;

        let coalesced = self
            .undo
            .last_mut()
            .is_some_and(|previous| previous.try_coalesce(&transaction));
        if !coalesced {
            self.undo.push(transaction);
        }
        self.redo.clear();
        self.revision = next_revision;
        Ok(self.revision)
    }

    /// Atomically reverts the most recent transaction.
    ///
    /// # Errors
    ///
    /// Rejects stale revisions, empty history, command failures, failed
    /// compensation, or revision overflow.
    pub fn undo(
        &mut self,
        document: &mut D,
        expected_revision: Revision,
    ) -> Result<Revision, TransactionError> {
        self.require_healthy()?;
        self.require_revision(expected_revision)?;
        let next_revision = self
            .revision
            .next()
            .ok_or(TransactionError::RevisionOverflow)?;
        let Some(mut transaction) = self.undo.pop() else {
            return Err(TransactionError::NothingToUndo);
        };
        let mut staged = document.clone();
        if let Err(error) = transaction.revert_staged(&mut staged) {
            self.poison(error.to_string());
            return Err(error);
        }
        *document = staged;
        self.redo.push(transaction);
        self.revision = next_revision;
        Ok(self.revision)
    }

    /// Atomically reapplies the most recently undone transaction.
    ///
    /// # Errors
    ///
    /// Rejects stale revisions, empty redo history, command failures, failed
    /// compensation, or revision overflow.
    pub fn redo(
        &mut self,
        document: &mut D,
        expected_revision: Revision,
    ) -> Result<Revision, TransactionError> {
        self.require_healthy()?;
        self.require_revision(expected_revision)?;
        let next_revision = self
            .revision
            .next()
            .ok_or(TransactionError::RevisionOverflow)?;
        let Some(mut transaction) = self.redo.pop() else {
            return Err(TransactionError::NothingToRedo);
        };
        let mut staged = document.clone();
        if let Err(error) = transaction.apply_staged(&mut staged) {
            self.poison(error.to_string());
            return Err(error);
        }
        *document = staged;
        self.undo.push(transaction);
        self.revision = next_revision;
        Ok(self.revision)
    }

    fn require_revision(&self, expected: Revision) -> Result<(), TransactionError> {
        if expected == self.revision {
            Ok(())
        } else {
            Err(TransactionError::RevisionConflict {
                expected,
                actual: self.revision,
            })
        }
    }

    fn require_healthy(&self) -> Result<(), TransactionError> {
        match &self.poisoned {
            Some(reason) => Err(TransactionError::Poisoned {
                reason: reason.clone(),
            }),
            None => Ok(()),
        }
    }

    fn poison(&mut self, reason: String) {
        self.undo.clear();
        self.redo.clear();
        self.poisoned = Some(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SetValue {
        before: i32,
        after: i32,
    }

    impl DocumentCommand<i32> for SetValue {
        fn label(&self) -> &str {
            "set value"
        }

        fn apply(&mut self, document: &mut i32) -> Result<(), CommandError> {
            if *document != self.before {
                return Err(CommandError::new("unexpected before value"));
            }
            *document = self.after;
            Ok(())
        }

        fn revert(&mut self, document: &mut i32) -> Result<(), CommandError> {
            if *document != self.after {
                return Err(CommandError::new("unexpected after value"));
            }
            *document = self.before;
            Ok(())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn merge_applied(&mut self, newer: &dyn DocumentCommand<i32>) -> bool {
            let Some(newer) = newer.as_any().downcast_ref::<Self>() else {
                return false;
            };
            if self.after != newer.before {
                return false;
            }
            self.after = newer.after;
            true
        }
    }

    struct FailApply;

    impl DocumentCommand<i32> for FailApply {
        fn label(&self) -> &str {
            "fail"
        }

        fn apply(&mut self, _document: &mut i32) -> Result<(), CommandError> {
            Err(CommandError::new("deliberate failure"))
        }

        fn revert(&mut self, _document: &mut i32) -> Result<(), CommandError> {
            Ok(())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    struct MutateThenFail;

    impl DocumentCommand<i32> for MutateThenFail {
        fn label(&self) -> &str {
            "mutate then fail"
        }

        fn apply(&mut self, document: &mut i32) -> Result<(), CommandError> {
            *document = 99;
            Err(CommandError::new("failed after mutation"))
        }

        fn revert(&mut self, document: &mut i32) -> Result<(), CommandError> {
            *document = -99;
            Err(CommandError::new("failed after revert mutation"))
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn failed_transaction_rolls_back_its_applied_prefix() {
        let mut document = 1;
        let transaction = CommandTransaction::new("atomic")
            .push(SetValue {
                before: 1,
                after: 2,
            })
            .push(FailApply);
        let mut history = CommandHistory::new();
        let error = history
            .commit(&mut document, Revision::INITIAL, transaction)
            .expect_err("second command fails");

        assert!(matches!(error, TransactionError::Apply { index: 1, .. }));
        assert_eq!(document, 1);
        assert_eq!(history.revision(), Revision::INITIAL);
        assert_eq!(history.undo_len(), 0);
    }

    #[test]
    fn failing_command_cannot_leak_partial_mutation_from_staged_document() {
        let mut document = 1;
        let transaction = CommandTransaction::new("staged").push(MutateThenFail);
        let mut history = CommandHistory::new();
        assert!(matches!(
            history.commit(&mut document, Revision::INITIAL, transaction),
            Err(TransactionError::Apply { .. })
        ));
        assert_eq!(document, 1);
        assert!(!history.is_poisoned());
    }

    #[test]
    fn failed_undo_preserves_document_and_poison_history() {
        let mut document = 1;
        let mut history = CommandHistory::new();
        let revision = history
            .commit(
                &mut document,
                Revision::INITIAL,
                CommandTransaction::new("poison").push(SetValue {
                    before: 1,
                    after: 2,
                }),
            )
            .expect("commit");
        // Make revert fail after mutating only the clone by changing the live
        // document away from the command's expected after value.
        document = 3;
        assert!(matches!(
            history.undo(&mut document, revision),
            Err(TransactionError::Revert { .. })
        ));
        assert_eq!(document, 3);
        assert!(history.is_poisoned());
        assert!(matches!(
            history.undo(&mut document, revision),
            Err(TransactionError::Poisoned { .. })
        ));
    }

    #[test]
    fn coalescing_keeps_one_undo_step_but_advances_revision() {
        let mut document = 1;
        let mut history = CommandHistory::new();
        let revision = history
            .commit(
                &mut document,
                Revision::INITIAL,
                CommandTransaction::new("drag")
                    .with_merge_key("entity/transform/x")
                    .push(SetValue {
                        before: 1,
                        after: 2,
                    }),
            )
            .expect("first edit");
        let revision = history
            .commit(
                &mut document,
                revision,
                CommandTransaction::new("drag")
                    .with_merge_key("entity/transform/x")
                    .push(SetValue {
                        before: 2,
                        after: 3,
                    }),
            )
            .expect("second edit");

        assert_eq!(document, 3);
        assert_eq!(revision.get(), 2);
        assert_eq!(history.undo_len(), 1);
        let revision = history
            .undo(&mut document, revision)
            .expect("undo merged edit");
        assert_eq!(document, 1);
        history
            .redo(&mut document, revision)
            .expect("redo merged edit");
        assert_eq!(document, 3);
    }

    #[test]
    fn stale_revision_cannot_mutate_document() {
        let mut document = 1;
        let mut history = CommandHistory::at_revision(Revision::new(5));
        let transaction = CommandTransaction::new("stale").push(SetValue {
            before: 1,
            after: 2,
        });
        assert!(matches!(
            history.commit(&mut document, Revision::new(4), transaction),
            Err(TransactionError::RevisionConflict { .. })
        ));
        assert_eq!(document, 1);
    }
}
