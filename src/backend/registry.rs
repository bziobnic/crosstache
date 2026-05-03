//! Backend registry — runtime backend resolution.
//!
//! [`BackendRegistry`] holds instantiated backends and dispatches
//! operations to the active one. This is a skeleton for now; actual
//! backend instantiation will be added in a later PR.

use std::collections::HashMap;
use std::sync::Arc;

use super::Backend;

/// Maps backend names to live [`Backend`] instances.
///
/// Created once at startup from the application config. The CLI and TUI
/// layers call [`active()`](Self::active) to get the current backend.
pub struct BackendRegistry {
    backends: HashMap<&'static str, Arc<dyn Backend>>,
    default: &'static str,
}

impl BackendRegistry {
    /// Create a new registry with a single backend.
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let name = backend.name();
        let mut backends = HashMap::new();
        backends.insert(name, backend);
        Self {
            backends,
            default: name,
        }
    }

    /// Get the currently-active backend.
    pub fn active(&self) -> &dyn Backend {
        self.backends[self.default].as_ref()
    }

    /// Get a backend by name.
    pub fn get(&self, name: &str) -> Option<&dyn Backend> {
        self.backends.get(name).map(|b| b.as_ref())
    }

    /// List all registered backend names.
    pub fn names(&self) -> Vec<&'static str> {
        self.backends.keys().copied().collect()
    }

    /// The name of the default (active) backend.
    pub fn default_name(&self) -> &'static str {
        self.default
    }
}
