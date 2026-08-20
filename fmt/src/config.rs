//! Locating the `nyxfmt.toml` that governs a source file
//!
//! The nearest configuration wins outright: a file closer to the source does not
//! inherit keys from one further up, so what applies to a directory is answered by
//! a single file rather than by a chain of them.

use crate::FormatOptions;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// resolves options per file, remembering every directory it has already answered
pub struct Discovery {
    /// set when an explicit `--config` disables the walk
    pinned: Option<FormatOptions>,
    resolved: HashMap<PathBuf, FormatOptions>,
}

#[derive(Debug)]
pub enum ConfigError {
    Read { path: PathBuf, error: std::io::Error },
    Parse { path: PathBuf, error: toml::de::Error },
}

/// names read from a directory, in the order they take precedence
const NAMES: [&str; 2] = ["nyxfmt.toml", ".nyxfmt.toml"];

impl Discovery {
    #[inline]
    pub fn new() -> Self {
        Self { pinned: None, resolved: HashMap::new() }
    }

    /// Discovery disabled: every file is laid out with `options`
    #[inline]
    pub fn fixed(options: FormatOptions) -> Self {
        Self { pinned: Some(options), resolved: HashMap::new() }
    }

    pub fn options_for(&mut self, file: &Path) -> Result<FormatOptions, ConfigError> {
        if let Some(options) = self.pinned {
            return Ok(options);
        }

        let start = match file.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };

        if let Some(options) = self.resolved.get(start) {
            return Ok(*options);
        }

        // every directory between the file and the answer shares that answer
        let (mut visited, mut found) = (vec![start.to_path_buf()], None);

        for directory in start.ancestors() {
            if let Some(options) = load_from(directory)? {
                found = Some(options);
                break;
            }

            // the repository root is searched, and then closes the walk
            if directory.join(".git").exists() {
                break;
            }

            if let Some(parent) = directory.parent() {
                visited.push(parent.to_path_buf());
            }
        }

        let options = found.unwrap_or_default();
        for directory in visited {
            self.resolved.insert(directory, options);
        }

        Ok(options)
    }
}

fn load_from(directory: &Path) -> Result<Option<FormatOptions>, ConfigError> {
    for name in NAMES {
        let path = directory.join(name);

        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(ConfigError::Read { path, error }),
        };

        return match source.parse() {
            Ok(options) => Ok(Some(options)),
            Err(error) => Err(ConfigError::Parse { path, error }),
        };
    }

    Ok(None)
}

impl Default for Discovery {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl std::error::Error for ConfigError {}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Parse { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Indentation;
    use std::fs;

    struct Tree(PathBuf);

    const WIDE: &str = "[layout]\nline_width = 100\n";
    const SPACED: &str = "[layout.indentation]\nstyle = \"spaces\"\nwidth = 2\n";

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl Tree {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nyxfmt-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("the scratch tree is created");

            Self(root)
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("a written path has a parent"))
                .expect("the parent directory is created");
            fs::write(&path, contents).expect("the file is written");

            path
        }
    }

    #[test]
    fn the_nearest_config_wins_without_inheriting_the_outer_one() {
        let tree = Tree::new("nearest");
        tree.write("nyxfmt.toml", WIDE);
        tree.write("std/nyxfmt.toml", SPACED);
        let file = tree.write("std/io.nyx", "");

        let options = Discovery::new().options_for(&file).unwrap();

        assert_eq!(options.indentation(), Indentation::Spaces { width: 2 });
        assert_eq!(options.line_width(), 80, "the outer line_width is not inherited");
    }

    #[test]
    fn an_ancestor_config_applies_when_no_nearer_one_exists() {
        let tree = Tree::new("ancestor");
        tree.write("nyxfmt.toml", WIDE);
        let file = tree.write("src/deep/main.nyx", "");

        assert_eq!(Discovery::new().options_for(&file).unwrap().line_width(), 100);
    }

    #[test]
    fn the_walk_stops_at_the_directory_holding_dot_git() {
        let tree = Tree::new("gitroot");
        tree.write("nyxfmt.toml", WIDE);
        fs::create_dir_all(tree.0.join("inner/.git")).unwrap();
        let file = tree.write("inner/src/main.nyx", "");

        assert_eq!(
            Discovery::new().options_for(&file).unwrap().line_width(),
            80,
            "a config above the repository root must not apply"
        );
    }

    #[test]
    fn a_config_in_the_directory_holding_dot_git_still_applies() {
        let tree = Tree::new("gitrootself");
        fs::create_dir_all(tree.0.join("inner/.git")).unwrap();
        tree.write("inner/nyxfmt.toml", WIDE);
        let file = tree.write("inner/src/main.nyx", "");

        assert_eq!(Discovery::new().options_for(&file).unwrap().line_width(), 100);
    }

    #[test]
    fn the_dotted_name_is_found_and_the_plain_one_wins() {
        let tree = Tree::new("dotted");
        tree.write(".nyxfmt.toml", SPACED);
        let dotted_only = tree.write("a/only.nyx", "");

        assert_eq!(
            Discovery::new().options_for(&dotted_only).unwrap().indentation(),
            Indentation::Spaces { width: 2 }
        );

        tree.write("nyxfmt.toml", WIDE);
        let both = tree.write("b/both.nyx", "");
        let options = Discovery::new().options_for(&both).unwrap();

        assert_eq!(options.line_width(), 100);
        assert_eq!(options.indentation(), Indentation::default(), "the dotted file is ignored");
    }

    #[test]
    fn a_malformed_config_errors_with_its_path() {
        let tree = Tree::new("malformed");
        let config = tree.write("nyxfmt.toml", "[layout]\nline_width = \"wide\"\n");
        let file = tree.write("main.nyx", "");

        let error = Discovery::new().options_for(&file).unwrap_err();

        assert!(error.to_string().contains(&config.display().to_string()), "got {error}");
    }

    #[test]
    fn a_fixed_discovery_ignores_the_tree_entirely() {
        let tree = Tree::new("fixed");
        tree.write("nyxfmt.toml", WIDE);
        let file = tree.write("main.nyx", "");

        let pinned = FormatOptions::default();

        assert_eq!(Discovery::fixed(pinned).options_for(&file).unwrap(), pinned);
    }
}
