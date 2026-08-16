//! The diagnostics sink used during HIR lowering
//!
//! Instead of aborting on the first error, lowering records diagnostics here and
//! recovers, so a single pass can report every error
//!
//! It mirrors rustc's `DiagCtxt` + `ErrorGuaranteed`, the only way to mint the proof token is to actually report a diagnostic

use crate::diagnostic::{RichDiagnostic, Severity};
use std::collections::HashSet;

/// A zero-sized proof that a diagnostic has been reported
///
/// Constructible only by [Diagnostics::emit], so a poison [Type::error] cannot exist unless an error was genuinely recorded
///
/// [Type::error]: crate::hir::Type::error
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ErrorGuaranteed(());

/// Accumulates lowering diagnostics and remembers whether any error was emitted,
/// so callers can taint results and skip code generation for poisoned items
///
/// Warnings live here too: they travel with the batch to the editor and the CLI
/// but never stop a build, so [Diagnostics::has_errors] filters on severity
#[derive(Debug, Default)]
pub(crate) struct Diagnostics {
    errors: Vec<RichDiagnostic>,
    seen: HashSet<RichDiagnostic>,
}

impl Diagnostics {
    pub(crate) fn emit(&mut self, diagnostic: RichDiagnostic) -> ErrorGuaranteed {
        self.record(diagnostic);
        ErrorGuaranteed(())
    }

    /// Record a diagnostic that does not stop the build
    ///
    /// Takes no [ErrorGuaranteed] out, so a warning can never poison a type
    pub(crate) fn warn(&mut self, mut diagnostic: RichDiagnostic) {
        diagnostic.severity = Severity::Warning;
        self.record(diagnostic);
    }

    fn record(&mut self, diagnostic: RichDiagnostic) {
        if self.seen.insert(diagnostic.clone()) {
            self.errors.push(diagnostic);
        }
    }

    pub(crate) fn extend(&mut self, diagnostics: impl IntoIterator<Item = RichDiagnostic>) {
        for diagnostic in diagnostics {
            self.record(diagnostic);
        }
    }

    pub(crate) fn take_errors(&mut self) -> Vec<RichDiagnostic> {
        self.seen.clear();
        let mut errors = std::mem::take(&mut self.errors);
        errors.sort_by_key(|error| {
            error.primary.as_ref().map_or(u32::MAX, |label| label.span.start.0)
        });

        errors
    }
}
