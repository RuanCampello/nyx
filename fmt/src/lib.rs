//! Nyx source formatter.
//!
//! Its document algebra and layout strategy are inspired by Philip Wadler's
//! [*A Prettier Printer*](https://homepages.inf.ed.ac.uk/wadler/papers/prettier/prettier.pdf).

pub mod doc;
mod format;
mod render;

pub use doc::{Doc, Line};
pub use format::{FormatError, FormatOptions, Indentation, format};
pub use render::{RenderOptions, render};
