use fmt::{FormatOptions, format};
use indoc::indoc;

fn assert_formats(source: &str, expected: &str) {
    assert_eq!(format(source, FormatOptions::default()).unwrap(), expected);
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
