//! The central registry of lints
//!
//! A lint is a diagnostic that never has to stop a build. Unlike an
//! [ErrorCode](crate::error_codes::ErrorCode) it is *named* rather than
//! numbered, and it carries a default [Level], so a future `@allow(name)` marker
//! can turn one off at a declaration the way rustc's `#[allow]` does.
use crate::diagnostic::Severity;

/// How loudly a lint is reported when nothing overrides it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    Allow,
    Warn,
    Deny,
}

macro_rules! lints {
    ($($variant:ident => $name:literal, $level:expr, $description:literal;)*) => {
        /// A stable identifier for one kind of compiler judgement
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Lint {
            $($variant,)*
        }

        impl Lint {
            pub const ALL: &'static [Lint] = &[$(Lint::$variant,)*];

            #[inline]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            #[inline]
            pub const fn default_level(self) -> Level {
                match self {
                    $(Self::$variant => $level,)*
                }
            }

            #[inline]
            pub const fn description(self) -> &'static str {
                match self {
                    $(Self::$variant => $description,)*
                }
            }

            pub fn parse(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|lint| lint.name() == name)
            }
        }

        impl std::fmt::Display for Lint {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

lints! {
    UnusedUnsafe => "unused_unsafe", Level::Warn,
        "an `@unsafe` block whose contents need no unsafe context";
}

impl Level {
    #[inline]
    pub const fn severity(self) -> Severity {
        match self {
            Self::Deny => Severity::Error,
            Self::Allow | Self::Warn => Severity::Warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lint_round_trips_through_its_name() {
        for &lint in Lint::ALL {
            assert_eq!(Lint::parse(lint.name()), Some(lint));
            assert!(!lint.description().is_empty());
        }
    }
}
