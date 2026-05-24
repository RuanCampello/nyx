extern crate proc_macro;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod diagnostic;
mod fmt;

/// Derive `AsDiagnostic` for an error kind enum.
///
/// Each variant must carry a `#[diagnostic(...)]` attribute specifying how to
/// render that variant into a human-readable compiler diagnostic. Variants that
/// simply wrap another `AsDiagnostic` type can use `#[diagnostic(transparent)]`.
///
/// ## Required keys
/// - `message = "..."` — the top-level error heading
/// - `primary  = "..."` — the label attached to the primary span
///
/// ## Optional keys
/// - `note = "..."`  — a note shown below the diagnostic
/// - `help = "..."`  — a help suggestion shown below the diagnostic
/// - `secondary(label = "...")`
///   — an extra label using the primary span by default
/// - `secondary(span_field = "FIELD_NAME", label = "...")`
///   — an extra label using a span from one of the variant's fields
///
/// ## String interpolation
/// Inside any string value you can embed field references (including simple methods like `.display()`):
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
///     #[diagnostic(transparent)]
///     Diagnostic(Diagnostic),
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
