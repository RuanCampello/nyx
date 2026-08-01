use fmt::{FormatOptions, format};

#[test]
fn formats_a_function() {
    let source = include_str!("fixtures/function/input.nyx");
    let expected = include_str!("fixtures/function/output.nyx");

    assert_eq!(format(source, FormatOptions::default()).unwrap(), expected);
}
