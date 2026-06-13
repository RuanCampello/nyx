mod support;
use nyx_lsp::fenced_text;
use support::{CONTENT_MODIFIED, TestClient, position_of, position_of_nth, position_of_utf16};
use tower_lsp::lsp_types::*;

#[tokio::test]
async fn initialize_negotiates_utf8_when_offered() {
    let client = TestClient::start().await;
    assert_eq!(client.init.capabilities.position_encoding, Some(PositionEncodingKind::UTF8));
    assert!(client.init.capabilities.hover_provider.is_some());
    assert!(client.init.capabilities.inlay_hint_provider.is_some());
}

#[tokio::test]
async fn initialize_falls_back_to_utf16() {
    let client = TestClient::start_with(None).await;
    assert_eq!(client.init.capabilities.position_encoding, Some(PositionEncodingKind::UTF16));
}

#[tokio::test]
async fn did_close_clears_diagnostics() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "fn main() { let x: bool = 232; }").await;
    assert!(!client.wait_diagnostics(&url).await.is_empty());

    client
        .notify::<notification::DidCloseTextDocument>(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: url.clone() },
        })
        .await;
    assert!(client.wait_diagnostics(&url).await.is_empty(), "closing must clear diagnostics");
}

#[tokio::test]
async fn hover_binding_shows_type_and_layout() {
    let src = "fn main() { let value = 232; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "value")).await;
    assert!(text.contains("i32"), "hover must show the inferred type: {text}");
    assert!(
        text.contains("size = 4 (0x4), align = 0x4"),
        "hover must show size and alignment: {text}"
    );
}

#[tokio::test]
async fn hover_function_declaration_shows_signature() {
    let src = "fn add(a: i32, b: i32): i32 { a + b }\nfn main() { let r = add(1, 2); }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "add")).await;
    assert!(text.contains("fn add(a: i32, b: i32): i32"), "expected the signature: {text}");
}

#[tokio::test]
async fn hover_call_site_shows_signature() {
    let src = "fn add(a: i32, b: i32): i32 { a + b }\nfn main() { let r = add(1, 2); }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of_nth(src, "add", 1)).await;
    assert!(text.contains("fn add(a: i32, b: i32): i32"), "expected the signature: {text}");
}

#[tokio::test]
async fn hover_shows_doc_comment() {
    let src = r#"///
        Adds two numbers together.
        fn add(a: i32, b: i32): i32 { a + b }
        fn main() { let r = add(1, 2); }
    "#;
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "add")).await;
    assert!(text.contains("fn add(a: i32, b: i32): i32"), "expected the signature: {text}");
    assert!(text.contains("Adds two numbers together."), "expected the doc comment: {text}");

    let call = client.hover_text(&url, position_of_nth(src, "add", 1)).await;
    assert!(call.contains("Adds two numbers together."), "call hover shows docs: {call}");
}

#[tokio::test]
async fn hover_parameter_shows_type() {
    let src = "fn scale(factor: i64): i64 { factor }\nfn main() { let r = scale(2); }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "factor")).await;
    assert!(text.contains("i64"), "parameter hover must show its type: {text}");
}

#[tokio::test]
async fn hover_struct_shows_path_definition_and_layout() {
    let src = "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { x: 1, y: 2 }; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "Point")).await;
    assert!(text.contains(&fenced_text("project")), "expected the module path: {text}");
    assert!(text.contains("struct Point"), "expected the definition: {text}");
    assert!(text.contains("x: i64"), "expected the fields: {text}");
    assert!(text.contains("size = 16 (0x10), align = 0x8"), "expected the layout: {text}");
}

#[tokio::test]
async fn hover_struct_literal_shows_the_definition() {
    let src = "struct Point { x: i64, y: i64 }\nfn main() { let p = Point { x: 1, y: 2 }; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    // the second `Point` is the literal in main's body, not the declaration
    let text = client.hover_text(&url, position_of_nth(src, "Point", 1)).await;
    assert!(text.contains("struct Point"), "a type use hovers as its declaration: {text}");
    assert!(text.contains("size = 16 (0x10), align = 0x8"), "expected the layout: {text}");
}

#[tokio::test]
async fn hover_truncates_long_field_lists() {
    let src = "struct Wide { a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32 }\n\
               fn main() { let w = Wide { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7 }; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "Wide")).await;
    assert!(text.contains("e: i32"), "the first five fields are shown: {text}");
    assert!(!text.contains("f: i32"), "fields past the fifth are elided: {text}");
    assert!(text.contains("/* … */"), "the elision marker is shown: {text}");
}

#[tokio::test]
async fn hover_enum_shows_path_definition_and_layout() {
    let src = "enum Status { Ready, Done = 7 } as u16\nfn main() { let s = Status::Ready; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "Status")).await;
    assert!(text.contains(&fenced_text("project")), "expected the module path: {text}");
    assert!(text.contains("enum Status"), "expected the definition: {text}");
    assert!(text.contains("Ready"), "expected the variants: {text}");
    assert!(text.contains("size = 2 (0x2), align = 0x2"), "expected the layout: {text}");

    let used = client.hover_text(&url, position_of_nth(src, "Status", 1)).await;
    assert!(used.contains("enum Status"), "an enum use hovers as its declaration: {used}");
}

#[tokio::test]
async fn hover_method_shows_implementor_and_path() {
    let src = r#"
        struct Point { x: i64, y: i64 }
        impl Point {
            fn norm(&self): i64 { self.x }
        }
        fn main() {
            let p = Point { x: 1, y: 2 };
            let n = p.norm(); }
    "#;

    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "norm")).await;
    assert!(
        text.contains(&fenced_text("project::Point")),
        "the path is qualified by the implementor: {text}"
    );
    assert!(text.contains("impl Point"), "expected the implementor line: {text}");
    assert!(text.contains("fn norm("), "expected the signature: {text}");
}

#[tokio::test]
async fn hover_works_inside_generic_templates() {
    let src = r#"
        pub enum Opt<T> {
            Filled(T),
            Empty,
        }
        impl Opt<T> {
            fn flag(self): bool {
                let marker = 232;
                true
            }
        }
        fn main() { }
    "#;

    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    let diagnostics = client.wait_diagnostics(&url).await;
    assert!(
        diagnostics.is_empty(),
        "template analysis must not leak diagnostics: {diagnostics:#?}"
    );

    let decl = client.hover_text(&url, position_of(src, "Opt")).await;
    assert!(decl.contains("enum Opt<T>"), "the template declaration hovers: {decl}");
    assert!(decl.contains("Filled(T)"), "variants keep their payloads: {decl}");
    assert!(!decl.contains("size ="), "open generics have no meaningful layout: {decl}");

    let method = client.hover_text(&url, position_of(src, "flag")).await;
    assert!(method.contains("fn flag("), "methods inside templates hover: {method}");
    assert!(method.contains("impl Opt<T>"), "with their implementor: {method}");

    let binding = client.hover_text(&url, position_of(src, "marker")).await;
    assert!(binding.contains("i32"), "bindings inside template bodies hover: {binding}");
}

#[tokio::test]
async fn hover_constant_shows_value() {
    let src = r#"
        pub const LIMIT: i32 = -2;
        const NAME: str = "nyx";
        fn main() { }
    "#;
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of(src, "LIMIT")).await;
    assert!(text.contains(&fenced_text("project")), "expected the module path: {text}");
    assert!(
        text.contains("pub const LIMIT: i32 = -2 (0xFFFFFFFE)"),
        "negative constants show their bit pattern: {text}"
    );

    let name = client.hover_text(&url, position_of(src, "NAME")).await;
    assert!(
        name.contains("const NAME: str = \"nyx\""),
        "string constants show the value: {name}"
    );
}

#[tokio::test]
async fn hover_constant_evaluates_wide_arithmetic() {
    let src = r#"
        const HUGE: u64 = (1 << 64) - 1;
        const FLOOR: i64 = (-1 << 63);
        fn main() { }
    "#;
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let huge = client.hover_text(&url, position_of(src, "HUGE")).await;
    assert!(
        huge.contains("const HUGE: u64 = 18446744073709551615"),
        "unsigned constants render in their own domain: {huge}"
    );

    let floor = client.hover_text(&url, position_of(src, "FLOOR")).await;
    assert!(
        floor.contains("const FLOOR: i64 = -9223372036854775808 (0x8000000000000000)"),
        "signed minimums render with their bit pattern: {floor}"
    );
}

#[tokio::test]
async fn hover_constant_use_resolves_the_declaration() {
    let src = "const LIMIT: i32 = 5;\nfn main() { let x = LIMIT; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    // the use in main, not the declaration
    let text = client.hover_text(&url, position_of_nth(src, "LIMIT", 1)).await;
    assert!(
        text.contains("const LIMIT: i32 = 5"),
        "a spliced constant use hovers as its declaration: {text}"
    );

    let response = client
        .request::<request::GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: url.clone() },
                position: position_of_nth(src, "LIMIT", 1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();
    let Some(GotoDefinitionResponse::Scalar(location)) = response else {
        panic!("expected the constant declaration, got {response:?}");
    };
    assert_eq!(location.range.start.line, 0, "must jump to the declaration line");
}

#[tokio::test]
async fn hover_outside_any_declaration_is_none() {
    let src = "fn main() { let x = 1; }    ";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let hover = client.hover(&url, position_of(src, "   ")).await;
    assert!(hover.is_none(), "no hover target there: {hover:?}");
}

#[tokio::test]
async fn inlay_hints_appear_for_unannotated_bindings() {
    let src = "fn main() { let x = 232; let y: bool = true; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let hints = client.inlay_hints_fresh(&url).await;
    let labels: Vec<_> = hints.iter().map(label).collect();
    assert!(labels.contains(&": i32".to_string()), "x needs a hint: {labels:?}");
    assert!(
        !labels.contains(&": bool".to_string()),
        "y is already annotated, no hint: {labels:?}"
    );
}

#[tokio::test]
async fn inlay_hints_track_edits() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "fn main() { let x = 232; }").await;
    client.wait_diagnostics(&url).await;
    assert_eq!(labels_of(&client.inlay_hints_fresh(&url).await), vec![": i32"]);

    client.change(&url, "fn main() { let x = true; }").await;
    client.wait_diagnostics(&url).await;
    assert_eq!(
        labels_of(&client.inlay_hints_fresh(&url).await),
        vec![": bool"],
        "hints must reflect the edited buffer"
    );
}

#[tokio::test]
async fn rapid_edits_settle_on_the_last_content() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "fn main() { let x = 1; }").await;

    client.change(&url, "fn main() { let x = ").await;
    client.change(&url, "fn main() { let x = 2.5").await;
    client.change(&url, "fn main() { let x = 2.5; }").await;

    client.wait_diagnostics(&url).await;
    assert_eq!(
        labels_of(&client.inlay_hints_fresh(&url).await),
        vec![": f64"],
        "only the final buffer content may win"
    );
}

#[tokio::test]
async fn multiple_independent_errors_are_all_published() {
    let src =
        "fn first(): i32 { true }\nfn second() { let c: bool = 99; }\nfn main() { let x = 1; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;

    let diagnostics = client.wait_diagnostics(&url).await;
    assert_eq!(diagnostics.len(), 2, "both function errors must surface: {diagnostics:#?}");
}

#[tokio::test]
async fn features_stay_alive_on_broken_code() {
    let src = "fn broken(a: Nonexistent) { }\nfn main() { let x = 232; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;

    let diagnostics = client.wait_diagnostics(&url).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert!(diagnostics[0].message.contains("unknown type Nonexistent"));

    let hints = client.inlay_hints_fresh(&url).await;
    assert_eq!(labels_of(&hints), vec![": i32"], "hints must survive the broken sibling");

    let text = client.hover_text(&url, position_of(src, "broken")).await;
    assert!(
        text.contains("fn broken(a: {unknown})"),
        "the broken signature still hovers, poisoned: {text}"
    );
}

#[tokio::test]
async fn parse_error_keeps_last_good_hover_and_reports() {
    let good = "fn main() { let x = 232; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", good).await;
    client.wait_diagnostics(&url).await;

    client.change(&url, "fn main() { let x = ").await;
    let diagnostics = client.wait_diagnostics(&url).await;
    assert!(!diagnostics.is_empty(), "the parse error must be reported");

    let text = client.hover_text(&url, position_of(good, "x")).await;
    assert!(text.contains("i32"), "hover keeps serving the last good analysis: {text}");

    let stale = client.inlay_hints(&url).await;
    assert_eq!(
        stale.unwrap_err()["code"],
        serde_json::json!(CONTENT_MODIFIED),
        "hints must not be served from a stale analysis"
    );
}

#[tokio::test]
async fn diagnostics_clear_after_a_fix() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "fn main() { let x: bool = 232; }").await;
    assert!(!client.wait_diagnostics(&url).await.is_empty());

    client.change(&url, "fn main() { let x: i32 = 232; }").await;
    assert!(
        client.wait_diagnostics(&url).await.is_empty(),
        "fixing the buffer must clear its diagnostics"
    );
}

#[tokio::test]
async fn empty_buffer_analyses_clean() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "").await;
    assert!(client.wait_diagnostics(&url).await.is_empty(), "an empty module is valid");
}

#[tokio::test]
async fn goto_definition_resolves_a_local() {
    let src = "fn main() { let value = 1; let twice = value + value; }";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let response = client
        .request::<request::GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: url.clone() },
                position: position_of_nth(src, "value", 1),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    let Some(GotoDefinitionResponse::Scalar(location)) = response else {
        panic!("expected a single definition, got {response:?}");
    };
    assert_eq!(location.uri, url);
    assert_eq!(location.range.start, position_of(src, "value"), "must point at the declaration");
}

#[tokio::test]
async fn document_symbols_list_declarations() {
    let src = "struct Point { x: i64, y: i64 }\nfn helper() {}\nfn main() {}";
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let response = client
        .request::<request::DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: url.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    let Some(DocumentSymbolResponse::Flat(symbols)) = response else {
        panic!("expected flat symbols, got {response:?}");
    };
    let names: Vec<_> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    for expected in ["Point", "helper", "main"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
}

#[tokio::test]
async fn semantic_tokens_cover_the_buffer() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "fn main() { let x = 232; }").await;

    let response = client
        .request::<request::SemanticTokensFullRequest>(SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri: url.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .unwrap();

    let Some(SemanticTokensResult::Tokens(tokens)) = response else {
        panic!("expected tokens, got {response:?}");
    };
    assert!(!tokens.data.is_empty(), "the buffer has keywords and literals to highlight");
}

#[tokio::test]
async fn common_client_requests_never_yield_method_not_found() {
    let mut client = TestClient::start().await;
    let url = client.open("main.nyx", "fn main() {}").await;

    let position = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: url.clone() },
        position: Position::new(0, 0),
    };

    let completion = client
        .request::<request::Completion>(CompletionParams {
            text_document_position: position.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .await;
    assert_eq!(completion.unwrap(), None, "completion must answer empty, not -32601");

    let help = client
        .request::<request::SignatureHelpRequest>(SignatureHelpParams {
            text_document_position_params: position,
            work_done_progress_params: Default::default(),
            context: None,
        })
        .await;
    assert_eq!(help.unwrap(), None, "signature help must answer empty, not -32601");

    let actions = client
        .request::<request::CodeActionRequest>(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: url.clone() },
            range: Range::default(),
            context: CodeActionContext::default(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await;
    assert_eq!(actions.unwrap(), None, "code actions must answer empty, not -32601");
}

#[tokio::test]
async fn utf16_positions_resolve_after_multibyte_text() {
    // the emoji needs two UTF-16 units, so byte and UTF-16 columns diverge
    // for everything after it on the same line
    let src = "fn main() { let emoji = \"😀\"; let after = 1; }";
    let mut client = TestClient::start_with(None).await;
    let url = client.open("main.nyx", src).await;
    client.wait_diagnostics(&url).await;

    let text = client.hover_text(&url, position_of_utf16(src, "after")).await;
    assert!(text.contains("i32"), "UTF-16 column must land on the binding: {text}");
}

#[tokio::test]
async fn diagnostics_route_to_the_file_that_owns_them() {
    let mut client = TestClient::start().await;

    let util = "pub fn helper(): i32 { true }";
    let util_url = client.open("util.nyx", util).await;
    let main = "use project::util::{helper};\nfn main() { let x = helper(); }";
    let url = client.open("main.nyx", main).await;

    let diagnostics = client.wait_diagnostics(&util_url).await;
    assert_eq!(diagnostics.len(), 1, "the mismatch belongs to util.nyx: {diagnostics:#?}");
    assert!(diagnostics[0].message.contains("type mismatch"));

    client.wait_diagnostics(&url).await;
    let hints = client.inlay_hints_fresh(&url).await;
    assert_eq!(labels_of(&hints), vec![": i32"], "main.nyx features survive util's error");
}

fn label(hint: &InlayHint) -> String {
    match &hint.label {
        InlayHintLabel::String(label) => label.clone(),
        InlayHintLabel::LabelParts(parts) => parts.iter().map(|part| part.value.as_str()).collect(),
    }
}

fn labels_of(hints: &[InlayHint]) -> Vec<String> {
    hints.iter().map(label).collect()
}
