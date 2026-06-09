//! Runs semantic analysis off the async executor and maps its diagnostics

use crate::convert::{Encoding, span_to_range, url_for_file};
use nyx::{Analysis, RichDiagnostic, SemanticAnalysis, Severity, SourceMap};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tower_lsp::lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Url,
};

/// how long to wait after the last keystroke before re-analysing
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// cpu-bound, intended to run inside [`tokio::task::spawn_blocking`]
pub fn run(entry: PathBuf, overlays: HashMap<PathBuf, String>) -> SemanticAnalysis {
    Analysis::new(entry).with_overlays(overlays).run()
}

/// lsp diagnostics routed to the file each one belongs to
/// diagnostics with no resolvable primary span are attached to `entry_url`
pub fn diagnostics_by_url(
    analysis: &SemanticAnalysis,
    entry_url: &Url,
    encoding: Encoding,
) -> HashMap<Url, Vec<Diagnostic>> {
    let map = &analysis.source_map;
    let mut out: HashMap<_, Vec<_>> = HashMap::new();

    for error in &analysis.diagnostics {
        let (url, range) = match &error.primary {
            Some(label) if !map.is_empty() => {
                let file = map.span_data(label.span).file;
                match url_for_file(map, file) {
                    Some(url) => (url, span_to_range(map, label.span, encoding)),
                    None => (entry_url.clone(), Default::default()),
                }
            },
            _ => (entry_url.clone(), Default::default()),
        };

        out.entry(url).or_default().push(Diagnostic {
            range,
            severity: Some(severity(error.severity)),
            code: error.code.map(|c| NumberOrString::String(c.to_string())),
            message: error.to_string(),
            source: Some("nyx".into()),
            related_information: related_information(map, error, encoding),
            ..Default::default()
        });
    }

    out
}

fn related_information(
    map: &SourceMap,
    error: &RichDiagnostic,
    encoding: Encoding,
) -> Option<Vec<DiagnosticRelatedInformation>> {
    if error.secondary.is_empty() || map.is_empty() {
        return None;
    }

    let infos: Vec<_> = error
        .secondary
        .iter()
        .filter_map(|label| {
            let file = map.span_data(label.span).file;
            Some(DiagnosticRelatedInformation {
                location: Location {
                    uri: url_for_file(map, file)?,
                    range: span_to_range(map, label.span, encoding),
                },
                message: label.message.clone(),
            })
        })
        .collect();

    (!infos.is_empty()).then_some(infos)
}

fn severity(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
    }
}
