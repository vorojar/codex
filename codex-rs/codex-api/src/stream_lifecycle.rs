//! Captures lifecycle breadcrumbs for Responses streams so transport failures can
//! include useful diagnostics in logs and error messages.

use crate::error::ApiError;
use crate::sse::ResponsesStreamEvent;
use std::fmt;
use std::time::Instant;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseStreamLifecycleOptions {
    pub attempt: u64,
    pub transport: ResponseStreamTransport,
}

impl ResponseStreamLifecycleOptions {
    pub fn new(attempt: u64, transport: ResponseStreamTransport) -> Self {
        Self { attempt, transport }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseStreamTransport {
    ResponsesHttp,
    ResponsesWebsocket,
}

impl ResponseStreamTransport {
    fn as_str(self) -> &'static str {
        match self {
            Self::ResponsesHttp => "responses_http",
            Self::ResponsesWebsocket => "responses_websocket",
        }
    }
}

impl fmt::Display for ResponseStreamTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResponseStreamTerminalState {
    Completed,
    ClosedBeforeCompletion,
    IdleTimeout,
    StreamError,
}

impl ResponseStreamTerminalState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ClosedBeforeCompletion => "closed_before_completion",
            Self::IdleTimeout => "idle_timeout",
            Self::StreamError => "stream_error",
        }
    }
}

impl fmt::Display for ResponseStreamTerminalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseStreamLifecycleSummary {
    options: ResponseStreamLifecycleOptions,
    terminal_state: ResponseStreamTerminalState,
    created_response_id: Option<String>,
    completed_response_id: Option<String>,
    first_event_elapsed_ms: Option<u64>,
    last_event_elapsed_ms: Option<u64>,
    last_event_kind: Option<String>,
    first_output_item_added_elapsed_ms: Option<u64>,
    first_output_item_done_elapsed_ms: Option<u64>,
    first_output_text_delta_elapsed_ms: Option<u64>,
    observed_event_kinds: Vec<String>,
    event_count: u64,
}

impl ResponseStreamLifecycleSummary {
    fn ids_mismatch(&self) -> bool {
        match (&self.created_response_id, &self.completed_response_id) {
            (Some(created), Some(completed)) => created != completed,
            _ => false,
        }
    }

    fn diagnostic_phrase(&self) -> String {
        match self.terminal_state {
            ResponseStreamTerminalState::Completed if self.ids_mismatch() => {
                "stream completed with mismatched response IDs".to_string()
            }
            ResponseStreamTerminalState::Completed => "stream completed".to_string(),
            ResponseStreamTerminalState::IdleTimeout if self.event_count == 0 => {
                "stream timed out before receiving any events".to_string()
            }
            ResponseStreamTerminalState::IdleTimeout
                if self.first_output_text_delta_elapsed_ms.is_some() =>
            {
                "stream timed out after durable text output started".to_string()
            }
            ResponseStreamTerminalState::IdleTimeout
                if self.first_output_item_done_elapsed_ms.is_some() =>
            {
                "stream timed out after response.output_item.done".to_string()
            }
            ResponseStreamTerminalState::IdleTimeout
                if self.first_output_item_added_elapsed_ms.is_some() =>
            {
                "stream timed out after response.output_item.added, before durable output"
                    .to_string()
            }
            ResponseStreamTerminalState::IdleTimeout => {
                "stream timed out after receiving events".to_string()
            }
            ResponseStreamTerminalState::ClosedBeforeCompletion if self.event_count == 0 => {
                "stream closed before response.completed and before receiving any events"
                    .to_string()
            }
            ResponseStreamTerminalState::ClosedBeforeCompletion => {
                "stream closed before response.completed".to_string()
            }
            ResponseStreamTerminalState::StreamError if self.event_count == 0 => {
                "stream failed before receiving any events".to_string()
            }
            ResponseStreamTerminalState::StreamError => {
                "stream failed after receiving events".to_string()
            }
        }
    }

    fn log(&self) {
        // Keep lifecycle logs focused on streams that did not complete normally,
        // plus the suspicious case where created/completed response IDs disagree.
        if self.terminal_state == ResponseStreamTerminalState::Completed && !self.ids_mismatch() {
            return;
        }

        let observed_event_kinds = self.observed_event_kinds.join(",");
        warn!(
            stream_attempt = self.options.attempt,
            stream_transport = %self.options.transport,
            stream_terminal_state = %self.terminal_state,
            stream_created_response_id = self.created_response_id.as_deref().unwrap_or(""),
            stream_completed_response_id = self.completed_response_id.as_deref().unwrap_or(""),
            stream_first_event_elapsed_ms = ?self.first_event_elapsed_ms,
            stream_last_event_elapsed_ms = ?self.last_event_elapsed_ms,
            stream_last_event_kind = self.last_event_kind.as_deref().unwrap_or(""),
            stream_first_output_item_added_elapsed_ms = ?self.first_output_item_added_elapsed_ms,
            stream_first_output_item_done_elapsed_ms = ?self.first_output_item_done_elapsed_ms,
            stream_first_output_text_delta_elapsed_ms = ?self.first_output_text_delta_elapsed_ms,
            stream_observed_event_kinds = %observed_event_kinds,
            stream_event_count = self.event_count,
            stream_diagnostic = %self.diagnostic_phrase(),
            "responses stream lifecycle"
        );
    }

    fn error_detail(&self) -> String {
        let mut parts = vec![
            format!("diagnostic: {}", self.diagnostic_phrase()),
            format!("transport={}", self.options.transport),
            format!("attempt={}", self.options.attempt),
            format!("terminal={}", self.terminal_state),
            format!("events={}", self.event_count),
        ];
        if let Some(kind) = &self.last_event_kind {
            parts.push(format!("last_event={kind}"));
        }
        if let Some(id) = &self.created_response_id {
            parts.push(format!("created_response_id={id}"));
        }
        if let Some(id) = &self.completed_response_id {
            parts.push(format!("completed_response_id={id}"));
        }
        if let Some(elapsed) = self.first_event_elapsed_ms {
            parts.push(format!("first_event_ms={elapsed}"));
        }
        if let Some(elapsed) = self.last_event_elapsed_ms {
            parts.push(format!("last_event_ms={elapsed}"));
        }
        if let Some(elapsed) = self.first_output_item_added_elapsed_ms {
            parts.push(format!("first_output_item_added_ms={elapsed}"));
        }
        if let Some(elapsed) = self.first_output_item_done_elapsed_ms {
            parts.push(format!("first_output_item_done_ms={elapsed}"));
        }
        if let Some(elapsed) = self.first_output_text_delta_elapsed_ms {
            parts.push(format!("first_output_text_delta_ms={elapsed}"));
        }
        if !self.observed_event_kinds.is_empty() {
            parts.push(format!(
                "observed_event_kinds={}",
                self.observed_event_kinds.join(",")
            ));
        }
        parts.join("; ")
    }

    fn decorate_error(&self, error: ApiError) -> ApiError {
        match error {
            ApiError::Stream(message) => ApiError::Stream(format!(
                "{message}. Stream lifecycle: {}",
                self.error_detail()
            )),
            other => other,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ResponseStreamLifecycleRecorder {
    options: ResponseStreamLifecycleOptions,
    started_at: Instant,
    created_response_id: Option<String>,
    completed_response_id: Option<String>,
    first_event_elapsed_ms: Option<u64>,
    last_event_elapsed_ms: Option<u64>,
    last_event_kind: Option<String>,
    first_output_item_added_elapsed_ms: Option<u64>,
    first_output_item_done_elapsed_ms: Option<u64>,
    first_output_text_delta_elapsed_ms: Option<u64>,
    observed_event_kinds: Vec<String>,
    event_count: u64,
    finalized: bool,
}

pub(crate) fn finalize_lifecycle_error(
    lifecycle: &mut Option<ResponseStreamLifecycleRecorder>,
    terminal_state: ResponseStreamTerminalState,
    error: ApiError,
) -> ApiError {
    if let Some(lifecycle) = lifecycle.as_mut() {
        lifecycle.finalize_error(terminal_state, error)
    } else {
        error
    }
}

impl ResponseStreamLifecycleRecorder {
    pub(crate) fn new(options: ResponseStreamLifecycleOptions) -> Self {
        Self {
            options,
            started_at: Instant::now(),
            created_response_id: None,
            completed_response_id: None,
            first_event_elapsed_ms: None,
            last_event_elapsed_ms: None,
            last_event_kind: None,
            first_output_item_added_elapsed_ms: None,
            first_output_item_done_elapsed_ms: None,
            first_output_text_delta_elapsed_ms: None,
            observed_event_kinds: Vec::new(),
            event_count: 0,
            finalized: false,
        }
    }

    pub(crate) fn observe_event(&mut self, event: &ResponsesStreamEvent) {
        let elapsed_ms = self.elapsed_ms();
        let kind = event.kind();
        self.event_count += 1;
        self.first_event_elapsed_ms.get_or_insert(elapsed_ms);
        self.last_event_elapsed_ms = Some(elapsed_ms);
        self.last_event_kind = Some(kind.to_string());
        if !self
            .observed_event_kinds
            .iter()
            .any(|observed| observed == kind)
        {
            self.observed_event_kinds.push(kind.to_string());
        }

        match kind {
            "response.created" => {
                if self.created_response_id.is_none() {
                    self.created_response_id = event.response_id().map(str::to_string);
                }
            }
            "response.completed" => {
                if self.completed_response_id.is_none() {
                    self.completed_response_id = event.response_id().map(str::to_string);
                }
            }
            "response.output_item.added" => {
                self.first_output_item_added_elapsed_ms
                    .get_or_insert(elapsed_ms);
            }
            "response.output_item.done" => {
                self.first_output_item_done_elapsed_ms
                    .get_or_insert(elapsed_ms);
            }
            "response.output_text.delta" => {
                self.first_output_text_delta_elapsed_ms
                    .get_or_insert(elapsed_ms);
            }
            _ => {}
        }
    }

    pub(crate) fn finalize_completed(&mut self) {
        let Some(summary) = self.finalize(ResponseStreamTerminalState::Completed) else {
            return;
        };
        summary.log();
    }

    pub(crate) fn finalize_error(
        &mut self,
        terminal_state: ResponseStreamTerminalState,
        error: ApiError,
    ) -> ApiError {
        let Some(summary) = self.finalize(terminal_state) else {
            return error;
        };
        summary.log();
        summary.decorate_error(error)
    }

    fn finalize(
        &mut self,
        terminal_state: ResponseStreamTerminalState,
    ) -> Option<ResponseStreamLifecycleSummary> {
        if self.finalized {
            return None;
        }
        self.finalized = true;
        Some(ResponseStreamLifecycleSummary {
            options: self.options,
            terminal_state,
            created_response_id: self.created_response_id.clone(),
            completed_response_id: self.completed_response_id.clone(),
            first_event_elapsed_ms: self.first_event_elapsed_ms,
            last_event_elapsed_ms: self.last_event_elapsed_ms,
            last_event_kind: self.last_event_kind.clone(),
            first_output_item_added_elapsed_ms: self.first_output_item_added_elapsed_ms,
            first_output_item_done_elapsed_ms: self.first_output_item_done_elapsed_ms,
            first_output_text_delta_elapsed_ms: self.first_output_text_delta_elapsed_ms,
            observed_event_kinds: self.observed_event_kinds.clone(),
            event_count: self.event_count,
        })
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn records_response_ids_milestones_and_observed_kinds_in_order() {
        let mut recorder =
            ResponseStreamLifecycleRecorder::new(ResponseStreamLifecycleOptions::new(
                /*attempt*/ 2,
                ResponseStreamTransport::ResponsesHttp,
            ));

        for event in [
            json!({"type": "response.created", "response": {"id": "resp-created"}}),
            json!({"type": "response.output_item.added", "item": {"type": "message"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message"}}),
            json!({"type": "response.output_text.delta", "delta": " again"}),
            json!({"type": "response.completed", "response": {"id": "resp-completed"}}),
        ] {
            let event: ResponsesStreamEvent = serde_json::from_value(event).unwrap();
            recorder.observe_event(&event);
        }

        let summary = recorder
            .finalize(ResponseStreamTerminalState::Completed)
            .expect("summary should finalize");

        assert_eq!(summary.created_response_id.as_deref(), Some("resp-created"));
        assert_eq!(
            summary.completed_response_id.as_deref(),
            Some("resp-completed")
        );
        assert_eq!(
            summary.observed_event_kinds,
            vec![
                "response.created",
                "response.output_item.added",
                "response.output_text.delta",
                "response.output_item.done",
                "response.completed"
            ]
        );
        assert_eq!(summary.event_count, 6);
        assert!(summary.first_output_item_added_elapsed_ms.is_some());
        assert!(summary.first_output_item_done_elapsed_ms.is_some());
        assert!(summary.first_output_text_delta_elapsed_ms.is_some());
        assert!(summary.ids_mismatch());
    }

    #[test]
    fn stream_errors_gain_diagnostic_context() {
        let mut recorder =
            ResponseStreamLifecycleRecorder::new(ResponseStreamLifecycleOptions::new(
                /*attempt*/ 1,
                ResponseStreamTransport::ResponsesWebsocket,
            ));
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.output_item.added",
            "item": {"type": "message"}
        }))
        .unwrap();
        recorder.observe_event(&event);

        let error = recorder.finalize_error(
            ResponseStreamTerminalState::IdleTimeout,
            ApiError::Stream("idle timeout waiting for websocket".to_string()),
        );

        let ApiError::Stream(message) = error else {
            panic!("expected stream error");
        };
        assert!(message.contains("stream timed out after response.output_item.added"));
        assert!(message.contains("transport=responses_websocket"));
        assert!(message.contains("observed_event_kinds=response.output_item.added"));
    }
}
