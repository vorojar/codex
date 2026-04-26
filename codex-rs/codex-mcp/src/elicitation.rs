use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use crate::mcp::mcp_permission_prompt_is_auto_approved;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use async_channel::Sender;
use codex_protocol::approvals::ElicitationRequest;
use codex_protocol::approvals::ElicitationRequestEvent;
use codex_protocol::mcp::RequestId as ProtocolRequestId;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SandboxPolicy;
use codex_rmcp_client::ElicitationResponse;
use codex_rmcp_client::SendElicitation;
use futures::future::FutureExt;
use rmcp::model::CreateElicitationRequestParams;
use rmcp::model::ElicitationAction;
use rmcp::model::RequestId;
use tokio::sync::Mutex;
use tokio::sync::oneshot;

type ResponderMap = HashMap<(String, RequestId), oneshot::Sender<ElicitationResponse>>;

pub(crate) fn elicitation_is_rejected_by_policy(approval_policy: AskForApproval) -> bool {
    match approval_policy {
        AskForApproval::Never => true,
        AskForApproval::OnFailure => false,
        AskForApproval::OnRequest => false,
        AskForApproval::UnlessTrusted => false,
        AskForApproval::Granular(granular_config) => !granular_config.allows_mcp_elicitations(),
    }
}

fn can_auto_accept_elicitation(elicitation: &CreateElicitationRequestParams) -> bool {
    match elicitation {
        CreateElicitationRequestParams::FormElicitationParams {
            requested_schema, ..
        } => {
            // Auto-accept confirm/approval elicitations without schema requirements.
            requested_schema.properties.is_empty()
        }
        CreateElicitationRequestParams::UrlElicitationParams { .. } => false,
    }
}

#[derive(Clone)]
pub(crate) struct ElicitationRequestManager {
    requests: Arc<Mutex<ResponderMap>>,
    pub(crate) approval_policy: Arc<StdMutex<AskForApproval>>,
    pub(crate) sandbox_policy: Arc<StdMutex<SandboxPolicy>>,
}

impl ElicitationRequestManager {
    pub(crate) fn new(approval_policy: AskForApproval, sandbox_policy: SandboxPolicy) -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
            approval_policy: Arc::new(StdMutex::new(approval_policy)),
            sandbox_policy: Arc::new(StdMutex::new(sandbox_policy)),
        }
    }

    pub(crate) async fn resolve(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> Result<()> {
        self.requests
            .lock()
            .await
            .remove(&(server_name, id))
            .ok_or_else(|| anyhow!("elicitation request not found"))?
            .send(response)
            .map_err(|e| anyhow!("failed to send elicitation response: {e:?}"))
    }

    pub(crate) fn make_sender(
        &self,
        server_name: String,
        tx_event: Sender<Event>,
    ) -> SendElicitation {
        let elicitation_requests = self.requests.clone();
        let approval_policy = self.approval_policy.clone();
        let sandbox_policy = self.sandbox_policy.clone();
        Box::new(move |id, elicitation| {
            let elicitation_requests = elicitation_requests.clone();
            let tx_event = tx_event.clone();
            let server_name = server_name.clone();
            let approval_policy = approval_policy.clone();
            let sandbox_policy = sandbox_policy.clone();
            async move {
                let approval_policy = approval_policy
                    .lock()
                    .map(|policy| *policy)
                    .unwrap_or(AskForApproval::Never);
                let sandbox_policy = sandbox_policy
                    .lock()
                    .map(|policy| policy.clone())
                    .unwrap_or_else(|_| SandboxPolicy::new_read_only_policy());
                if mcp_permission_prompt_is_auto_approved(approval_policy, &sandbox_policy)
                    && can_auto_accept_elicitation(&elicitation)
                {
                    return Ok(ElicitationResponse {
                        action: ElicitationAction::Accept,
                        content: Some(serde_json::json!({})),
                        meta: None,
                    });
                }

                if elicitation_is_rejected_by_policy(approval_policy) {
                    return Ok(ElicitationResponse {
                        action: ElicitationAction::Decline,
                        content: None,
                        meta: None,
                    });
                }

                let request = match elicitation {
                    CreateElicitationRequestParams::FormElicitationParams {
                        meta,
                        message,
                        requested_schema,
                    } => ElicitationRequest::Form {
                        meta: meta
                            .map(serde_json::to_value)
                            .transpose()
                            .context("failed to serialize MCP elicitation metadata")?,
                        message,
                        requested_schema: serde_json::to_value(requested_schema)
                            .context("failed to serialize MCP elicitation schema")?,
                    },
                    CreateElicitationRequestParams::UrlElicitationParams {
                        meta,
                        message,
                        url,
                        elicitation_id,
                    } => ElicitationRequest::Url {
                        meta: meta
                            .map(serde_json::to_value)
                            .transpose()
                            .context("failed to serialize MCP elicitation metadata")?,
                        message,
                        url,
                        elicitation_id,
                    },
                };
                let (tx, rx) = oneshot::channel();
                {
                    let mut lock = elicitation_requests.lock().await;
                    lock.insert((server_name.clone(), id.clone()), tx);
                }
                let _ = tx_event
                    .send(Event {
                        id: "mcp_elicitation_request".to_string(),
                        msg: EventMsg::ElicitationRequest(ElicitationRequestEvent {
                            turn_id: None,
                            server_name,
                            id: match id.clone() {
                                rmcp::model::NumberOrString::String(value) => {
                                    ProtocolRequestId::String(value.to_string())
                                }
                                rmcp::model::NumberOrString::Number(value) => {
                                    ProtocolRequestId::Integer(value)
                                }
                            },
                            request,
                        }),
                    })
                    .await;
                rx.await
                    .context("elicitation request channel closed unexpectedly")
            }
            .boxed()
        })
    }
}
