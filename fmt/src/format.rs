use crate::print::Printer;
use crate::render::{Indentation, RenderOptions, render};
use frontend::lexer::token::Span;
use frontend::parser::Parser;
use serde::Deserialize;
use std::num::NonZero;

/// Layout options shared by every source-formatting entry point
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FormatOptions {
    layout: LayoutOptions,
    field: FieldOptions,
    style: StyleOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LayoutOptions {
    /// The preferred maximum number of columns in a formatted line
    line_width: usize,
    /// The whitespace unit written at the start of each indented line
    indentation: Indentation,
}

/// Choices between two spellings the grammar accepts for the same construct
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StyleOptions {
    /// Whether a block body holding a single expression is rewritten as `= expr;`
    ///
    /// **default: false**
    /// ```rust,ignore
    /// fn add(x: i8, y: i8): i8 {
    ///     return x + y;
    /// }
    /// ```
    ///
    /// **true**
    /// ```rust,ignore
    /// fn add(x: i8, y: i8): i8 = x + y;
    /// ```
    prefer_expression_body: bool,
    /// Whether an `if` whose block holds a single statement is rewritten braceless
    ///
    /// **default: false**
    /// ```rust,ignore
    /// if feed.device_id() != 41 {
    ///     return 1;
    /// }
    /// ```
    ///
    /// **true**
    /// ```rust,ignore
    /// if feed.device_id() != 41 return 1;
    /// ```
    prefer_single_line_if: bool,
    /// Whether the last arm of a `match` is closed with a comma
    ///
    /// A struct declaration always carries one, this governs match arms, where
    /// both spellings are common.
    ///
    /// **default: true**
    /// ```rust,ignore
    /// match signal {
    ///     Signal::Halt -> 0,
    ///     _ -> 1,
    /// }
    /// ```
    ///
    /// **false**
    /// ```rust,ignore
    /// match signal {
    ///     Signal::Halt -> 0,
    ///     _ -> 1
    /// }
    /// ```
    trailing_comma: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FieldOptions {
    /// Whether to write a field as `x` when its value is the variable `x`
    ///
    /// **false**
    /// ```rust
    /// struct Foo {
    ///     x: u32,
    ///     y: u32,
    ///     z: u32,
    /// }
    ///
    /// fn main() {
    ///     let x = 1;
    ///     let y = 2;
    ///     let z = 3;
    ///     let a = Foo { x, y, z };
    ///     let b = Foo { x: x, y: y, z: z };
    /// }
    ///
    ///```
    /// **default: true**
    /// ```rust
    ///struct Foo {
    ///    x: u32,
    ///    y: u32,
    ///    z: u32,
    ///}
    ///
    ///fn main() {
    ///    let x = 1;
    ///    let y = 2;
    ///    let z = 3;
    ///    let a = Foo { x, y, z };
    ///    let b = Foo { x, y, z };
    ///}
    ///```
    initialise_short_hand: bool,
    /// The maximum diff between struct fields to be aligned
    /// with each other
    ///
    /// **default: 0**
    /// ```rust
    /// struct Foo {
    ///     x: u32,
    ///     yy: u32,
    ///     zzz: u32
    /// }
    /// ```
    ///
    /// **20**:
    /// ```rust
    /// struct Foo {
    ///     x:   u32,
    ///     yy: u32,
    ///     zzz: u32,
    /// }
    /// ```
    struct_align: Option<NonZero<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// The source does not parse, so there is nothing to lay out
    Parse { span: Span },
    /// A grammar production the printer does not handle yet
    Unsupported { span: Span },
    /// The printer did not account for every comment in the source
    /// Formatting is refused rather than silently discarding a comment
    CommentDropped { printed: usize, scanned: usize },
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self { indentation: Indentation::default(), line_width: 80 }
    }
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            layout: LayoutOptions::default(),
            field: FieldOptions::default(),
            style: StyleOptions::default(),
        }
    }
}

impl Default for StyleOptions {
    fn default() -> Self {
        Self {
            prefer_expression_body: false,
            prefer_single_line_if: false,
            trailing_comma: true,
        }
    }
}

impl Default for FieldOptions {
    fn default() -> Self {
        Self { initialise_short_hand: true, struct_align: None }
    }
}

impl std::str::FromStr for FormatOptions {
    type Err = toml::de::Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        toml::from_str(source)
    }
}

impl FormatError {
    /// byte offset the failure is anchored to, for callers that report a location
    #[inline]
    pub const fn offset(&self) -> Option<usize> {
        match self {
            Self::Parse { span } | Self::Unsupported { span } => Some(span.start.offset()),
            Self::CommentDropped { .. } => None,
        }
    }
}

impl FormatOptions {
    #[inline]
    #[must_use]
    pub const fn with_indentation(mut self, indentation: Indentation) -> Self {
        self.layout.indentation = indentation;
        self
    }

    #[inline]
    pub const fn line_width(&self) -> usize {
        self.layout.line_width
    }

    #[inline]
    pub const fn indentation(&self) -> Indentation {
        self.layout.indentation
    }

    #[inline]
    pub const fn indent_width(&self) -> u8 {
        self.layout.indentation.width()
    }

    #[inline]
    pub const fn initialise_short_hand(&self) -> bool {
        self.field.initialise_short_hand
    }

    #[inline]
    pub const fn struct_align(&self) -> Option<NonZero<u8>> {
        self.field.struct_align
    }

    #[inline]
    pub const fn prefer_expression_body(&self) -> bool {
        self.style.prefer_expression_body
    }

    #[inline]
    pub const fn prefer_single_line_if(&self) -> bool {
        self.style.prefer_single_line_if
    }

    #[inline]
    pub const fn trailing_comma(&self) -> bool {
        self.style.trailing_comma
    }
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse { .. } => f.write_str("does not parse"),
            Self::Unsupported { .. } => f.write_str("syntax the formatter does not handle yet"),
            Self::CommentDropped { printed, scanned } => write!(
                f,
                "refused to place {} of {scanned} comments, so the file was left alone",
                scanned - printed
            ),
        }
    }
}

pub fn format(source: &str, options: FormatOptions) -> Result<String, FormatError> {
    use frontend::parser::ParseOutput;

    let ParseOutput { diagnostics, statements } = Parser::new(source).parse();
    if let Some(error) = diagnostics.iter().find(|error| !error.is_missing_semicolon()) {
        return Err(FormatError::Parse { span: error.span() });
    }

    let document = Printer::new(source, options).into_document(&statements)?;

    Ok(render(
        &document,
        RenderOptions {
            print_width: options.line_width(),
            indentation: options.indentation(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn parse_full_config() {
        let config = r#"
            [layout]
            line_width = 120

            [layout.indentation]
            style = "spaces"
            width = 2
        "#;
        let expected = FormatOptions {
            layout: LayoutOptions {
                line_width: 120,
                indentation: Indentation::Spaces { width: 2 },
            },
            ..Default::default()
        };

        assert_eq!(FormatOptions::from_str(config).unwrap(), expected)
    }

    #[test]
    fn parse_only_override_passed_options() {
        let config = r#"
            [layout.indentation]
            style = "spaces"
            width = 6
        "#;
        let expected = FormatOptions {
            layout: LayoutOptions {
                indentation: Indentation::Spaces { width: 6 },
                line_width: 80,
            },
            ..Default::default()
        };

        assert_eq!(FormatOptions::from_str(config).unwrap(), expected)
    }
}
