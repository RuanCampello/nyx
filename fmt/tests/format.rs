use fmt::{FormatOptions, format};
use indoc::indoc;
use std::str::FromStr;

fn assert_formats(source: &str, expected: &str) {
    assert_eq!(format(source, FormatOptions::default()).unwrap(), expected);
}

fn assert_formats_with(options: FormatOptions, source: &str, expected: &str) {
    assert_eq!(format(source, options).unwrap(), expected);
}

fn assert_idempotent(source: &str) {
    let once = format(source, FormatOptions::default()).unwrap();
    let twice = format(&once, FormatOptions::default()).unwrap();

    assert_eq!(once, twice, "formatting is not idempotent");
}

fn shorthand_enabled() -> FormatOptions {
    FormatOptions::from_str("[field]\ninitialise_short_hand = true\n").unwrap()
}

#[test]
fn normalises_a_function_signature() {
    assert_formats(
        indoc! {"
            fn   add( a:i32,b : i32):i32{a+b}
        "},
        indoc! {"
            fn add(a: i32, b: i32): i32 {
                a + b
            }
        "},
    );
}

#[test]
fn expands_an_empty_function_body() {
    assert_formats(
        indoc! {"
            fn main(){}
        "},
        indoc! {"
            fn main() {
            }
        "},
    );
}

#[test]
fn indents_function_body_statements() {
    assert_formats(
        indoc! {"
            fn main():i32{let answer=40+2;answer}
        "},
        indoc! {"
            fn main(): i32 {
                let answer = 40 + 2;
                answer
            }
        "},
    );
}

#[test]
fn separates_top_level_functions() {
    assert_formats(
        indoc! {"
            fn first():i32{1}
            fn second():i32{2}
        "},
        indoc! {"
            fn first(): i32 {
                1
            }

            fn second(): i32 {
                2
            }
        "},
    );
}

#[test]
fn indents_nested_blocks() {
    assert_formats(
        indoc! {"
            fn classify(value:i32):i32{if value>0{value}else{0}}
        "},
        indoc! {"
            fn classify(value: i32): i32 {
                if value > 0 {
                    value
                } else {
                    0
                }
            }
        "},
    );
}

#[test]
fn keeps_explicit_struct_fields_by_default() {
    assert_formats(
        indoc! {"
            fn main(){let x=1;let p=Point{x:x,y:2};p.x}
        "},
        indoc! {"
            fn main() {
                let x = 1;
                let p = Point { x: x, y: 2 };
                p.x
            }
        "},
    );
}

#[test]
fn collapses_a_matching_field_to_shorthand_when_enabled() {
    assert_formats_with(
        shorthand_enabled(),
        indoc! {"
            fn main(){let x=1;let p=Point{x:x,y:2};p.x}
        "},
        indoc! {"
            fn main() {
                let x = 1;
                let p = Point { x, y: 2 };
                p.x
            }
        "},
    );
}

#[test]
fn shorthand_needs_the_field_and_variable_to_share_a_name() {
    assert_formats_with(
        shorthand_enabled(),
        indoc! {"
            fn main(){let y=1;let p=Point{x:y};p.x}
        "},
        indoc! {"
            fn main() {
                let y = 1;
                let p = Point { x: y };
                p.x
            }
        "},
    );
}

#[test]
fn shorthand_never_applies_to_a_computed_value() {
    assert_formats_with(
        shorthand_enabled(),
        indoc! {"
            fn main(){let x=1;let p=Point{x:x+1};p.x}
        "},
        indoc! {"
            fn main() {
                let x = 1;
                let p = Point { x: x + 1 };
                p.x
            }
        "},
    );
}

#[test]
fn shorthand_output_does_not_parse_yet() {
    let source = "fn main(){let x=1;let p=Point{x:x};p.x}\n";
    let collapsed = format(source, shorthand_enabled()).unwrap();

    assert!(collapsed.contains("Point { x }"));
    assert!(
        format(&collapsed, FormatOptions::default()).is_err(),
        "the parser accepted shorthand; enable it by default and invert this test"
    );
}

#[test]
fn breaks_a_struct_literal_that_does_not_fit() {
    assert_formats(
        indoc! {"
            fn main(){let p=Configuration{alpha:1,beta:2,gamma:3,delta:4,epsilon:5,zeta:6};p.alpha}
        "},
        indoc! {"
            fn main() {
                let p = Configuration {
                    alpha: 1,
                    beta: 2,
                    gamma: 3,
                    delta: 4,
                    epsilon: 5,
                    zeta: 6,
                };
                p.alpha
            }
        "},
    );
}

#[test]
fn keeps_a_leading_and_a_trailing_comment() {
    assert_formats(
        indoc! {"
            fn main():i32{
                // why we start here
                let x=1;   // the seed
                x
            }
        "},
        indoc! {"
            fn main(): i32 {
                // why we start here
                let x = 1; // the seed
                x
            }
        "},
    );
}

#[test]
fn keeps_a_comment_dangling_before_a_closing_brace() {
    assert_formats(
        indoc! {"
            fn main():i32{
                let x=1;
                x
                // nothing follows
            }
        "},
        indoc! {"
            fn main(): i32 {
                let x = 1;
                x
                // nothing follows
            }
        "},
    );
}

#[test]
fn keeps_one_blank_line_between_statements_and_collapses_more() {
    assert_formats(
        indoc! {"
            fn main():i32{
                let x=1;



                let y=2;
                x+y
            }
        "},
        indoc! {"
            fn main(): i32 {
                let x = 1;

                let y = 2;
                x + y
            }
        "},
    );
}

#[test]
fn restores_the_parentheses_the_ast_does_not_record() {
    assert_formats(
        indoc! {"
            fn main():i32{(1+2)*3}
        "},
        indoc! {"
            fn main(): i32 {
                (1 + 2) * 3
            }
        "},
    );
}

#[test]
fn drops_parentheses_that_precedence_already_gives() {
    assert_formats(
        indoc! {"
            fn main():i32{1+(2*3)}
        "},
        indoc! {"
            fn main(): i32 {
                1 + 2 * 3
            }
        "},
    );
}

#[test]
fn keeps_subtraction_grouped_to_the_left() {
    assert_formats(
        indoc! {"
            fn main():i32{10-(4-1)}
        "},
        indoc! {"
            fn main(): i32 {
                10 - (4 - 1)
            }
        "},
    );
}

#[test]
fn preserves_literal_spelling() {
    assert_formats(
        indoc! {"
            fn main():i32{let big=1_000;let ratio=1.0;let text=\"a\\tb\";big}
        "},
        indoc! {"
            fn main(): i32 {
                let big = 1_000;
                let ratio = 1.0;
                let text = \"a\\tb\";
                big
            }
        "},
    );
}

#[test]
fn prints_a_struct_declaration_one_field_per_line() {
    assert_formats(
        indoc! {"
            struct Point{x:i32,y:i32}
        "},
        indoc! {"
            struct Point {
                x: i32,
                y: i32,
            }
        "},
    );
}

#[test]
fn keeps_doc_comments_on_items_and_fields() {
    assert_formats(
        indoc! {"
            /// A point in the plane
            struct Point{
                /// horizontal offset
                x:i32,
                y:i32
            }
        "},
        indoc! {"
            /// A point in the plane
            struct Point {
                /// horizontal offset
                x: i32,
                y: i32,
            }
        "},
    );
}

#[test]
fn a_blank_line_never_carries_trailing_whitespace() {
    let formatted = format("fn a():i32{1}\nfn b():i32{2}\n", FormatOptions::default()).unwrap();

    for line in formatted.lines() {
        assert_eq!(line.trim_end(), line, "line has trailing whitespace: {line:?}");
    }
}

#[test]
fn formatting_is_idempotent() {
    assert_idempotent(indoc! {"
        /// A point
        struct Point{x:i32,y:i32}

        fn main():i32{
            // set up
            let p=Point{x:1,y:2};   // the origin

            if p.x>0{(p.x+p.y)*2}else{0}
        }
    "});
}

#[test]
fn formats_the_repository_corpus_without_damage() {
    let mut formatted = 0;
    let mut unsupported = 0;

    for directory in ["../std", "../tests/single"] {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "nyx") {
                continue;
            }

            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };

            match format(&source, FormatOptions::default()) {
                Ok(once) => {
                    formatted += 1;

                    let twice = format(&once, FormatOptions::default()).unwrap_or_else(|error| {
                        panic!("{} no longer parses after formatting: {error:?}", path.display())
                    });

                    assert_eq!(once, twice, "{} is not idempotent", path.display());
                },
                Err(_) => unsupported += 1,
            }
        }
    }

    println!("formatted {formatted} files, {unsupported} use unsupported syntax");
    assert!(formatted > 0, "the corpus exercised nothing");
}
