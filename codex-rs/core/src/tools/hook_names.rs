//! Hook-facing tool names and matcher compatibility aliases.
//!
//! Hook stdin exposes one canonical `tool_name`, but matcher selection may also
//! need to recognize names from adjacent tool ecosystems. Keeping those two
//! concepts together prevents handlers from accidentally serializing a
//! compatibility alias, such as `Write`, as the stable hook payload name.

use codex_tools::ToolName;

/// Identifies a tool in hook payloads and hook matcher selection.
///
/// `name` is the canonical value serialized into hook stdin. Matcher aliases are
/// internal-only compatibility names that may select the same hook handlers but
/// must not change the payload seen by hook processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HookToolName {
    name: String,
    matcher_aliases: Vec<String>,
}

impl HookToolName {
    /// Builds a hook tool name with no matcher aliases.
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            matcher_aliases: Vec::new(),
        }
    }

    /// Builds the canonical hook-facing identity for dynamic tools.
    ///
    /// Plain dynamic tools keep their plain tool name. Namespaced dynamic tools
    /// flatten to `namespace__tool`, mirroring the namespaced form used across
    /// the model-facing tool surface.
    ///
    /// Each segment is escaped independently so the structural `__` separator
    /// stays injective even when identifiers contain edge underscores or runs
    /// of multiple underscores. Other bytes remain percent-encoded
    /// defensively, though new dynamic tool registration already narrows the
    /// upstream contract to Responses-compatible ASCII identifiers.
    pub(crate) fn for_dynamic_tool(tool_name: &ToolName) -> Self {
        match tool_name.namespace.as_deref() {
            Some(namespace) => Self::new(format!(
                "{}__{}",
                encode_dynamic_segment(namespace),
                encode_dynamic_segment(&tool_name.name),
            )),
            None => Self::new(encode_dynamic_segment(&tool_name.name)),
        }
    }

    /// Builds the canonical hook-facing identity for MCP tools.
    ///
    /// MCP tool names already use the stable fully qualified
    /// `mcp__server__tool` form, so we preserve them verbatim.
    pub(crate) fn for_mcp_tool(tool_name: &ToolName) -> Self {
        Self::new(tool_name.display())
    }

    /// Returns the hook identity for file edits performed through `apply_patch`.
    ///
    /// The serialized name remains `apply_patch` so logs and policies can key
    /// off the actual Codex tool. `Write` and `Edit` are accepted as matcher
    /// aliases for compatibility with hook configurations that describe edits
    /// using Claude Code-style names.
    pub(crate) fn apply_patch() -> Self {
        Self {
            name: "apply_patch".to_string(),
            matcher_aliases: vec!["Write".to_string(), "Edit".to_string()],
        }
    }

    /// Returns the hook identity historically used for shell-like tools.
    pub(crate) fn bash() -> Self {
        Self::new("Bash")
    }

    /// Returns the canonical hook name serialized into hook stdin.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns additional matcher inputs that should select the same handlers.
    pub(crate) fn matcher_aliases(&self) -> &[String] {
        &self.matcher_aliases
    }
}

fn encode_dynamic_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut encoded = String::with_capacity(segment.len());

    for (index, byte) in bytes.iter().copied().enumerate() {
        if should_preserve_dynamic_byte(bytes, index, byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
        }
    }

    encoded
}

fn should_preserve_dynamic_byte(bytes: &[u8], index: usize, byte: u8) -> bool {
    match byte {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' => true,
        b'_' => {
            index > 0
                && index + 1 < bytes.len()
                && (bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'-')
                && (bytes[index + 1].is_ascii_alphanumeric() || bytes[index + 1] == b'-')
                && bytes[index - 1] != b'_'
                && bytes[index + 1] != b'_'
        }
        _ => false,
    }
}

const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

#[cfg(test)]
mod tests {
    use super::HookToolName;
    use codex_tools::ToolName;
    use pretty_assertions::assert_eq;

    #[test]
    fn for_dynamic_tool_keeps_plain_tool_names_plain() {
        assert_eq!(
            HookToolName::for_dynamic_tool(&ToolName::plain("tool_search")),
            HookToolName::new("tool_search"),
        );
    }

    #[test]
    fn for_mcp_tool_keeps_mcp_names_stable() {
        assert_eq!(
            HookToolName::for_mcp_tool(&ToolName::namespaced("mcp__memory__", "create_entities",)),
            HookToolName::new("mcp__memory__create_entities"),
        );
    }

    #[test]
    fn for_dynamic_tool_uses_namespace_separator_for_namespaced_tools() {
        assert_eq!(
            HookToolName::for_dynamic_tool(
                &ToolName::namespaced("codex_app", "automation_update",)
            ),
            HookToolName::new("codex_app__automation_update"),
        );
    }

    #[test]
    fn for_dynamic_tool_does_not_spoof_plain_namespaced_shapes() {
        assert_eq!(
            HookToolName::for_dynamic_tool(
                &ToolName::namespaced("mcp__filesystem__", "read_file",)
            ),
            HookToolName::new("mcp%5F%5Ffilesystem%5F%5F__read_file"),
        );
    }

    #[test]
    fn for_dynamic_tool_escapes_ambiguous_delimiters() {
        let first = HookToolName::for_dynamic_tool(&ToolName::namespaced("foo__bar", "baz"));
        let second = HookToolName::for_dynamic_tool(&ToolName::namespaced("foo", "bar__baz"));

        assert_eq!(first, HookToolName::new("foo%5F%5Fbar__baz"));
        assert_eq!(second, HookToolName::new("foo__bar%5F%5Fbaz"));
        assert_ne!(first, second);
    }

    #[test]
    fn for_dynamic_tool_escapes_edge_underscores_and_preserves_hyphens() {
        assert_eq!(
            HookToolName::for_dynamic_tool(&ToolName::namespaced("_google-drive", "update.file_",)),
            HookToolName::new("%5Fgoogle-drive__update%2Efile%5F"),
        );
    }

    #[test]
    fn for_dynamic_tool_keeps_single_internal_underscores() {
        assert_eq!(
            HookToolName::for_dynamic_tool(&ToolName::namespaced("codex_app", "automation_update")),
            HookToolName::new("codex_app__automation_update"),
        );
    }

    #[test]
    fn for_dynamic_tool_percent_encodes_unsupported_bytes_defensively() {
        assert_eq!(
            HookToolName::for_dynamic_tool(&ToolName::namespaced("検", "索")),
            HookToolName::new("%E6%A4%9C__%E7%B4%A2"),
        );
    }
}
