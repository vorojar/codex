use std::sync::Arc;

use tracing::debug;

use crate::session::session::Session;

impl Session {
    pub(crate) async fn schedule_startup_auth_prewarm(self: &Arc<Self>) {
        let model_client = self.services.model_client.clone();
        tokio::spawn(async move {
            if let Err(err) = model_client.prewarm_provider_auth().await {
                debug!("startup provider auth prewarm failed: {err:#}");
            }
        });
    }
}
