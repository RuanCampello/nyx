#[derive(Debug, Clone)]
pub(in crate::hir) struct Mangler<'m> {
    module: &'m str,
}

impl<'m> Mangler<'m> {
    pub const DEFAULT_MODULE: &'m str = "nyx";

    pub fn new(module: &'m str) -> Self {
        Self { module }
    }

    pub fn default() -> Self {
        Self::new(Self::DEFAULT_MODULE)
    }

    pub fn item(&self, name: &str) -> String {
        format!("{}::{name}", self.module)
    }

    pub fn scoped_item(&self, scope: &str, name: &str) -> String {
        format!("{}::{scope}::{name}", self.module)
    }

    pub fn interface_item(&self, scope: &str, interface: &str, name: &str) -> String {
        format!("{}::{scope}::{interface}::{name}", self.module)
    }
}

pub(crate) fn assembly_label(name: &str) -> String {
    name.replace("::", ".")
}
