use super::ModuleError;
use crate::lexer::token::Span;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct ModuleResolver {
    name: String,
    root: PathBuf,
    std: PathBuf,
}

impl ModuleResolver {
    pub(super) fn new(name: String, root: PathBuf, std: PathBuf) -> Self {
        Self { name, root, std }
    }

    pub(super) fn std_root(&self) -> &Path {
        &self.std
    }

    pub(super) fn resolve_path(
        &self,
        segments: &[&str],
        span: Span,
    ) -> Result<PathBuf, ModuleError> {
        let (&root, rest) = segments.split_first().ok_or(ModuleError::EmptyPath)?;

        if rest.is_empty() {
            return Err(ModuleError::EmptyPath);
        }

        let base = match root {
            root if root == self.name => &self.root,
            "std" => &self.std,
            other => {
                return Err(ModuleError::UnknownRoot { name: other.to_string(), span });
            },
        };

        let (dirs, file_segment) = rest.split_at(rest.len() - 1);
        let mut path = base.to_path_buf();

        path.extend(dirs);
        path.push(format!("{}.nyx", file_segment[0]));

        Ok(path)
    }
}
