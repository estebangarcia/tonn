//! Plugin system for Nexterm.
//! Phase 0 stub - full implementation in Phase 6.

/// Metadata about a plugin.
pub struct PluginMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
}

/// The plugin trait that all plugins implement.
pub trait Plugin: Send + Sync {
    fn metadata(&self) -> PluginMetadata;
}
