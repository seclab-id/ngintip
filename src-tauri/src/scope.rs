//! Target scope: host patterns like `example.com` or `*.example.com`.
//! An empty scope means everything is in scope.

use std::sync::RwLock;

#[derive(Default)]
pub struct Scope {
    patterns: RwLock<Vec<String>>,
}

impl Scope {
    pub fn set(&self, patterns: Vec<String>) {
        *self.patterns.write().unwrap() = normalize(patterns);
    }

    pub fn get(&self) -> Vec<String> {
        self.patterns.read().unwrap().clone()
    }

    pub fn contains(&self, host: &str) -> bool {
        let patterns = self.patterns.read().unwrap();
        patterns.is_empty() || patterns.iter().any(|p| host_matches(p, host))
    }
}

/// Trim, lowercase, drop blanks and duplicates.
pub fn normalize(patterns: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in patterns {
        let p = p.trim().to_ascii_lowercase();
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// `*.example.com` matches example.com and any subdomain; anything else is exact.
/// A port on `host` is ignored.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h).to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(base) => host == base || host.ends_with(&format!(".{base}")),
        None => host == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("example.com", "example.com:8443"));
        assert!(!host_matches("example.com", "api.example.com"));
        assert!(host_matches("*.example.com", "api.example.com"));
        assert!(host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "badexample.com"));
    }

    #[test]
    fn empty_scope_allows_all() {
        let scope = Scope::default();
        assert!(scope.contains("anything.test"));
        scope.set(vec![" *.Example.com ".into(), "".into(), "*.example.com".into()]);
        assert_eq!(scope.get(), vec!["*.example.com"]);
        assert!(scope.contains("a.example.com"));
        assert!(!scope.contains("other.test"));
    }
}
