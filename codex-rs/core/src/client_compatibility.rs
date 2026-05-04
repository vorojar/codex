//! Internal compatibility flags selected by app-server clients.
//!
//! These flags keep app-server client identity at the app-server boundary. Core
//! should receive the behavior switches it needs, not the raw client name and
//! version that led to those switches.

pub use codex_mcp::McpElicitationCompatibility;

/// Behavior switches selected by the app-server layer for known client quirks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClientCompatibilityFlags {
    /// MCP elicitation capability policy to apply when starting or refreshing MCP servers.
    pub mcp_elicitation: McpElicitationCompatibility,
}
