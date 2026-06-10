//! The diagnostics sink used during HIR lowering
//!
//! Instead of aborting on the first error, lowering records diagnostics here and
//! recovers, so a single pass can report every error
//!
//! It mirrors rustc's `DiagCtxt` + `ErrorGuaranteed`, the only way to mint the proof token is to actually report a diagnostic

use crate::diagnostic::RichDiagnostic;

/// A zero-sized proof that a diagnostic has been reported
///
/// Constructible only by [Diagnostics::emit], so a poison [Type::error] cannot exist unless an error was genuinely recorded
///
/// [Type::error]: crate::hir::Type::error
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ErrorGuaranteed(());

/// Accumulates lowering diagnostics and remembers whether any error was emitted,
/// so callers can taint results and skip code generation for poisoned items
#[derive(Debug, Default)]
pub(crate) struct Diagnostics {
    errors: Vec<RichDiagnostic>,
}

impl Diagnostics {
    pub(crate) fn emit(&mut self, diagnostic: RichDiagnostic) -> ErrorGuaranteed {
        self.errors.push(diagnostic);
        ErrorGuaranteed(())
    }

    pub(crate) fn take_errors(&mut self) -> Vec<RichDiagnostic> {
        std::mem::take(&mut self.errors)
    }
}
