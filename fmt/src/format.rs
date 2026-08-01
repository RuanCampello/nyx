use serde::Deserialize;

/// Layout options shared by every source-formatting entry point
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FormatOptions {
    layout: LayoutOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LayoutOptions {
    /// The preferred maximum number of columns in a formatted line
    line_width: usize,
    /// The whitespace unit written at the start of each indented line
    indentation: Indentation,
}

/// Whitespace used for one indentation level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "style", rename_all = "snake_case", deny_unknown_fields)]
pub enum Indentation {
    /// A tab character for every indentation level
    Tabs,
    /// A fixed number of spaces for every indentation level
    Spaces {
        /// Number of spaces written for one indentation level
        width: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {}

impl Default for Indentation {
    fn default() -> Self {
        Self::Tabs
    }
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self { indentation: Indentation::default(), line_width: 80 }
    }
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self { layout: LayoutOptions::default() }
    }
}

impl std::str::FromStr for FormatOptions {
    type Err = toml::de::Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        toml::from_str(source)
    }
}

pub fn format(_source: &str, _options: FormatOptions) -> Result<String, FormatError> {
    unimplemented!()
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
        };

        assert_eq!(FormatOptions::from_str(config).unwrap(), expected)
    }
}
