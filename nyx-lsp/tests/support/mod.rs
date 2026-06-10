//! In-process LSP test harness
//!
//! Boots the real [`NyxLsp`] server over an in-memory duplex transport and
//! speaks framed JSON-RPC to it exactly like an editor would, so every test
//! exercises the full wire path: framing, routing, handlers, and notifications

use nyx_lsp::Lsp;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tower_lsp::lsp_types::*;
use tower_lsp::{LspService, Server};

pub struct TestClient {
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
    buffer: Vec<u8>,
    /// server -> client notifications received while waiting for something else
    stash: VecDeque<Value>,
    next_id: i64,
    version: i32,
    pub root: PathBuf,
    pub init: InitializeResult,
}

/// JSON-RPC `ContentModified`: the document changed under the request
pub const CONTENT_MODIFIED: i64 = -32801;

const TIMEOUT: Duration = Duration::from_secs(10);

static UNIQUE: AtomicU64 = AtomicU64::new(0);

impl TestClient {
    /// boot a server and complete the `initialize`/`initialized` handshake,
    /// offering utf-8 position encoding
    pub async fn start() -> Self {
        Self::start_with(Some(vec![PositionEncodingKind::UTF8])).await
    }

    /// same as [start](TestClient::start), with explicit position encodings
    pub async fn start_with(encodings: Option<Vec<PositionEncodingKind>>) -> Self {
        let (service, socket) = LspService::new(Lsp::new);
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        tokio::spawn(async move {
            Server::new(server_read, server_write, socket).serve(service).await;
        });

        let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join(format!("nyx_lsp_harness_{}_{unique}", std::process::id()))
            .join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();

        let (reader, writer) = tokio::io::split(client_io);
        let mut client = Self {
            reader,
            writer,
            buffer: Vec::new(),
            stash: VecDeque::new(),
            next_id: 0,
            version: 0,
            root: root.clone(),
            init: InitializeResult::default(),
        };

        #[allow(deprecated)]
        let params = InitializeParams {
            root_uri: Some(Url::from_file_path(&root).unwrap()),
            capabilities: ClientCapabilities {
                general: Some(GeneralClientCapabilities {
                    position_encodings: encodings,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        client.init = client.request::<request::Initialize>(params).await.unwrap();
        client.notify::<notification::Initialized>(InitializedParams {}).await;

        client
    }

    pub async fn request<R: request::Request>(
        &mut self,
        params: R::Params,
    ) -> Result<R::Result, Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": R::METHOD,
            "params": serde_json::to_value(params).unwrap(),
        }))
        .await;

        loop {
            let message = self.next_message().await;
            match message.get("method") {
                Some(_) => self.stash.push_back(message),
                None if message["id"] == json!(id) => {
                    return match message.get("error") {
                        Some(error) => Err(error.clone()),
                        None => Ok(serde_json::from_value(message["result"].clone()).unwrap()),
                    };
                },
                None => panic!("response to a request that was never sent: {message}"),
            }
        }
    }

    pub async fn notify<N: notification::Notification>(&mut self, params: N::Params) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": serde_json::to_value(params).unwrap(),
        }))
        .await;
    }

    /// write `text` into the project on disk and open it as a buffer
    pub async fn open(&mut self, name: &str, text: &str) -> Url {
        let path = self.root.join(name);
        std::fs::write(&path, text).unwrap();
        let url = Url::from_file_path(path.canonicalize().unwrap()).unwrap();

        self.notify::<notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: url.clone(),
                language_id: "nyx".into(),
                version: self.version,
                text: text.into(),
            },
        })
        .await;

        url
    }

    /// replace the buffer contents (full-document sync, like the server advertises)
    pub async fn change(&mut self, url: &Url, text: &str) {
        self.version += 1;
        self.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: url.clone(),
                version: self.version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.into(),
            }],
        })
        .await;
    }

    /// wait for the next `textDocument/publishDiagnostics` for `url`
    pub async fn wait_diagnostics(&mut self, url: &Url) -> Vec<Diagnostic> {
        let matches = |message: &Value| {
            message["method"] == json!("textDocument/publishDiagnostics")
                && message["params"]["uri"] == json!(url)
        };

        if let Some(at) = self.stash.iter().position(matches) {
            let message = self.stash.remove(at).unwrap();
            return serde_json::from_value(message["params"]["diagnostics"].clone()).unwrap();
        }

        loop {
            let message = self.next_message().await;
            match matches(&message) {
                true => {
                    return serde_json::from_value(message["params"]["diagnostics"].clone())
                        .unwrap();
                },
                false => self.stash.push_back(message),
            }
        }
    }

    pub async fn hover(&mut self, url: &Url, position: Position) -> Option<Hover> {
        self.request::<request::HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: url.clone() },
                position,
            },
            work_done_progress_params: Default::default(),
        })
        .await
        .unwrap()
    }

    pub async fn hover_text(&mut self, url: &Url, position: Position) -> String {
        let hover = self.hover(url, position).await.unwrap_or_else(|| {
            panic!("expected hover at {}:{}", position.line, position.character)
        });

        match hover.contents {
            HoverContents::Markup(markup) => markup.value,
            other => panic!("expected markdown hover, got {other:?}"),
        }
    }

    pub async fn inlay_hints(&mut self, url: &Url) -> Result<Vec<InlayHint>, Value> {
        self.request::<request::InlayHintRequest>(InlayHintParams {
            text_document: TextDocumentIdentifier { uri: url.clone() },
            range: Range::new(Position::new(0, 0), Position::new(u32::MAX, 0)),
            work_done_progress_params: Default::default(),
        })
        .await
        .map(Option::unwrap_or_default)
    }

    /// pull inlay hints, retrying `ContentModified` exactly like an editor
    /// re-pulls after a refresh
    pub async fn inlay_hints_fresh(&mut self, url: &Url) -> Vec<InlayHint> {
        for _ in 0..100 {
            match self.inlay_hints(url).await {
                Ok(hints) => return hints,
                Err(error) if error["code"] == json!(CONTENT_MODIFIED) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                },
                Err(error) => panic!("inlay hint request failed: {error}"),
            }
        }

        panic!("inlay hints never became fresh for {url}");
    }

    async fn send(&mut self, value: &Value) {
        let body = value.to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());

        self.writer.write_all(framed.as_bytes()).await.unwrap();
    }

    /// server -> client requests (progress creation, refreshes) are answered with `null` transparently
    async fn next_message(&mut self) -> Value {
        loop {
            let message = self.recv().await;
            if message.get("method").is_some() && message.get("id").is_some() {
                let id = message["id"].clone();
                self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": null })).await;

                continue;
            }
            return message;
        }
    }

    async fn recv(&mut self) -> Value {
        loop {
            if let Some(message) = self.parse_frame() {
                return message;
            }

            let mut chunk = [0u8; 4096];
            let read = tokio::time::timeout(TIMEOUT, self.reader.read(&mut chunk))
                .await
                .expect("timed out waiting for a server message")
                .expect("transport failed");
            assert!(read > 0, "server closed the transport");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn parse_frame(&mut self) -> Option<Value> {
        let headers_end = find(&self.buffer, b"\r\n\r\n")?;
        let headers = std::str::from_utf8(&self.buffer[..headers_end]).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .expect("missing Content-Length header")
            .trim()
            .parse()
            .unwrap();

        let body_start = headers_end + 4;
        if self.buffer.len() < body_start + length {
            return None;
        }

        let message = serde_json::from_slice(&self.buffer[body_start..body_start + length])
            .expect("malformed JSON-RPC body");
        self.buffer.drain(..body_start + length);

        Some(message)
    }
}

/// 0-based position of the first byte of `needle`'s `nth` occurrence, with byte (UTF-8) columns
pub fn position_of_nth(text: &str, needle: &str, nth: usize) -> Position {
    let offset = text
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("{needle:?} (occurrence {nth}) not found"))
        .0;

    let line = text[..offset].matches('\n').count() as u32;
    let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);

    Position::new(line, (offset - line_start) as u32)
}

pub fn position_of(text: &str, needle: &str) -> Position {
    position_of_nth(text, needle, 0)
}

/// like [position_of], but with UTF-16 columns for a UTF-16 client
pub fn position_of_utf16(text: &str, needle: &str) -> Position {
    let offset = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not found"));
    let line = text[..offset].matches('\n').count() as u32;
    let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    let column = text[line_start..offset].encode_utf16().count() as u32;

    Position::new(line, column)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}
