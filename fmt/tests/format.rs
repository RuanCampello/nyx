use fmt::{FormatOptions, Indentation, format};
use indoc::indoc;
use std::str::FromStr;

const LONG_BODY: &str =
    "fn describe(alpha: i32, beta: i32): i32 = alpha * beta + alpha - beta + alpha * 2;\n";

fn prefer_expression_body() -> FormatOptions {
    FormatOptions::from_str("[style]\nprefer_expression_body = true\n")
        .unwrap()
        .with_indentation(Indentation::Spaces { width: 4 })
}

fn shorthand_disabled() -> FormatOptions {
    FormatOptions::from_str("[field]\ninitialise_short_hand = false\n")
        .unwrap()
        .with_indentation(Indentation::Spaces { width: 4 })
}

fn spaces() -> FormatOptions {
    FormatOptions::default().with_indentation(Indentation::Spaces { width: 4 })
}

fn assert_formats(source: &str, expected: &str) {
    assert_eq!(format(source, spaces()).unwrap(), expected);
}

fn assert_formats_with(options: FormatOptions, source: &str, expected: &str) {
    assert_eq!(format(source, options).unwrap(), expected);
}

fn assert_idempotent(source: &str) {
    let once = format(source, spaces()).unwrap();
    let twice = format(&once, spaces()).unwrap();

    assert_eq!(once, twice, "formatting is not idempotent");
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
fn collapses_a_matching_field_to_shorthand_by_default() {
    assert_formats(
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
fn keeps_explicit_fields_when_disabled() {
    assert_formats_with(
        shorthand_disabled(),
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
fn shorthand_needs_the_field_and_variable_to_share_a_name() {
    assert_formats(
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
    assert_formats(
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
fn shorthand_output_round_trips() {
    let source = "fn main(){let x=1;let p=Point{x:x};p.x}\n";
    let collapsed = format(source, spaces()).unwrap();
    assert!(collapsed.contains("Point { x }"));

    let reprinted =
        format(&collapsed, spaces()).expect("shorthand the printer emits must parse again");
    assert!(reprinted.contains("Point { x }"), "and stays shorthand: {reprinted}");
    assert_eq!(reprinted, collapsed, "formatting is idempotent over the shorthand");
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
fn formats_both_import_forms_and_keeps_them_in_one_block() {
    assert_formats(
        indoc! {"
            use   std::io;
            use std::cmp::{PartialEq,Ordering};

            fn main(){}
        "},
        indoc! {"
            use std::io;
            use std::cmp::{PartialEq, Ordering};

            fn main() {
            }
        "},
    );
}

#[test]
fn formats_every_loop_header() {
    assert_formats(
        indoc! {"
            fn main(){
                loop{break;}
                loop 0..5{continue;}
                loop i in 0..=10{break;}
                loop value in items{break;}
            }
        "},
        indoc! {"
            fn main() {
                loop {
                    break;
                }
                loop 0..5 {
                    continue;
                }
                loop i in 0..=10 {
                    break;
                }
                loop value in items {
                    break;
                }
            }
        "},
    );
}

#[test]
fn formats_an_impl_block_with_an_interface_and_a_receiver() {
    assert_formats(
        indoc! {"
            impl Counter with Clone{
                const LIMIT:i32=10;
                pub fn tick(&mut self,by:i32):i32{self.value+by}
            }
        "},
        indoc! {"
            impl Counter with Clone {
                const LIMIT: i32 = 10;
                pub fn tick(&mut self, by: i32): i32 {
                    self.value + by
                }
            }
        "},
    );
}

#[test]
fn keeps_impl_members_in_the_order_they_were_written() {
    let source = indoc! {"
        impl Counter {
            pub fn first(&self): i32 {
                1
            }
            const MIDDLE: i32 = 2;
            pub fn last(&self): i32 {
                3
            }
        }
    "};

    let formatted = format(source, FormatOptions::default()).unwrap();
    let order: Vec<_> = ["first", "MIDDLE", "last"]
        .iter()
        .map(|name| formatted.find(name).expect("member is printed"))
        .collect();

    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "members were reordered: {formatted}"
    );
}

#[test]
fn formats_an_interface_with_requirements_and_a_default() {
    assert_formats(
        indoc! {"
            pub interface Ord {
                const LIMIT: i32;
                fn cmp( &self ,other:&Self ):Ordering;
                fn max(&self,other:&Self):Self{
                    self
                }
            }
        "},
        indoc! {"
            pub interface Ord {
                const LIMIT: i32;
                fn cmp(&self, other: &Self): Ordering;
                fn max(&self, other: &Self): Self {
                    self
                }
            }
        "},
    );
}

/// an associated type is a member like any other: dropping it silently deletes the
/// binding an implementation is required to supply
#[test]
fn keeps_associated_types() {
    assert_formats(
        indoc! {"
            pub interface Index<Idx> {
                type   Output ;
                fn index(&self,index:Idx):&Self::Output;
            }
        "},
        indoc! {"
            pub interface Index<Idx> {
                type Output;
                fn index(&self, index: Idx): &Self::Output;
            }
        "},
    );

    assert_formats(
        indoc! {"
            impl Bag with Index<uptr> {
                type Output=i32;
                fn index(&self,index:uptr):&Self::Output{&self.items[index]}
            }
        "},
        indoc! {"
            impl Bag with Index<uptr> {
                type Output = i32;
                fn index(&self, index: uptr): &Self::Output {
                    &self.items[index]
                }
            }
        "},
    );
}

#[test]
fn keeps_inline_on_a_requirement() {
    assert_formats(
        indoc! {"
            pub interface Hasher {
                fn write(&mut self,byte:u8);
                inline fn twice(&mut self,byte:u8){self.write(byte);}
            }
        "},
        indoc! {"
            pub interface Hasher {
                fn write(&mut self, byte: u8);
                inline fn twice(&mut self, byte: u8) {
                    self.write(byte);
                }
            }
        "},
    );
}

#[test]
fn keeps_const_on_a_requirement() {
    assert_formats(
        indoc! {"
            pub interface Bounded {
                const LIMIT: i32;
                const fn peak(&self):i32;
            }
        "},
        indoc! {"
            pub interface Bounded {
                const LIMIT: i32;
                const fn peak(&self): i32;
            }
        "},
    );
}

#[test]
fn keeps_associated_type_bounds() {
    assert_formats(
        indoc! {"
            pub interface Show {
                type Output : Display+Clone ;
            }
        "},
        indoc! {"
            pub interface Show {
                type Output: Display + Clone;
            }
        "},
    );
}

#[test]
fn formats_superinterfaces_without_duplicating_them() {
    assert_formats(
        indoc! {"
            pub interface Copy: Clone {}
        "},
        indoc! {"
            pub interface Copy: Clone {
            }
        "},
    );

    assert_formats(
        indoc! {"
            pub interface Ord: PartialOrd + Eq {}
        "},
        indoc! {"
            pub interface Ord: PartialOrd + Eq {
            }
        "},
    );
}

#[test]
fn a_generic_interface_keeps_its_parameters_and_member_docs() {
    assert_formats(
        indoc! {"
            pub interface PartialEq<Rhs> {
                /// whether the two compare equal
                @unsafe
                fn eq(&self, other: &Rhs): bool;
            }
        "},
        indoc! {"
            pub interface PartialEq<Rhs> {
                /// whether the two compare equal
                @unsafe
                fn eq(&self, other: &Rhs): bool;
            }
        "},
    );
}

#[test]
fn preserves_an_expression_bodied_function() {
    assert_formats(
        indoc! {"
            fn double(x:i32):i32=x*2;
        "},
        indoc! {"
            fn double(x: i32): i32 = x * 2;
        "},
    );
}

#[test]
fn preserves_generics_markers_and_a_where_clause() {
    assert_formats(
        indoc! {"
            @intrinsic
            pub inline const fn each<T: Clone>(items:T)  where  T: Default {
            }
        "},
        indoc! {"
            @intrinsic
            pub inline const fn each<T: Clone>(items: T) where T: Default {
            }
        "},
    );
}

#[test]
fn a_where_clause_is_never_moved_into_the_angle_brackets() {
    let source =
        "impl Result<S, F> {\n    pub fn get(self): S where S: Default {\n        1\n    }\n}\n";
    let formatted = format(source, FormatOptions::default()).unwrap();

    assert!(formatted.contains("where S: Default"), "{formatted}");
    assert!(
        !formatted.contains("get<"),
        "the bound was redeclared on the method: {formatted}"
    );
}

#[test]
fn preserves_the_spelling_of_an_array_repeat_count() {
    assert_formats(
        indoc! {"
            fn main(){let grid=[0;1_000];let nested=[[1;2];3];}
        "},
        indoc! {"
            fn main() {
                let grid = [0; 1_000];
                let nested = [[1; 2]; 3];
            }
        "},
    );
}

#[test]
fn parenthesises_a_dereference_a_cast_would_otherwise_absorb() {
    assert_formats(
        indoc! {"
            fn main(){let n=(*self) as u32;}
        "},
        indoc! {"
            fn main() {
                let n = (*self) as u32;
            }
        "},
    );
}

#[test]
fn keeps_the_semicolon_after_an_expression_else() {
    assert_formats(
        indoc! {"
            fn main(){if a{b=1;}else b=2;}
        "},
        indoc! {"
            fn main() {
                if a {
                    b = 1;
                } else b = 2;
            }
        "},
    );
}

#[test]
fn a_comment_above_a_modified_item_is_kept() {
    assert_formats(
        indoc! {"
            // why this is public
            pub fn visible(){}
        "},
        indoc! {"
            // why this is public
            pub fn visible() {
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

#[test]
fn formats_static_declarations() {
    assert_formats(
        indoc! {"
            static   LIMIT:i32=10;
            pub static mut CURSOR:uptr=0;
            pub   static SEEDED:u64=7;
        "},
        indoc! {"
            static LIMIT: i32 = 10;

            pub static mut CURSOR: uptr = 0;

            pub static SEEDED: u64 = 7;
        "},
    );
}

#[test]
fn indents_with_four_spaces_by_default() {
    assert_eq!(
        format("fn main():i32{let answer=40+2;answer}\n", FormatOptions::default()).unwrap(),
        "fn main(): i32 {\n    let answer = 40 + 2;\n    answer\n}\n"
    );
}

#[test]
fn tabs_are_written_when_the_configuration_asks_for_them() {
    let tabs = FormatOptions::from_str("[layout.indentation]\nstyle = \"tabs\"\n").unwrap();

    assert_eq!(
        format("fn main():i32{let answer=40+2;answer}\n", tabs).unwrap(),
        "fn main(): i32 {\n\tlet answer = 40 + 2;\n\tanswer\n}\n"
    );
}

#[test]
fn a_tab_is_charged_at_its_display_width() {
    let source =
        "fn main(){call(aaaaaaaaaa,bbbbbbbbbb,cccccccccc,dddddddddd,eeeeeeeeee,fffffffffff);}\n";

    let narrow =
        FormatOptions::from_str("[layout.indentation]\nstyle = \"tabs\"\nwidth = 1\n").unwrap();
    let wide =
        FormatOptions::from_str("[layout.indentation]\nstyle = \"tabs\"\nwidth = 4\n").unwrap();

    assert_eq!(format(source, narrow).unwrap().lines().count(), 3, "the call fits on one line");
    assert!(format(source, wide).unwrap().lines().count() > 3, "the call must break");
}

#[test]
fn keeps_an_expression_body_written_as_one() {
    assert_formats("fn add(x:i8,y:i8):i8=x+y;\n", "fn add(x: i8, y: i8): i8 = x + y;\n");
}

#[test]
fn expands_an_expression_body_that_exceeds_the_line_width() {
    let formatted = format(LONG_BODY, spaces()).unwrap();

    assert!(
        formatted.starts_with("fn describe(alpha: i32, beta: i32): i32 {\n"),
        "got {formatted}"
    );
    assert!(formatted.contains("    return alpha * beta"), "got {formatted}");
}

#[test]
fn keeps_a_block_body_as_a_block_by_default() {
    assert_formats(
        "fn add(x:i8,y:i8):i8{return x+y;}\n",
        indoc! {"
            fn add(x: i8, y: i8): i8 {
                return x + y;
            }
        "},
    );
}

#[test]
fn collapses_a_returning_body_when_preferred() {
    assert_formats_with(
        prefer_expression_body(),
        "fn add(x:i8,y:i8):i8{return x+y;}\n",
        "fn add(x: i8, y: i8): i8 = x + y;\n",
    );
}

#[test]
fn collapses_a_tail_expression_body_when_preferred() {
    assert_formats_with(
        prefer_expression_body(),
        "fn add(x:i8,y:i8):i8{x+y}\n",
        "fn add(x: i8, y: i8): i8 = x + y;\n",
    );
}

#[test]
fn never_collapses_a_body_without_a_return_type() {
    assert_formats_with(
        prefer_expression_body(),
        "fn shout(){print(1);}\n",
        indoc! {"
            fn shout() {
                print(1);
            }
        "},
    );
}

#[test]
fn never_collapses_a_body_of_two_statements() {
    assert_formats_with(
        prefer_expression_body(),
        "fn add(x:i8,y:i8):i8{let s=x+y;return s;}\n",
        indoc! {"
            fn add(x: i8, y: i8): i8 {
                let s = x + y;
                return s;
            }
        "},
    );
}

#[test]
fn never_collapses_a_body_holding_a_comment() {
    assert_formats_with(
        prefer_expression_body(),
        "fn add(x:i8,y:i8):i8{\n// sum\nreturn x+y;\n}\n",
        indoc! {"
            fn add(x: i8, y: i8): i8 {
                // sum
                return x + y;
            }
        "},
    );
}

#[test]
fn never_collapses_a_conditional_body() {
    assert_formats_with(
        prefer_expression_body(),
        "fn classify(v:i32):i32{if v>0{v}else{0}}\n",
        indoc! {"
            fn classify(v: i32): i32 {
                if v > 0 {
                    v
                } else {
                    0
                }
            }
        "},
    );
}

#[test]
fn expression_bodies_are_idempotent() {
    assert_idempotent("fn add(x:i8,y:i8):i8=x+y;\n");
    assert_idempotent(LONG_BODY);
}

fn prefer_single_line_if() -> FormatOptions {
    FormatOptions::from_str("[style]\nprefer_single_line_if = true\n")
        .unwrap()
        .with_indentation(Indentation::Spaces { width: 4 })
}

#[test]
fn keeps_a_braceless_if_written_as_one() {
    assert_formats(
        "fn f(d:i32):i32{if d!=41 return 1;\nreturn 0;}\n",
        indoc! {"
            fn f(d: i32): i32 {
                if d != 41 return 1;
                return 0;
            }
        "},
    );
}

#[test]
fn braces_a_braceless_if_that_exceeds_the_line_width() {
    let source = "fn f(device:i32):i32{if device!=41414141 return 111111 + 222222 + 333333 + 444444 + 555555 + 666666;\nreturn 0;}\n";
    let formatted = format(source, spaces()).unwrap();

    assert!(formatted.contains("    if device != 41414141 {\n"), "got {formatted}");
    assert!(formatted.contains("        return 111111 + 222222"), "got {formatted}");
}

#[test]
fn keeps_a_braced_if_braced_by_default() {
    assert_formats(
        "fn f(d:i32):i32{if d!=41{return 1;}\nreturn 0;}\n",
        indoc! {"
            fn f(d: i32): i32 {
                if d != 41 {
                    return 1;
                }
                return 0;
            }
        "},
    );
}

#[test]
fn collapses_a_single_statement_if_when_preferred() {
    assert_formats_with(
        prefer_single_line_if(),
        "fn f(d:i32):i32{if d!=41{return 1;}\nreturn 0;}\n",
        indoc! {"
            fn f(d: i32): i32 {
                if d != 41 return 1;
                return 0;
            }
        "},
    );
}

#[test]
fn never_collapses_an_if_with_an_else() {
    assert_formats_with(
        prefer_single_line_if(),
        "fn f(d:i32):i32{if d!=41{return 1;}else{return 2;}}\n",
        indoc! {"
            fn f(d: i32): i32 {
                if d != 41 {
                    return 1;
                } else {
                    return 2;
                }
            }
        "},
    );
}

#[test]
fn never_collapses_an_if_of_two_statements() {
    assert_formats_with(
        prefer_single_line_if(),
        "fn f(d:i32):i32{if d!=41{print(d);return 1;}\nreturn 0;}\n",
        indoc! {"
            fn f(d: i32): i32 {
                if d != 41 {
                    print(d);
                    return 1;
                }
                return 0;
            }
        "},
    );
}

#[test]
fn never_collapses_an_if_holding_a_comment() {
    assert_formats_with(
        prefer_single_line_if(),
        "fn f(d:i32):i32{if d!=41{\n// bail\nreturn 1;\n}\nreturn 0;}\n",
        indoc! {"
            fn f(d: i32): i32 {
                if d != 41 {
                    // bail
                    return 1;
                }
                return 0;
            }
        "},
    );
}

#[test]
fn braceless_ifs_are_idempotent() {
    assert_idempotent("fn f(d:i32):i32{if d!=41 return 1;\nreturn 0;}\n");
}

#[test]
fn inserts_a_semicolon_the_source_forgot() {
    assert_formats(
        "fn main():i32{\nlet x = 1\nreturn x;\n}\n",
        indoc! {"
            fn main(): i32 {
                let x = 1;
                return x;
            }
        "},
    );
}

#[test]
fn inserts_a_semicolon_after_an_expression_statement() {
    assert_formats(
        "fn main():i32{\nprint(1)\nreturn 0;\n}\n",
        indoc! {"
            fn main(): i32 {
                print(1);
                return 0;
            }
        "},
    );
}

#[test]
fn inserts_a_semicolon_after_a_return() {
    assert_formats(
        "fn main():i32{\nreturn 0\n}\n",
        indoc! {"
            fn main(): i32 {
                return 0;
            }
        "},
    );
}

#[test]
fn refuses_a_file_whose_errors_are_not_only_semicolons() {
    assert!(format("fn main(: i32 { 2 }\n", spaces()).is_err());
}

#[test]
fn semicolon_insertion_is_idempotent() {
    assert_idempotent("fn main():i32{\nlet x = 1\nreturn x;\n}\n");
}
