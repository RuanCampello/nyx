use super::hover::HoverInfo;
use super::*;
use rstest::rstest;

fn rendered(a: &SemanticAnalysis) -> Vec<(Span, HoverInfo)> {
    a.with_snapshot(|snapshot| {
        let Some(snapshot) = snapshot else {
            return Vec::new();
        };
        snapshot
            .hover_types
            .iter()
            .filter_map(|&(span, target)| Some((span, snapshot.hover(target, &a.source_map)?)))
            .collect()
    })
}

fn document_symbol_names(a: &SemanticAnalysis) -> Vec<String> {
    a.with_snapshot(|snapshot| {
        snapshot
            .map(|s| s.document_symbols.iter().map(|symbol| symbol.name.clone()).collect())
            .unwrap_or_default()
    })
}

fn hints(a: &SemanticAnalysis) -> Vec<String> {
    a.with_snapshot(|snapshot| {
        let Some(snapshot) = snapshot else {
            return Vec::new();
        };
        snapshot
            .inlay_hints
            .iter()
            .map(|&(_, typ, at)| snapshot.hint(typ, at))
            .collect()
    })
}

fn analyse(tag: &str, content: &str) -> SemanticAnalysis {
    let entry = std::env::temp_dir().join(format!("nyx_analysis_{tag}.nyx"));
    std::fs::write(&entry, "").unwrap();
    let entry = std::fs::canonicalize(&entry).unwrap();
    let mut analysis = Analysis::new(entry.clone()).with_overlay(entry.clone(), content).run();
    let source_map = &analysis.source_map;
    analysis.diagnostics.retain(|diagnostic| {
        diagnostic.primary.as_ref().is_none_or(|label| {
            let file = source_map.span_data(label.span).file;
            source_map.path(file) == entry
        })
    });
    std::fs::remove_file(&entry).ok();

    analysis
}

#[test]
fn array_hint_renders_element_and_length() {
    let a = analyse("array_render", "fn main() { let arr = [0; 3]; let l = arr.len(); }");
    let hints = hints(&a);
    assert!(hints.iter().any(|h| h == "[i32; 3]"), "array renders as `[i32; 3]`: {hints:?}");
    assert!(hints.iter().any(|h| h == "uptr"), "len() result is uptr: {hints:?}");
}

#[test]
fn array_element_infers_from_later_assignment() {
    let a = analyse(
        "array_infer",
        "fn main() { let mut arr = [0; 3]; let p: uptr = 1; arr[0] = p; }",
    );
    let hints = hints(&a);
    assert!(hints.iter().any(|h| h == "[uptr; 3]"), "element infers to uptr: {hints:?}");
}

#[test]
fn array_element_infers_from_len_assignment() {
    let a = analyse(
        "array_len_assign",
        "fn main() { let mut arr = [0; 3]; arr[2] = arr.len(); let s = arr[0]; }",
    );
    assert!(a.diagnostics.is_empty(), "no type mismatch: {:?}", a.diagnostics);
    let hints = hints(&a);
    assert!(
        hints.iter().any(|h| h == "[uptr; 3]"),
        "element infers to uptr from len(): {hints:?}"
    );
}

#[test]
fn valid_buffer_analyses_with_hints() {
    let a = analyse("valid", "fn main() { let x = 232; }");
    assert!(a.ok, "valid source must analyse into HIR");
    assert!(a.diagnostics.is_empty());
    assert!(hints(&a).iter().any(|ty| ty == "i32"), "expected the `: i32` hint");
}

#[test]
fn enum_payload_can_reference_a_later_struct() {
    let a = analyse(
        "forward_enum_payload",
        r#"
            enum Msg { ChangeColour(Colour) }
            struct Colour { r: u8, g: u8, b: u8 }
            fn main() { }
        "#,
    );

    assert!(a.ok, "{:#?}", a.diagnostics);
    assert!(a.diagnostics.is_empty(), "{:#?}", a.diagnostics);
}

#[test]
fn direct_qualified_std_call_has_no_diagnostics() {
    let a = analyse("qualified_std", "fn main() { std::io::println(\"ok\"); }");

    assert!(a.ok, "{:#?}", a.diagnostics);
    assert!(a.diagnostics.is_empty(), "{:#?}", a.diagnostics);
}

#[test]
fn broken_buffer_still_reports_and_keeps_features() {
    let a = analyse("broken", "fn main() { let x = 1; let y = ");
    assert!(a.ok, "a syntax error is recovered, not fatal");
    assert!(!a.diagnostics.is_empty(), "the error is still reported");
    assert!(
        hints(&a).iter().any(|ty| ty == "i32"),
        "the sound binding keeps its hint: {:?}",
        hints(&a)
    );
}

#[test]
fn a_syntax_error_does_not_hide_the_rest_of_the_file() {
    let a = analyse(
        "syntax_then_type",
        r#"
        struct Point {
            /// the horizontal coordinate
            x: i32,
            y: i32,
        }
        fn broken(: i32 { 1 }
        fn typed(): i32 { true }
        fn main() { let p = Point { x: 1, y: 2 }; }
        "#,
    );

    assert!(a.ok, "{:#?}", a.diagnostics);
    let messages: Vec<_> = a.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("Expected an identifier")),
        "the syntax error is reported: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("does not match the declared type")),
        "the type error after it is reported too: {messages:?}"
    );
    assert!(
        rendered(&a).iter().any(|(_, h)| h.ty.contains("struct Point")),
        "the struct still hovers"
    );
    assert!(
        document_symbol_names(&a).iter().any(|name| name == "main"),
        "the outline still lists main: {:?}",
        document_symbol_names(&a)
    );
}

#[test]
fn an_unterminated_body_keeps_the_following_function_analysable() {
    let a = analyse(
        "unterminated",
        "fn unfinished() { let z = 1; let w =\n\nfn main() { let total = 1 + 2; }",
    );

    assert!(a.ok, "{:#?}", a.diagnostics);
    assert!(!a.diagnostics.is_empty(), "the broken binding is reported");
    assert!(
        document_symbol_names(&a).iter().any(|name| name == "main"),
        "main is still an item of its own: {:?}",
        document_symbol_names(&a)
    );
    assert!(
        hints(&a).iter().any(|ty| ty == "i32"),
        "and its bindings still get hints: {:?}",
        hints(&a)
    );
}

#[test]
fn diagnostics_come_back_in_source_order() {
    let a = analyse(
        "ordered",
        "fn one(): i32 { true }\nfn two(): i32 { true }\nfn three(): i32 { true }",
    );

    let starts: Vec<_> = a
        .diagnostics
        .iter()
        .filter_map(|d| d.primary.as_ref().map(|label| label.span.start.0))
        .collect();
    assert_eq!(starts.len(), 3, "one per function: {:?}", a.diagnostics);
    assert!(starts.is_sorted(), "reported top to bottom: {starts:?}");
}

#[test]
fn a_std_entry_reports_every_error_in_its_own_bodies() {
    let entry = std::fs::canonicalize("../std/alloc.nyx").expect("std/alloc.nyx must exist");
    let mut content = std::fs::read_to_string(&entry).expect("readable");
    content.push_str("\nfn first(): i32 { true }\nfn second() { nope(); }\n");

    let a = Analysis::new(entry.clone()).with_overlay(entry, content).run();
    let messages: Vec<_> = a.diagnostics.iter().map(|d| d.message.as_str()).collect();

    assert!(a.ok, "{messages:?}");
    assert!(
        messages.iter().any(|m| m.contains("does not match the declared type")),
        "a std entry gets its bodies checked: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("Cannot find function")),
        "and every later error too: {messages:?}"
    );
}

#[test]
fn unknown_param_type_keeps_features_alive() {
    let a = analyse("param", "fn poisoned(a: Nonexistent): i32 { 1 }\nfn main() { let x = 232; }");
    assert!(a.ok, "recovery must still produce a HIR with live features");
    assert_eq!(a.diagnostics.len(), 1, "exactly the unknown type: {:?}", a.diagnostics);
    assert!(hints(&a).iter().any(|ty| ty == "i32"), "main still gets its hint");
    assert!(
        rendered(&a).iter().any(|(_, h)| h.ty.contains("fn poisoned")),
        "the poisoned function still hovers as a signature"
    );
}

#[test]
fn errors_in_two_functions_are_both_reported() {
    let a = analyse(
        "two_fns",
        r#"
        fn first(): i32 { true }
        fn second() { let x: bool = 232; }
        fn main() { let y = 1; }
        "#,
    );
    assert!(a.ok, "recovery must still produce a HIR with live features");
    assert_eq!(a.diagnostics.len(), 2, "one error per function: {:?}", a.diagnostics);
    assert!(hints(&a).iter().any(|ty| ty == "i32"), "main still gets its hint");
}

#[test]
fn unknown_struct_field_type_still_registers_the_struct() {
    let a = analyse(
        "struct_field",
        r#"
        struct Holder { value: Missing, count: i32 }
        fn main() { let h = 1; }
        "#,
    );
    assert!(a.ok, "recovery must still produce a HIR with live features");
    assert_eq!(a.diagnostics.len(), 1, "{:?}", a.diagnostics);
    assert!(
        rendered(&a).iter().any(|(_, h)| h.ty.contains("struct Holder")),
        "the struct must survive a poisoned field"
    );
    assert!(
        document_symbol_names(&a).iter().any(|name| name == "Holder"),
        "the outline still lists the struct"
    );
}

#[test]
fn doc_comments_surface_on_item_hover() {
    let a = analyse(
        "docs",
        r#"
        /// Adds two numbers.
        fn add(a: i32, b: i32): i32 { a + b }

        /// A 2D point.
        struct Point {
            /// the horizontal coordinate
            x: i32,
            y: i32,
        }

        /// The answer.
        const ANSWER: i32 = 42;

        fn main() {
            let p = Point { x: 1, y: 2 };
            let _ = add(p.x, p.y) + ANSWER;
        }
        "#,
    );
    assert!(a.ok, "{:?}", a.diagnostics);

    let hovers = rendered(&a);
    let doc_of = |needle: &str| {
        hovers
            .iter()
            .find(|(_, hover)| hover.ty.contains(needle))
            .and_then(|(_, hover)| hover.docs.as_deref())
    };

    assert_eq!(doc_of("fn add"), Some("Adds two numbers."));
    assert_eq!(doc_of("struct Point"), Some("A 2D point."));
    assert_eq!(doc_of("const ANSWER"), Some("The answer."));
    assert_eq!(doc_of("fn main"), None, "an undocumented item has no docs");
}

#[test]
fn impl_method_docs_surface_on_hover() {
    let a = analyse(
        "impl_docs",
        r#"
        struct Point { x: i32 }
        impl Point {
            /// the horizontal coordinate
            fn get(&self): i32 { self.x }
        }
        fn main() {
            let p = Point { x: 1 };
            let _ = p.get();
        }
        "#,
    );
    assert!(a.ok, "{:?}", a.diagnostics);

    let hovers = rendered(&a);
    let doc = hovers
        .iter()
        .find(|(_, hover)| hover.ty.contains("fn get"))
        .and_then(|(_, hover)| hover.docs.as_deref());
    assert_eq!(doc, Some("the horizontal coordinate"));
}

#[test]
fn fieldless_enums_auto_size_while_payload_enums_keep_the_tag() {
    let a = analyse(
        "enum_repr",
        r#"
        enum Direction { North, West, East, South }
        enum Tiny { No, Yes(bool) }
        fn main() {
            let _ = Direction::North;
            let _ = Tiny::No;
        }
        "#,
    );
    assert!(a.ok, "{:?}", a.diagnostics);

    let layout_of = |needle: &str| {
        rendered(&a)
            .iter()
            .find_map(|(_, hover)| hover.ty.contains(needle).then_some(hover.layout))
            .flatten()
    };

    assert_eq!(layout_of("enum Direction"), Some((1, 1)));
    assert_eq!(layout_of("enum Tiny"), Some((8, 4)));
}

#[test]
fn broken_initialiser_keeps_the_binding_alive() {
    let a = analyse("broken_init", "fn main() { let d = unknown_fn(); let e = d; }");
    assert!(a.ok, "recovery must still produce a HIR with live features");
    assert_eq!(a.diagnostics.len(), 1, "only the unknown call, once: {:?}", a.diagnostics);
    assert!(
        hints(&a).iter().any(|ty| ty == "{unknown}"),
        "d stays declared with a poison hint: {:?}",
        hints(&a)
    );
}

#[test]
fn duplicate_functions_report_without_killing_analysis() {
    let a = analyse("dup_fn", "fn twice() {}\nfn twice() {}\nfn main() { let z = 42; }");
    assert!(a.ok, "recovery must still produce a HIR with live features");
    assert!(!a.diagnostics.is_empty());
    assert!(hints(&a).iter().any(|ty| ty == "i32"), "main still gets its hint");
}

fn entry_origin(a: &SemanticAnalysis, source: &str) -> u32 {
    a.source_map
        .files()
        .find(|file| file.src == source)
        .map(|file| file.start_pos.0)
        .expect("the analysed buffer is registered")
}

fn text_at(origin: u32, source: &str, span: Span) -> Option<&str> {
    let (start, end) = (span.start.0.checked_sub(origin)?, span.end.0.checked_sub(origin)?);
    source.get(start as usize..end as usize)
}

/// The tightest hover covering exactly `needle`, so a member wins over the
/// declaration that contains it
fn hover_on(a: &SemanticAnalysis, source: &str, needle: &str) -> HoverInfo {
    let origin = entry_origin(a, source);
    a.with_snapshot(|snapshot| {
        snapshot
            .and_then(|snapshot| {
                snapshot
                    .hover_types
                    .iter()
                    .filter(|(span, _)| text_at(origin, source, *span) == Some(needle))
                    .min_by_key(|(span, _)| span.end.0 - span.start.0)
                    .and_then(|&(_, target)| snapshot.hover(target, &a.source_map))
            })
            .unwrap_or_else(|| panic!("nothing hovers `{needle}`"))
    })
}

fn hover_nth(a: &SemanticAnalysis, source: &str, needle: &str, nth: usize) -> HoverInfo {
    let origin = entry_origin(a, source);
    a.with_snapshot(|snapshot| {
        snapshot
            .and_then(|snapshot| {
                let mut hits: Vec<_> = snapshot
                    .hover_types
                    .iter()
                    .filter(|(span, _)| text_at(origin, source, *span) == Some(needle))
                    .collect();
                hits.sort_by_key(|(span, _)| (span.start.0, span.end.0 - span.start.0));

                let &&(_, target) = hits.get(nth)?;
                snapshot.hover(target, &a.source_map)
            })
            .unwrap_or_else(|| panic!("nothing hovers occurrence {nth} of `{needle}`"))
    })
}

/// The text the definition of `needle` lands on, `<std>` when it leaves the buffer
fn definition_of(a: &SemanticAnalysis, source: &str, needle: &str) -> String {
    let origin = entry_origin(a, source);
    a.with_snapshot(|snapshot| {
        let (_, target) = snapshot
            .unwrap_or_else(|| panic!("`{needle}` has no definition"))
            .goto_definitions
            .iter()
            .filter(|(use_span, _)| text_at(origin, source, **use_span) == Some(needle))
            .min_by_key(|(use_span, _)| use_span.end.0 - use_span.start.0)
            .unwrap_or_else(|| panic!("`{needle}` has no definition"));

        text_at(origin, source, *target).unwrap_or("<std>").to_owned()
    })
}

const RICH: &str = r#"
use std::mem::{size_of};

/// A documented interface.
interface Shape {
    /// the area of the shape
    fn area(&self): i32;
}

/// A point in space.
struct Point {
    /// the horizontal coordinate
    x: i32,
    y: i32,
}

/// The kind of message.
enum Msg {
    /// nothing to say
    Quiet,
    /// shouting, with a volume
    Loud(i32),
}

impl Point {
    /// make a point
    fn origin(): Point { Point { x: 0, y: 0 } }
}

impl Point with Shape {
    fn area(&self): i32 { self.x * self.y }
}

@unsafe
fn danger(): i32 { 7 }

fn take(p: Point, m: Msg): i32 { p.x }

fn main() {
    let p = Point::origin();
    let total = p.area();
    let m = Msg::Loud(3);
    let size = size_of(i32);
}
"#;

#[test]
fn struct_fields_hover_with_their_docs() {
    let a = analyse("field_hover", RICH);
    assert!(a.ok, "{:?}", a.diagnostics);

    let x = hover_on(&a, RICH, "x");
    assert_eq!(x.ty, "x: i32");
    assert_eq!(x.docs.as_deref(), Some("the horizontal coordinate"));
    assert_eq!(x.layout, Some((4, 4)), "a field carries its own layout");
    assert!(x.path.as_deref().is_some_and(|p| p.ends_with("::Point")), "{:?}", x.path);
}

#[test]
fn a_field_access_reaches_the_field_declaration() {
    let a = analyse("field_access", RICH);

    let access = hover_on(&a, RICH, "p.x");
    assert_eq!(access.ty, "x: i32", "the access shows the field, not just its type");
    assert_eq!(access.docs.as_deref(), Some("the horizontal coordinate"));
    assert_eq!(definition_of(&a, RICH, "p.x"), "x", "and jumps to the field's name");
}

#[test]
fn enum_variants_hover_at_their_declaration_and_use() {
    let a = analyse("variant_hover", RICH);

    let quiet = hover_on(&a, RICH, "Quiet");
    assert_eq!(quiet.ty, "Msg::Quiet = 0", "a fieldless variant shows its discriminant");
    assert_eq!(quiet.docs.as_deref(), Some("nothing to say"));

    let used = hover_on(&a, RICH, "Msg::Loud(3)");
    assert_eq!(used.ty, "Msg::Loud(i32)", "a use shows the payload type");
    assert_eq!(used.docs.as_deref(), Some("shouting, with a volume"));
    assert_eq!(definition_of(&a, RICH, "Msg::Loud(3)"), "Loud");
}

#[test]
fn interfaces_and_their_methods_hover() {
    let a = analyse("interface_hover", RICH);

    let shape = hover_nth(&a, RICH, "Shape", 0);
    assert_eq!(shape.ty, "interface Shape {\n    fn area(&self): i32;\n}");
    assert_eq!(shape.docs.as_deref(), Some("A documented interface."));

    let area = hover_on(&a, RICH, "area");
    assert_eq!(area.ty, "interface Shape\nfn area(&self): i32");
    assert_eq!(area.docs.as_deref(), Some("the area of the shape"));
}

#[test]
fn a_type_annotation_reaches_its_declaration() {
    let a = analyse("type_ref", RICH);

    let point = hover_on(&a, RICH, "Point");
    assert!(point.ty.starts_with("struct Point {"), "got {}", point.ty);
    assert_eq!(point.docs.as_deref(), Some("A point in space."));
    assert_eq!(definition_of(&a, RICH, "Point"), "Point", "and jumps to the declared name");
}

#[test]
fn an_import_reaches_the_item_it_names() {
    let a = analyse("import_hover", RICH);

    let import = hover_on(&a, RICH, "size_of");
    assert!(
        import.ty.contains("fn size_of"),
        "the import shows the signature: {}",
        import.ty
    );
    assert_eq!(
        definition_of(&a, RICH, "size_of"),
        "<std>",
        "and jumps into the std module that declares it"
    );
}

#[test]
fn markers_sit_above_the_signature_they_annotate() {
    let a = analyse("marker_hover", RICH);

    let danger = hover_on(&a, RICH, "danger");
    assert_eq!(danger.ty, "@unsafe\nfn danger(): i32");
}

#[test]
fn destructuring_a_variant_hints_the_payload() {
    let source = r#"
        enum Msg { Quiet, Loud(i32), Named(Point) }
        struct Point { x: i32, y: i32 }
        fn describe(m: Msg): i32 {
            match m {
                Msg::Loud(volume) -> volume,
                Msg::Named(Point { x, y }) -> x + y,
                other -> 0,
            }
        }
        fn main() { let _ = describe(Msg::Quiet); }
    "#;
    let a = analyse("destructure", source);
    assert!(a.ok, "{:?}", a.diagnostics);

    let origin = entry_origin(&a, source);
    let hint_on = |needle: &str| {
        a.with_snapshot(|snapshot| {
            let snapshot = snapshot?;
            snapshot
                .inlay_hints
                .iter()
                .find(|&&(span, ..)| text_at(origin, source, span) == Some(needle))
                .map(|&(_, typ, at)| snapshot.hint(typ, at))
        })
    };

    assert_eq!(hint_on("volume").as_deref(), Some("i32"), "a payload binding is hinted");
    assert_eq!(hint_on("x").as_deref(), Some("i32"), "and so is a nested struct field binding");
    assert_eq!(
        hint_on("other").as_deref(),
        Some("Msg"),
        "a catch-all binds the scrutinee itself"
    );
}

fn offered(a: &SemanticAnalysis, source: &str, cursor: &str) -> Vec<String> {
    use crate::feature::completion;

    let offset = source.find(cursor).expect("the cursor marker") + cursor.len();
    let context = completion::context_at(source, offset);
    let position = frontend::BytePos(entry_origin(a, source) + offset as u32);

    a.completion_candidates(&context, Some(position), |candidates| {
        candidates.items.iter().map(|item| item.label.to_owned()).collect()
    })
}

#[test]
fn a_dot_offers_fields_and_methods_of_the_receiver() {
    let a = analyse("complete_member", RICH);
    let offered = offered(&a, RICH, "let total = p.");

    assert!(offered.contains(&"x".to_owned()), "fields are offered: {offered:?}");
    assert!(offered.contains(&"y".to_owned()), "{offered:?}");
    assert!(offered.contains(&"area".to_owned()), "methods are offered: {offered:?}");
    assert!(!offered.contains(&"origin".to_owned()), "an associated fn is not: {offered:?}");
}

#[test]
fn a_type_qualifier_offers_its_associated_items() {
    let a = analyse("complete_assoc", RICH);

    let on_point = offered(&a, RICH, "let p = Point::");
    assert!(on_point.contains(&"origin".to_owned()), "{on_point:?}");
    assert!(!on_point.contains(&"x".to_owned()), "a field is not associated: {on_point:?}");

    let on_msg = offered(&a, RICH, "let m = Msg::");
    assert!(on_msg.contains(&"Quiet".to_owned()), "{on_msg:?}");
    assert!(on_msg.contains(&"Loud".to_owned()), "{on_msg:?}");
}

#[test]
fn a_module_path_offers_its_exports() {
    let a = analyse("complete_module", RICH);
    let offered = offered(&a, RICH, "use std::mem::");

    assert!(offered.contains(&"size_of".to_owned()), "{offered:?}");
}

#[test]
fn a_root_offers_its_submodules() {
    let a = analyse("complete_submodule", RICH);

    let under_std = offered(&a, RICH, "use std::");
    assert!(under_std.contains(&"io".to_owned()), "an unimported module: {under_std:?}");
    assert!(under_std.contains(&"mem".to_owned()), "{under_std:?}");

    let roots = offered(&a, RICH, "    let size = ");
    assert!(roots.contains(&"std".to_owned()), "the root itself is nameable: {roots:?}");
}

#[test]
fn unimported_standard_functions_are_not_offered_unqualified() {
    let source = "fn main() { let value = pri; }";
    let a = analyse("complete_unimported_std", source);
    let offered = offered(&a, source, "let value = pri");

    assert!(!offered.contains(&"print".to_owned()), "print needs an import: {offered:?}");
    assert!(!offered.contains(&"println".to_owned()), "println needs an import: {offered:?}");
}

#[test]
fn imported_standard_functions_are_offered_unqualified() {
    let source = "use std::io::{print};\nfn main() { let value = pri; }";
    let a = analyse("complete_imported_std", source);
    let offered = offered(&a, source, "let value = pri");

    assert!(offered.contains(&"print".to_owned()), "the imported name is open: {offered:?}");
    assert!(
        !offered.contains(&"println".to_owned()),
        "other exports stay qualified: {offered:?}"
    );
}

#[test]
fn a_module_path_offers_the_types_it_exports() {
    let a = analyse("complete_module_type", RICH);
    let exports: Vec<String> = a.with_snapshot(|snapshot| {
        snapshot.unwrap().completions.associated["std::optional"]
            .iter()
            .map(|item| item.label.to_owned())
            .collect()
    });

    assert!(
        exports.iter().any(|label| label == "Optional"),
        "a type is reachable through its module: {exports:?}"
    );
}

#[test]
fn an_intrinsic_method_completes_and_hovers() {
    let source = "fn main() { let s = \"nyx\"; let n = s.len(); }";
    let a = analyse("intrinsic", source);
    assert!(a.ok, "{:?}", a.diagnostics);

    let offered = offered(&a, source, "let n = s.");
    assert!(offered.contains(&"len".to_owned()), "len is offered on a str: {offered:?}");

    let signature = a
        .with_snapshot(|snapshot| {
            snapshot
                .unwrap()
                .completions
                .members
                .get("str")
                .and_then(|items| items.iter().find(|item| item.label == "len"))
                .map(|item| item.detail.to_owned())
        })
        .expect("str::len is indexed");
    assert!(signature.starts_with("@intrinsic\n"), "the marker is shown: {signature}");
    assert!(signature.contains("fn len(&self): uptr"), "{signature}");
}

#[test]
fn a_generic_signature_names_its_parameters_as_declared() {
    let a = analyse("generic_render", "use std::ptr;\nfn main() { }");
    let rendered = a
        .with_snapshot(|snapshot| {
            snapshot
                .unwrap()
                .completions
                .associated
                .get("std::ptr")
                .expect("std::ptr is indexed")
                .iter()
                .find(|item| item.label == "add_mut")
                .map(|item| item.detail.to_owned())
        })
        .expect("std::ptr::add_mut is indexed");

    assert!(
        rendered.contains("fn add_mut<T>(p: *mut T, count: uptr): *mut T"),
        "generics read back as written, not as the mangler numbered them: {rendered}"
    );
}

#[test]
fn an_unqualified_position_offers_locals_and_globals() {
    let a = analyse("complete_open", RICH);
    let offered = offered(&a, RICH, "    let size = ");

    assert!(offered.contains(&"p".to_owned()), "a local in the same body: {offered:?}");
    assert!(offered.contains(&"Point".to_owned()), "a type: {offered:?}");
    assert!(offered.contains(&"describe".to_owned()) || offered.contains(&"take".to_owned()));
    assert!(offered.contains(&"Shape".to_owned()), "an interface: {offered:?}");
}

#[test]
fn locals_of_another_body_are_not_offered() {
    let a = analyse("complete_scope", RICH);
    let outside = offered(&a, RICH, "fn take(p: Point, m: Msg): i32 { p");

    assert!(
        !outside.contains(&"total".to_owned()),
        "a local of main must not leak into take: {outside:?}"
    );
}

#[test]
fn a_binding_hovers_as_the_declaration_it_was_written_as() {
    let source = r#"
        struct Point { x: i32, y: i32 }
        fn take(origin: Point): i32 {
            let mut total = 0;
            let fixed = origin.x;
            total = total + fixed;
            total
        }
        fn main() { let _ = take(Point { x: 1, y: 2 }); }
    "#;
    let a = analyse("binding_hover", source);
    assert!(a.ok, "{:?}", a.diagnostics);

    let total = hover_on(&a, source, "total");
    assert_eq!(total.ty, "let mut total: i32", "mutability is part of the declaration");
    assert_eq!(total.layout, Some((4, 4)), "with its size and alignment");

    assert_eq!(hover_on(&a, source, "fixed").ty, "let fixed: i32");
    assert_eq!(hover_on(&a, source, "origin").ty, "origin: Point", "a parameter has no let");
}

#[test]
fn an_interface_implementation_names_the_interface_it_satisfies() {
    let a = analyse("impl_iface", RICH);

    let area = hover_nth(&a, RICH, "area", 1);
    assert_eq!(area.ty, "impl Point with Shape\nfn area(&self): i32");

    let origin = hover_on(&a, RICH, "origin");
    assert_eq!(origin.ty, "impl Point\nfn origin(): Point", "a plain impl names no interface");
}

#[test]
fn definitions_land_on_the_name_not_the_keyword() {
    let a = analyse("goto_name", RICH);

    assert_eq!(definition_of(&a, RICH, "Point::origin()"), "origin");
    assert_eq!(definition_of(&a, RICH, "p.area()"), "area");
}

#[test]
fn a_generic_impl_names_its_receiver_type() {
    let source = r#"
        struct Holder<T> { value: T }

        impl Holder<T> {
            fn get(&self): T { self.value }
        }

        fn main() {
            let h = Holder { value: 1 };
            let v = h.get();
        }
    "#;
    let a = analyse("generic_owner", source);
    assert!(a.ok, "{:?}", a.diagnostics);

    let got = hover_on(&a, source, "get");
    assert!(
        got.ty.starts_with("impl Holder<T>\n"),
        "the receiver type names the block, not a mangled segment: {}",
        got.ty
    );
}

#[test]
fn every_recorded_target_still_resolves() {
    let a = analyse("targets_resolve", RICH);
    assert!(a.ok, "{:?}", a.diagnostics);

    let hover_type_count = a.with_snapshot(|snapshot| snapshot.map_or(0, |s| s.hover_types.len()));
    assert!(hover_type_count != 0, "the fixture records hovers");
    assert_eq!(
        rendered(&a).len(),
        hover_type_count,
        "every span a walk recorded must resolve against the index it was built with"
    );
}

#[test]
fn a_hover_target_stays_a_handle() {
    // the walk records one of these per expression: it must stay a plain
    // handle, never grow a field that has to be rendered or allocated
    assert!(
        size_of::<HoverTarget<'_>>() <= 24,
        "a target is {} bytes, it should stay a handle",
        size_of::<HoverTarget<'_>>()
    );
}

/// a constant that cannot be folded is rendered without a value rather than wrongly
#[rstest]
#[case::sum("SUM", "3 + 4", Some("7"))]
#[case::product("PRODUCT", "3 * 4", Some("12"))]
#[case::quotient("QUOTIENT", "17 / 5", Some("3"))]
#[case::remainder("REMAINDER", "17 % 5", Some("2"))]
#[case::negative_remainder("NEGATIVE", "-17 % 5", Some("-2 (0xFFFFFFFE)"))]
#[case::remainder_after_division("CHAINED", "100 / 7 % 5", Some("4"))]
#[case::remainder_by_zero("BY_ZERO", "17 % 0", None)]
#[case::division_by_zero("DIV_ZERO", "17 / 0", None)]
fn a_constant_hover_shows_its_folded_value(
    #[case] name: &str,
    #[case] expression: &str,
    #[case] expected: Option<&str>,
) {
    let source = format!("const {name}: i32 = {expression};\nfn main(): i32 {{ {name} }}\n");
    let analysis = analyse(&name.to_lowercase(), &source);

    let hover = rendered(&analysis)
        .into_iter()
        .find_map(|(_, hover)| hover.ty.starts_with("const ").then_some(hover.ty))
        .unwrap_or_else(|| panic!("nothing hovers the constant"));

    match expected {
        Some(value) => assert_eq!(hover, format!("const {name}: i32 = {value}")),
        None => assert_eq!(hover, format!("const {name}: i32")),
    }
}
