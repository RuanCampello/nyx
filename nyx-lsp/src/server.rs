//! The Nyx language server: state, request handlers, and the debounced,
//! cancellable analysis pipeline

use crate::convert::{self, Encoding};
use crate::document::{self, Documents};
use crate::feature::diagnostics::{self, DEBOUNCE};
use crate::feature::{highlight, tokens};
use nyx::{SemanticAnalysis, SymbolKind};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{RwLock, RwLockReadGuard};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, jsonrpc::Result};

pub struct NyxLsp {
    client: Client,
    state: Arc<State>,
}

struct State {
    documents: RwLock<Documents>,
    analyses: RwLock<HashMap<Url, SemanticAnalysis>>,
    encoding: RwLock<Encoding>,
    /// per-document analysis generation, bumped on every change so stale runs
    /// can be discarded before they publish
    generations: Mutex<HashMap<Url, u64>>,
    /// the generation each stored analysis was produced for, a request whose
    /// document has moved past it is answered with `ContentModified`
    analysed: Mutex<HashMap<Url, u64>>,
    /// per-entry set of file urls we last published diagnostics to, so we can
    /// clear the ones that no longer have any
    published: Mutex<HashMap<Url, HashSet<Url>>>,
    /// whether the client renders work-done progress
    progress: AtomicBool,
}

/// floor on how long the load spinner stays up, so a near-instant analysis is
/// still perceptible as "loading … done" rather than an invisible flash
const MIN_PROGRESS: std::time::Duration = std::time::Duration::from_millis(500);

static PROGRESS_SEQ: AtomicU64 = AtomicU64::new(0);

impl NyxLsp {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(State {
                documents: RwLock::new(Documents::default()),
                analyses: RwLock::new(HashMap::new()),
                encoding: RwLock::new(Encoding::Utf16),
                generations: Mutex::new(HashMap::new()),
                analysed: Mutex::new(HashMap::new()),
                published: Mutex::new(HashMap::new()),
                progress: AtomicBool::new(false),
            }),
        }
    }

    /// schedule a debounced re-analysis of `url`, discarding the result if a
    /// newer change arrives while it is in flight
    fn schedule(&self, url: Url) {
        let generation = self.state.bump(&url);
        let client = self.client.clone();
        let state = Arc::clone(&self.state);

        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if state.generation(&url) != generation {
                return;
            }

            let (entry, overlays, encoding) = {
                let documents = state.documents.read().await;
                let Some(entry) = documents.entry(&url) else {
                    return;
                };
                (entry, documents.snapshot(), *state.encoding.read().await)
            };

            let started = std::time::Instant::now();
            let progress = match state.progress.load(Ordering::Relaxed)
                && state.analyses.read().await.get(&url).is_none()
            {
                true => progress_begin(&client, "nyx: analysing project").await,
                false => None,
            };

            let analysis =
                tokio::task::spawn_blocking(move || diagnostics::run(entry, overlays)).await;

            if let Ok(analysis) = analysis
                && state.generation(&url) == generation
            {
                let ok = analysis.ok;
                state.publish(&client, &url, encoding, analysis).await;

                if ok {
                    state.mark_analysed(&url, generation);
                    client.inlay_hint_refresh().await.ok();
                }
            }

            // a small project analyses in a few ms, far too fast to see, hold the
            // spinner to a floor so the load is actually perceptible, then close it
            if progress.is_some() {
                if let Some(remaining) = MIN_PROGRESS.checked_sub(started.elapsed()) {
                    tokio::time::sleep(remaining).await;
                }
                progress_end(&client, progress).await;
            }
        });
    }

    /// resolve a document position to its file and global byte offset
    async fn locate(
        &self,
        map: &nyx::SourceMap,
        uri: &Url,
        position: Position,
    ) -> Option<(nyx::FileId, nyx::BytePos)> {
        let path = document::canonical(uri)?;
        let file = map.file_by_path(&path)?;
        let encoding = *self.state.encoding.read().await;

        Some((file, convert::position_to_pos(map, file, position, encoding)))
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for NyxLsp {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        const VERSION: &str = env!("CARGO_PKG_VERSION");

        let root = params.root_uri.as_ref().and_then(|u| u.to_file_path().ok()).or_else(|| {
            params
                .workspace_folders
                .as_deref()?
                .first()
                .and_then(|f| f.uri.to_file_path().ok())
        });
        self.state.documents.write().await.set_root(root);

        let encoding = negotiate_encoding(&params.capabilities);
        *self.state.encoding.write().await = encoding;

        let progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or(false);
        self.state.progress.store(progress, Ordering::Relaxed);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(match encoding {
                    Encoding::Utf8 => PositionEncodingKind::UTF8,
                    Encoding::Utf16 => PositionEncodingKind::UTF16,
                }),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: tokens::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: Some(false),
                            work_done_progress_options: Default::default(),
                        },
                    ),
                ),
                ..Default::default()
            },
            server_info: Some(ServerInfo { name: "nyx-lsp".into(), version: Some(VERSION.into()) }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client.log_message(MessageType::INFO, "nyx-lsp initialised").await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let url = params.text_document.uri;
        self.state.documents.write().await.open(&url, params.text_document.text);
        self.schedule(url);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let url = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().next_back() {
            self.state.documents.write().await.open(&url, change.text);
        }
        self.schedule(url);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let url = params.text_document.uri;
        self.state.documents.write().await.close(&url);
        self.state.analyses.write().await.remove(&url);
        self.client.publish_diagnostics(url, vec![], None).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.schedule(params.text_document.uri);
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let url = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let Some((analysis, encoding)) = self.state.get_analysis_and_encoding(url).await else {
            return Ok(None);
        };

        let map = &analysis.source_map;
        let Some((file, pos)) = self.locate(map, url, position).await else {
            return Ok(None);
        };

        let hit = analysis
            .hover_types
            .iter()
            .filter(|(span, _)| {
                map.span_data(*span).file == file && span.start <= pos && pos < span.end
            })
            .min_by_key(|(span, _)| span.end.0 - span.start.0);

        Ok(hit.map(|(span, ty)| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```nyx\n{ty}\n```"),
            }),
            range: Some(convert::span_to_range(map, *span, encoding)),
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let url = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let Some((analysis, encoding)) = self.state.get_analysis_and_encoding(url).await else {
            return Ok(None);
        };
        let map = &analysis.source_map;
        let Some((file, pos)) = self.locate(map, url, position).await else {
            return Ok(None);
        };

        let hit = analysis.goto_definitions.iter().find(|(use_span, _)| {
            map.span_data(**use_span).file == file && use_span.start <= pos && pos < use_span.end
        });

        Ok(hit.and_then(|(_, def)| {
            let def_file = map.span_data(*def).file;
            Some(GotoDefinitionResponse::Scalar(Location {
                uri: convert::url_for_file(map, def_file)?,
                range: convert::span_to_range(map, *def, encoding),
            }))
        }))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let url = &params.text_document.uri;
        let range = params.range;

        if !self.state.is_fresh(url) {
            return Err(content_modified());
        }

        let Some((analysis, encoding)) = self.state.get_analysis_and_encoding(url).await else {
            return Ok(None);
        };
        let map = &analysis.source_map;
        let Some(path) = document::canonical(url) else {
            return Ok(None);
        };
        let Some(file) = map.file_by_path(&path) else {
            return Ok(None);
        };

        let hints = analysis
            .inlay_hints
            .iter()
            .filter(|(span, _)| map.span_data(*span).file == file && !already_annotated(map, *span))
            .filter_map(|(span, ty)| {
                let position = convert::span_to_range(map, *span, encoding).end;
                (position >= range.start && position <= range.end).then(|| InlayHint {
                    position,
                    label: InlayHintLabel::String(format!(": {ty}")),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                })
            })
            .collect();

        Ok(Some(hints))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let url = &params.text_document.uri;

        let Some((analysis, encoding)) = self.state.get_analysis_and_encoding(url).await else {
            return Ok(None);
        };
        let map = &analysis.source_map;
        let Some(path) = document::canonical(url) else {
            return Ok(None);
        };
        let Some(file) = map.file_by_path(&path) else {
            return Ok(None);
        };

        let symbols = analysis
            .document_symbols
            .iter()
            .filter(|symbol| map.span_data(symbol.span).file == file)
            .map(|symbol| {
                #[allow(deprecated)]
                SymbolInformation {
                    name: symbol.name.clone(),
                    kind: symbol_kind(symbol.kind),
                    location: Location {
                        uri: url.clone(),
                        range: convert::span_to_range(map, symbol.span, encoding),
                    },
                    tags: None,
                    deprecated: None,
                    container_name: None,
                }
            })
            .collect();

        Ok(Some(DocumentSymbolResponse::Flat(symbols)))
    }

    // clients (and their plugins) routinely request these even when we do not
    // advertise them. answering with an empty result avoids the json-rpc
    // "method not found" (-32601) replies that surface as editor error popups

    async fn completion(&self, _: CompletionParams) -> Result<Option<CompletionResponse>> {
        Ok(None)
    }

    async fn signature_help(&self, _: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        Ok(None)
    }

    async fn references(&self, _: ReferenceParams) -> Result<Option<Vec<Location>>> {
        Ok(None)
    }

    async fn document_highlight(
        &self,
        _: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        Ok(None)
    }

    async fn code_action(&self, _: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        Ok(None)
    }

    async fn code_lens(&self, _: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        Ok(None)
    }

    async fn formatting(&self, _: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        Ok(None)
    }

    async fn range_formatting(
        &self,
        _: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        Ok(None)
    }

    async fn rename(&self, _: RenameParams) -> Result<Option<WorkspaceEdit>> {
        Ok(None)
    }

    async fn prepare_rename(
        &self,
        _: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        Ok(None)
    }

    async fn folding_range(&self, _: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        Ok(None)
    }

    async fn selection_range(
        &self,
        _: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        Ok(None)
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let Some(text) = self.state.documents.read().await.text(&params.text_document.uri) else {
            return Ok(None);
        };
        let encoding = *self.state.encoding.read().await;
        let data = tokens::encode(&highlight::highlight(&text), encoding);

        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data })))
    }

    async fn symbol(&self, _: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        Ok(None)
    }
}

impl State {
    fn bump(&self, url: &Url) -> u64 {
        let mut generations = self.generations.lock().unwrap();
        let counter = generations.entry(url.clone()).or_insert(0);

        *counter += 1;
        *counter
    }

    #[inline]
    fn generation(&self, url: &Url) -> u64 {
        self.generations.lock().unwrap().get(url).copied().unwrap_or(0)
    }

    #[inline]
    fn mark_analysed(&self, url: &Url, generation: u64) {
        self.analysed.lock().unwrap().insert(url.clone(), generation);
    }

    #[inline]
    fn is_fresh(&self, url: &Url) -> bool {
        self.analysed.lock().unwrap().get(url).copied() == Some(self.generation(url))
    }

    async fn publish(
        &self,
        client: &Client,
        entry: &Url,
        encoding: Encoding,
        analysis: SemanticAnalysis,
    ) {
        let by_url = diagnostics::diagnostics_by_url(&analysis, entry, encoding);
        let fresh: HashSet<Url> = by_url.keys().cloned().collect();

        // swap in the feature data (hover/goto/inlay/symbols) only when the
        // project actually analysed, otherwise keep the last good results
        {
            let mut analyses = self.analyses.write().await;
            let previous = analyses.remove(entry);
            analyses.insert(entry.clone(), latest_good(previous, analysis));
        }

        // diagnostics always reflect the latest state, clear files that used to
        // have diagnostics from this entry but no longer do
        let stale: Vec<_> = {
            let mut published = self.published.lock().unwrap();
            let previous = published.get(entry).cloned().unwrap_or_default();

            published.insert(entry.clone(), fresh.clone());
            previous.difference(&fresh).cloned().collect()
        };

        for url in stale {
            client.publish_diagnostics(url, vec![], None).await;
        }
        for (url, diagnostics) in by_url {
            client.publish_diagnostics(url, diagnostics, None).await;
        }
    }

    async fn get_analysis_and_encoding<'a>(
        &'a self,
        url: &Url,
    ) -> Option<(RwLockReadGuard<'a, SemanticAnalysis>, Encoding)> {
        let state_guard = self.analyses.read().await;
        let encoding = *self.encoding.read().await;

        if !state_guard.contains_key(url) {
            return None;
        }

        let guard = RwLockReadGuard::map(state_guard, |state| state.get(url).unwrap());

        Some((guard, encoding))
    }
}

#[inline(always)]
fn negotiate_encoding(capabilities: &ClientCapabilities) -> Encoding {
    let offered = capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref());

    match offered {
        Some(encodings) if encodings.contains(&PositionEncodingKind::UTF8) => Encoding::Utf8,
        _ => Encoding::Utf16,
    }
}

#[inline(always)]
fn already_annotated(map: &nyx::SourceMap, span: nyx::Span) -> bool {
    map.source_after(span.end).trim_start().starts_with(':')
}

#[inline(always)]
const fn symbol_kind(kind: SymbolKind) -> tower_lsp::lsp_types::SymbolKind {
    match kind {
        SymbolKind::Function => tower_lsp::lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Struct => tower_lsp::lsp_types::SymbolKind::STRUCT,
        SymbolKind::Enum => tower_lsp::lsp_types::SymbolKind::ENUM,
    }
}

#[inline(always)]
fn latest_good(previous: Option<SemanticAnalysis>, next: SemanticAnalysis) -> SemanticAnalysis {
    match next.ok {
        true => next,
        false => previous.unwrap_or(next),
    }
}

/// the lsp `ContentModified` error: the document changed under a request, so the
/// client should keep its current results and re-pull after the next refresh
fn content_modified() -> tower_lsp::jsonrpc::Error {
    tower_lsp::jsonrpc::Error {
        code: tower_lsp::jsonrpc::ErrorCode::ServerError(-32801),
        message: "content modified".into(),
        data: None,
    }
}

/// open a work-done progress (the editor's load spinner), returning the token to
/// close it with via [`progress_end`]
///
/// `none` if the client declined to create it
async fn progress_begin(client: &Client, title: &str) -> Option<ProgressToken> {
    let token = format!("nyx/{}", PROGRESS_SEQ.fetch_add(1, Ordering::Relaxed));
    let token = ProgressToken::String(token);

    client
        .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
            token: token.clone(),
        })
        .await
        .ok()?;

    client
        .send_notification::<notification::Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: title.to_owned(),
                cancellable: Some(false),
                message: None,
                percentage: None,
            })),
        })
        .await;

    Some(token)
}

async fn progress_end(client: &Client, token: Option<ProgressToken>) {
    let Some(token) = token else {
        return;
    };

    client
        .send_notification::<notification::Progress>(ProgressParams {
            token,
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                message: None,
            })),
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(ok: bool, hints: usize) -> SemanticAnalysis {
        SemanticAnalysis {
            ok,
            inlay_hints: (0..hints).map(|_| (nyx::Span::default(), "i32".to_string())).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn failed_reanalysis_keeps_last_good_hints() {
        let kept = latest_good(Some(analysis(true, 2)), analysis(false, 0));
        assert!(kept.ok, "should retain the previous good analysis");
        assert_eq!(kept.inlay_hints.len(), 2, "hints must survive the broken edit");
    }

    #[test]
    fn successful_reanalysis_replaces() {
        let kept = latest_good(Some(analysis(true, 2)), analysis(true, 3));
        assert_eq!(kept.inlay_hints.len(), 3, "fresh hints replace the old ones");
    }

    #[test]
    fn first_result_is_kept_even_when_broken() {
        let kept = latest_good(None, analysis(false, 0));
        assert!(!kept.ok);
        assert!(kept.inlay_hints.is_empty());
    }
}
