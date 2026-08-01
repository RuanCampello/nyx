/// Layout options shared by every source-formatting entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions {
    pub print_width: usize,
    pub indent_width: u8,
    pub use_tabs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {}

impl Default for FormatOptions {
    fn default() -> Self {
        Self { print_width: 80, indent_width: 4, use_tabs: false }
    }
}

pub fn format(_source: &str, _options: FormatOptions) -> Result<String, FormatError> {
    unimplemented!()
}
