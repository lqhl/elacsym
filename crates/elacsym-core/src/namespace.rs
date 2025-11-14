//! Namespace data structures

use serde::{Deserialize, Serialize};

/// A namespace provides hard data isolation
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Namespace(String);

impl Namespace {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate namespace name
    pub fn is_valid(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 256
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }
}

impl Default for Namespace {
    fn default() -> Self {
        Self("default".to_string())
    }
}

impl From<String> for Namespace {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Namespace {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_namespaces() {
        assert!(Namespace::is_valid("default"));
        assert!(Namespace::is_valid("my-namespace"));
        assert!(Namespace::is_valid("namespace_123"));
        assert!(Namespace::is_valid("test123"));
    }

    #[test]
    fn test_invalid_namespaces() {
        assert!(!Namespace::is_valid(""));
        assert!(!Namespace::is_valid("my namespace"));
        assert!(!Namespace::is_valid("namespace!"));
        assert!(!Namespace::is_valid("name@space"));
    }
}
