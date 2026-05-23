#![allow(unused)]

extern crate proc_macro;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod diagnostic;
mod fmt;

/// Derive `IntoDiagnostic` for an error kind enum.
///
/// Each variant must carry a `#[diagnostic(...)]` attribute specifying how to
/// render that variant into a human-readable compiler diagnostic. Variants too
/// complex for the attribute DSL can use `#[diagnostic(custom)]` and supply a
/// manual `into_diagnostic_custom(self, span: Span) -> Diagnostic` inherent
/// method on the enum.
///
/// ## Required keys
/// - `message = "..."` — the top-level error heading
/// - `primary  = "..."` — the label attached to the primary span
///
/// ## Optional keys
/// - `note = "..."`  — a note shown below the diagnostic
/// - `help = "..."`  — a help suggestion shown below the diagnostic
/// - `secondary(span_field = "FIELD_NAME", label = "...")`
///   — an extra label using a span from one of the variant's fields
///
/// ## String interpolation
/// Inside any string value you can embed field references:
/// - `{field}`   — `field.to_string()` (plain Display)
/// - `{field!}`  — `hi(&field)` (highlight colour)
/// - `{field~}`  — `field.fg(PRIMARY)` (primary colour)
/// - `{field^}`  — `field.fg(SECONDARY)` (secondary colour)
/// - `` {`code snippet`} `` — `"code snippet".fg(SECONDARY)` (code colour)
///
/// Code snippets can contain plain `{field}` references themselves:
/// `` {`fn {name}(…)`} `` renders the field inside the secondary-coloured snippet.
///
/// ## Example
/// ```ignore
/// #[derive(Debug, Clone, PartialEq, Diagnostic)]
/// pub enum HirErrorKind<'h> {
///     #[diagnostic(
///         message = "call to unknown function {name!}",
///         primary = "{name!} is not a known function",
///         help    = "declare {`fn {name}(…)`} before calling it",
///     )]
///     UnknownFunction { name: String },
///
///     #[diagnostic(custom)]
///     OrphanImpl { name: String },
/// }
///
/// // for the custom variant:
/// impl<'h> HirErrorKind<'h> {
///     pub fn into_diagnostic_custom(self, span: Span) -> Diagnostic {
///         match self {
///             Self::OrphanImpl { name } => { /* ... */ }
///             _ => unreachable!("non-custom variant routed to into_diagnostic_custom"),
///         }
///     }
/// }
/// ```
#[proc_macro_derive(Diagnostic, attributes(diagnostic))]
pub fn derive_diagnostic(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match diagnostic::derive_diagnostic(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
