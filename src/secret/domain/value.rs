//! Plaintext secret value. See module docs in `mod.rs`.

use zeroize::Zeroizing;

/// A plaintext secret value.
///
/// - No `Serialize`/`Deserialize`: a value cannot reach JSON, YAML, TOML, the
///   listing cache, or a web body by accident. Disclosure boundaries convert
///   explicitly (`DisclosedSecret` in PR 3, `expose_secret` today).
/// - No `Display`/`Deref`: `format!("{v}")` and implicit `&str` coercion do
///   not compile.
/// - `Debug` prints `SecretValue([REDACTED])`, so any struct that derives
///   `Debug` and contains one stays safe to log.
/// - The buffer is zeroized on drop.
///
/// ```compile_fail
/// let v = crosstache::secret::domain::SecretValue::new("x");
/// let _ = serde_json::to_string(&v);
/// ```
///
/// ```compile_fail
/// let v = crosstache::secret::domain::SecretValue::new("x");
/// let _ = format!("{v}");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Zeroizing<String>);

// Not yet wired into `manager.rs`/`backend/secret.rs` (that lands in Task 2 of
// this split), so the `xv` binary's own module tree has no caller yet and
// clippy's dead_code lint fires on the bin target. Remove this allow once
// Task 2 wires the domain types into real call sites.
#[allow(dead_code)]
impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The only plaintext read. Every caller is a disclosure boundary or an
    /// adapter writing to a provider; keep the call sites greppable.
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::SecretValue;

    const CANARY: &str = "super-secret-value-canary";

    #[test]
    fn debug_is_redacted() {
        let v = SecretValue::new(CANARY);
        let dbg = format!("{v:?}");
        assert_eq!(dbg, "SecretValue([REDACTED])");
        assert!(!dbg.contains(CANARY));
        let opt = Some(SecretValue::new(CANARY));
        assert!(!format!("{opt:?}").contains(CANARY));
    }

    #[test]
    fn expose_returns_the_plaintext_and_only_that() {
        let v = SecretValue::new(CANARY);
        assert_eq!(v.expose_secret(), CANARY);
        assert_eq!(v.len(), CANARY.len());
        assert!(!v.is_empty());
        assert!(SecretValue::new("").is_empty());
    }

    #[test]
    fn equality_compares_plaintext() {
        assert_eq!(SecretValue::new("a"), SecretValue::new("a"));
        assert_ne!(SecretValue::new("a"), SecretValue::new("b"));
        let cloned = SecretValue::new("a").clone();
        assert_eq!(cloned.expose_secret(), "a");
    }
}
