use std::time::Duration;

use async_trait::async_trait;
use ocpp_proxy_application::ChargerCommand;
use ocpp_proxy_domain::OcppVersion;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::codec::status_accepted;
use crate::payload::{remote_start_payload, remote_stop_payload};
use crate::peer::Outbound;
use crate::state::ChargerLink;

#[async_trait]
impl ChargerCommand for ChargerLink {
    async fn remote_start(
        &self,
        version: OcppVersion,
        connector_id: i64,
        id_tag: &str,
    ) -> Result<bool, String> {
        let action = match version {
            OcppVersion::V16 => "RemoteStartTransaction",
            OcppVersion::V201 => "RequestStartTransaction",
        };
        let payload = remote_start_payload(version, connector_id, id_tag)?;
        let response = self.call(action, payload).await?;
        Ok(status_accepted(&response))
    }

    async fn remote_stop(
        &self,
        version: OcppVersion,
        transaction_id: &str,
    ) -> Result<bool, String> {
        let action = match version {
            OcppVersion::V16 => "RemoteStopTransaction",
            OcppVersion::V201 => "RequestStopTransaction",
        };
        let payload = remote_stop_payload(version, transaction_id)?;
        let response = self.call(action, payload).await?;
        Ok(status_accepted(&response))
    }
}

impl ChargerLink {
    async fn call(&self, action: &str, payload: Value) -> Result<Value, String> {
        let (reply, rx) = oneshot::channel();
        self.outbound
            .send(Outbound::Call {
                action: action.to_string(),
                payload,
                reply,
            })
            .map_err(|_| format!("charger is not connected for {action}"))?;
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!("charger dropped {action}")),
            Err(_) => Err(format!("charger timed out waiting for {action}")),
        }
    }
}
