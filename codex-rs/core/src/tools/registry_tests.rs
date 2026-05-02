use super::*;
use crate::tools::context::McpToolOutput;
use codex_protocol::mcp::CallToolResult;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;

#[derive(Default)]
struct TestHandler;

impl ToolHandler for TestHandler {
    type Output = crate::tools::context::FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, _invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        Ok(crate::tools::context::FunctionToolOutput::from_text(
            "ok".to_string(),
            Some(true),
        ))
    }
}

#[test]
fn handler_looks_up_namespaced_aliases_explicitly() {
    let plain_handler = Arc::new(TestHandler) as Arc<dyn AnyToolHandler>;
    let namespaced_handler = Arc::new(TestHandler) as Arc<dyn AnyToolHandler>;
    let namespace = "mcp__codex_apps__gmail";
    let tool_name = "gmail_get_recent_emails";
    let plain_name = codex_tools::ToolName::plain(tool_name);
    let namespaced_name = codex_tools::ToolName::namespaced(namespace, tool_name);
    let registry = ToolRegistry::new(HashMap::from([
        (plain_name.clone(), Arc::clone(&plain_handler)),
        (namespaced_name.clone(), Arc::clone(&namespaced_handler)),
    ]));

    let plain = registry.handler(&plain_name);
    let namespaced = registry.handler(&namespaced_name);
    let missing_namespaced = registry.handler(&codex_tools::ToolName::namespaced(
        "mcp__codex_apps__calendar",
        tool_name,
    ));

    assert_eq!(plain.is_some(), true);
    assert_eq!(namespaced.is_some(), true);
    assert_eq!(missing_namespaced.is_none(), true);
    assert!(
        plain
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &plain_handler))
    );
    assert!(
        namespaced
            .as_ref()
            .is_some_and(|handler| Arc::ptr_eq(handler, &namespaced_handler))
    );
}

#[test]
fn model_visible_override_does_not_replace_typed_tool_output() {
    let result = mcp_result_with_model_visible_override();

    match result.into_response() {
        ResponseInputItem::FunctionCallOutput { call_id, output } => {
            assert_eq!(call_id, "mcp-call-1");
            assert_eq!(output.body.to_text().as_deref(), Some("[redacted]"));
        }
        other => panic!("expected FunctionCallOutput, got {other:?}"),
    }

    assert_eq!(
        mcp_result_with_model_visible_override().code_mode_result(),
        json!({
            "content": [],
            "structuredContent": {
                "echo": "original",
            },
            "isError": false,
        })
    );
}

fn mcp_result_with_model_visible_override() -> AnyToolResult {
    AnyToolResult {
        call_id: "mcp-call-1".to_string(),
        payload: ToolPayload::Mcp {
            server: "memory".to_string(),
            tool: "lookup".to_string(),
            raw_arguments: "{}".to_string(),
        },
        result: Box::new(McpToolOutput {
            result: CallToolResult {
                content: Vec::new(),
                structured_content: Some(json!({ "echo": "original" })),
                is_error: Some(false),
                meta: None,
            },
            tool_input: json!({}),
            wall_time: Duration::ZERO,
            original_image_detail_supported: false,
            truncation_policy: codex_utils_output_truncation::TruncationPolicy::Bytes(1024),
        }),
        post_tool_use_payload: None,
        model_visible_override: Some(FunctionToolOutput::from_text(
            "[redacted]".to_string(),
            Some(true),
        )),
    }
}
