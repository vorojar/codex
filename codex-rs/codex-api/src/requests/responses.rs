use crate::common::ResponsesApiRequest;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_conversation_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Compression {
    #[default]
    None,
    Zstd,
}

pub(crate) fn build_responses_request_body(
    request: &ResponsesApiRequest,
    provider: &Provider,
) -> Result<Value, ApiError> {
    let mut body = serde_json::to_value(request)
        .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
    if request.store && provider.is_azure_responses_endpoint() {
        attach_item_ids(&mut body, &request.input);
    }
    Ok(body)
}

pub(crate) fn build_responses_request_headers(
    mut extra_headers: HeaderMap,
    conversation_id: Option<String>,
    session_source: Option<SessionSource>,
) -> HeaderMap {
    if let Some(ref conv_id) = conversation_id {
        insert_header(&mut extra_headers, "x-client-request-id", conv_id);
    }
    extra_headers.extend(build_conversation_headers(conversation_id));
    if let Some(subagent) = subagent_header(&session_source) {
        insert_header(&mut extra_headers, "x-openai-subagent", &subagent);
    }
    extra_headers
}

pub(crate) fn attach_item_ids(payload_json: &mut Value, original_items: &[ResponseItem]) {
    let Some(input_value) = payload_json.get_mut("input") else {
        return;
    };
    let Value::Array(items) = input_value else {
        return;
    };

    for (value, item) in items.iter_mut().zip(original_items.iter()) {
        if let ResponseItem::Reasoning { id, .. }
        | ResponseItem::Message { id: Some(id), .. }
        | ResponseItem::WebSearchCall { id: Some(id), .. }
        | ResponseItem::FunctionCall { id: Some(id), .. }
        | ResponseItem::ToolSearchCall { id: Some(id), .. }
        | ResponseItem::LocalShellCall { id: Some(id), .. }
        | ResponseItem::CustomToolCall { id: Some(id), .. } = item
        {
            if id.is_empty() {
                continue;
            }

            if let Some(obj) = value.as_object_mut() {
                obj.insert("id".to_string(), Value::String(id.clone()));
            }
        }
    }
}
