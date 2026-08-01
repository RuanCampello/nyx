//! In-memory overlay of open editor buffers over the on-disk project
//!
//! Analysis always runs from a project entry (`main.nyx`) so that imports
//! resolve, unsaved buffers are layered on top as overlays

use std::collections::HashMap;
use std::path::PathBuf;
use tower_lsp::lsp_types::Url;

#[derive(Debug, Default)]
pub struct Documents {
    overlays: HashMap<PathBuf, String>,
    root: Option<PathBuf>,
}

impl Documents {
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
    }

    pub fn open(&mut self, uri: &Url, text: String) {
        if let Some(path) = canonical(uri) {
            self.overlays.insert(path, text);
        }
    }

    pub fn close(&mut self, uri: &Url) {
        if let Some(path) = canonical(uri) {
            self.overlays.remove(&path);
        }
    }

    pub fn snapshot(&self) -> HashMap<PathBuf, String> {
        self.overlays.clone()
    }

    /// the current buffer text for `uri`, if it is open
    #[inline]
    pub fn text(&self, uri: &Url) -> Option<String> {
        self.overlays.get(&canonical(uri)?).cloned()
    }

    /// the project entry to analyse `uri` from: the nearest `main.nyx` walking
    /// up to the workspace root, or the file itself when there is none
    pub fn entry(&self, uri: &Url) -> Option<PathBuf> {
        let file = canonical(uri)?;
        let stop_at = self.root.as_deref();

        let mut dir = file.parent();
        while let Some(current) = dir {
            let candidate = current.join("main.nyx");
            if candidate.exists() {
                return Some(candidate);
            }
            if stop_at.is_some_and(|root| current == root) {
                break;
            }
            dir = current.parent();
        }

        Some(file)
    }
}

/// Canonical filesystem path for a `file://` URL, falling back to the
/// un-canonicalised path for buffers that are not yet on disk
pub fn canonical(uri: &Url) -> Option<PathBuf> {
    let path = uri.to_file_path().ok()?;
    Some(path.canonicalize().unwrap_or(path))
}
