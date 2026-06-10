//! The Nyx language server
//!
//! The binary in `main.rs` serves this over stdio
//! integration tests boot the same server over an in-memory transport

mod convert;
mod document;
mod feature;
mod server;

pub use server::Lsp;
