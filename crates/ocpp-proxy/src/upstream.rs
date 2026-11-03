use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ocpp_proxy_application::{
    remote_start, remote_stop, ChargerEvent, ChargerRegistry, RemoteStart, RemoteStop, ServiceRecord,
    StartDecision, StopDecision,
};
use ocpp_proxy_domain::{OcppServiceConfig, OcppVersion};
use tokio_util::sync::CancellationToken;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::payload::{remote_result, stop_remote_result, upstream_boot};
use crate::peer::{run_peer, InboundCall, Outbound, TungsteniteIo};
use crate::state::{AppState, StationHandle};

pub fn spawn_services(state: &AppState, station: &StationHandle) {
    let services = match state.store().list_services(station.id()) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(charger = station.id(), error = %err, "failed to read outbound services");
            return;
        }
    };
    let cancel = station.replace_service_cancel();
    for service in services {
        if !service.enabled || service.url.is_empty() {
            continue;
        }
        let state = state.clone();
        let station = station.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(err) = run_service(service, state, station, cancel).await {
                tracing::error!(error = %err, "upstream service stopped");
            }
        });
    }
}

async fn run_service(
    service: ServiceRecord,
    state: AppState,
    station: StationHandle,
    cancel: CancellationToken,
) -> Result<(), String> {
    let version = parse_version(&service.version);
    let id = service.id.clone();
    let config = to_config(&service);
    let socket = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        connected = connect_service(&config, version) => match connected {
            Ok(socket) => socket,
            Err(err) => {
                tracing::error!(service = %id, url = %config.url, error = %err, "failed to connect upstream");
                return Ok(());
            }
        }
    };
    station
        .upstream()
        .set_status(&id, true, true, version.as_str());
    tracing::info!(service = %id, version = version.as_str(), "upstream connected");
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let peer_cancel = cancel.clone();
    let reader = tokio::spawn(async move {
        run_peer(TungsteniteIo { socket }, out_rx, in_tx, peer_cancel).await;
    });
    let link = crate::state::ChargerLink {
        version,
        outbound: out_tx.clone(),
    };
    let _ = link.call_boot().await;
    let mut events = station.upstream().subscribe();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            call = in_rx.recv() => {
                let Some(call) = call else { break };
                let payload = on_upstream_call(&state, &station, &id, version, &call).await;
                let _ = out_tx.send(Outbound::Result { id: call.id, payload });
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if let Some((action, payload)) = translate(version, &event) {
                            let _ = link.call_raw(&action, payload).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    station.upstream().remove(&id);
    cancel.cancel();
    let _ = reader.await;
    Ok(())
}

async fn connect_service(
    service: &OcppServiceConfig,
    version: OcppVersion,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let mut request = service
        .url
        .clone()
        .into_client_request()
        .map_err(|err| format!("invalid upstream url {}: {err}", service.url))?;
    if let Some(header) = auth_header(service) {
        request.headers_mut().insert(
            http::header::AUTHORIZATION,
            header
                .parse()
                .map_err(|err| format!("invalid authorization header for {}: {err}", service.id))?,
        );
    }
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        version
            .subprotocol()
            .parse()
            .map_err(|err| format!("invalid subprotocol: {err}"))?,
    );
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(request))
        .await
        .map_err(|_| format!("timed out connecting to {}", service.url))?
        .map_err(|err| format!("connect {} failed: {err}", service.url))?;
    Ok(socket)
}

fn auth_header(service: &OcppServiceConfig) -> Option<String> {
    match service.auth_type.as_deref() {
        Some("basic") => {
            let username = service.username.as_deref().unwrap_or("");
            let password = service.password.as_deref().unwrap_or("");
            if username.is_empty() || password.is_empty() {
                return None;
            }
            let token = STANDARD.encode(format!("{username}:{password}"));
            Some(format!("Basic {token}"))
        }
        Some("token") => service.token.clone().map(|token| format!("Bearer {token}")),
        _ => None,
    }
}

fn to_config(service: &ServiceRecord) -> OcppServiceConfig {
    OcppServiceConfig {
        id: service.id.clone(),
        url: service.url.clone(),
        version: Some(service.version.clone()),
        auth_type: blank(&service.auth_type),
        username: blank(&service.username),
        password: blank(&service.password),
        token: blank(&service.token),
        enabled: service.enabled,
    }
}

fn blank(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_version(version: &str) -> OcppVersion {
    if version == "2.0.1" {
        OcppVersion::V201
    } else {
        OcppVersion::V16
    }
}

async fn on_upstream_call(
    state: &AppState,
    station: &StationHandle,
    service_id: &str,
    version: OcppVersion,
    call: &InboundCall,
) -> Value {
    let backend_id = format!("ocpp_service_{service_id}");
    let Some(charger) = station.charger_link() else {
        return rejected(version, &call.action);
    };
    match call.action.as_str() {
        "RemoteStartTransaction" | "RequestStartTransaction" => {
            let connector = call
                .payload
                .get("connectorId")
                .or_else(|| call.payload.get("evseId"))
                .and_then(Value::as_i64)
                .unwrap_or(1);
            let id_tag = call
                .payload
                .get("idTag")
                .and_then(Value::as_str)
                .or_else(|| {
                    call.payload
                        .pointer("/idToken/idToken")
                        .and_then(Value::as_str)
                })
                .unwrap_or("")
                .to_string();
            let decision = remote_start(
                station.arbiter(),
                state,
                &charger,
                state.clock(),
                &RemoteStart {
                    version: charger.version,
                    backend_id,
                    connector_id: connector,
                    id_tag,
                },
            )
            .await;
            let accepted = matches!(decision, StartDecision::Accepted);
            remote_result(version, accepted)
                .unwrap_or_else(|_| serde_json::json!({"status": "Rejected"}))
        }
        "RemoteStopTransaction" | "RequestStopTransaction" => {
            let transaction_id = call
                .payload
                .get("transactionId")
                .map(|value| match value {
                    Value::String(text) => text.clone(),
                    Value::Number(number) => number.to_string(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            let decision = remote_stop(
                station.arbiter(),
                state,
                &charger,
                state.clock(),
                &RemoteStop {
                    version: charger.version,
                    backend_id,
                    transaction_id,
                    acquire_lock: true,
                },
            )
            .await;
            let accepted = matches!(decision, StopDecision::Accepted);
            stop_remote_result(version, accepted)
                .unwrap_or_else(|_| serde_json::json!({"status": "Rejected"}))
        }
        _ => rejected(version, &call.action),
    }
}

fn rejected(version: OcppVersion, _action: &str) -> Value {
    remote_result(version, false).unwrap_or_else(|_| serde_json::json!({"status": "Rejected"}))
}

fn translate(version: OcppVersion, event: &ChargerEvent) -> Option<(String, Value)> {
    match (version, event) {
        (_, ChargerEvent::Boot { .. }) => None,
        (OcppVersion::V16, ChargerEvent::Heartbeat { .. }) => {
            Some(("Heartbeat".into(), serde_json::json!({})))
        }
        (OcppVersion::V201, ChargerEvent::Heartbeat { .. }) => {
            Some(("Heartbeat".into(), serde_json::json!({})))
        }
        (
            OcppVersion::V16,
            ChargerEvent::Status {
                connector_id,
                error_code,
                status,
                ..
            },
        ) => Some((
            "StatusNotification".into(),
            serde_json::json!({
                "connectorId": connector_id,
                "errorCode": error_code.clone().unwrap_or_else(|| "NoError".into()),
                "status": status,
            }),
        )),
        (
            OcppVersion::V201,
            ChargerEvent::Status {
                connector_id,
                evse_id,
                status,
                timestamp,
                ..
            },
        ) => Some((
            "StatusNotification".into(),
            serde_json::json!({
                "timestamp": timestamp.clone().unwrap_or_else(|| "1970-01-01T00:00:00Z".into()),
                "connectorStatus": status,
                "evseId": evse_id.unwrap_or(1),
                "connectorId": connector_id,
            }),
        )),
        (
            OcppVersion::V16,
            ChargerEvent::Meter {
                connector_id,
                values,
                ..
            },
        ) => Some((
            "MeterValues".into(),
            serde_json::json!({"connectorId": connector_id.unwrap_or(1), "meterValue": values}),
        )),
        (
            OcppVersion::V201,
            ChargerEvent::Meter {
                evse_id, values, ..
            },
        ) => Some((
            "MeterValues".into(),
            serde_json::json!({"evseId": evse_id.unwrap_or(1), "meterValue": values}),
        )),
        (
            version,
            ChargerEvent::TransactionStarted {
                transaction_id,
                connector_id,
                id_tag,
                meter_start,
                timestamp,
            },
        ) => Some(transaction_call(
            version,
            "Started",
            transaction_id,
            *connector_id,
            id_tag,
            *meter_start,
            timestamp,
        )),
        (
            version,
            ChargerEvent::TransactionStopped {
                transaction_id,
                meter_stop,
                timestamp,
            },
        ) => Some(transaction_call(
            version,
            "Ended",
            transaction_id,
            1,
            "",
            *meter_stop,
            timestamp,
        )),
    }
}

fn transaction_call(
    version: OcppVersion,
    kind: &str,
    transaction_id: &Value,
    connector_id: i64,
    id_tag: &str,
    meter: i64,
    timestamp: &str,
) -> (String, Value) {
    match version {
        OcppVersion::V16 if kind == "Started" => (
            "StartTransaction".into(),
            serde_json::json!({
                "connectorId": connector_id,
                "idTag": id_tag,
                "meterStart": meter,
                "timestamp": timestamp,
            }),
        ),
        OcppVersion::V16 => (
            "StopTransaction".into(),
            serde_json::json!({
                "transactionId": transaction_id,
                "meterStop": meter,
                "timestamp": timestamp,
            }),
        ),
        OcppVersion::V201 => (
            "TransactionEvent".into(),
            serde_json::json!({
                "eventType": kind,
                "timestamp": timestamp,
                "triggerReason": "RemoteStart",
                "seqNo": 0,
                "transactionInfo": {"transactionId": transaction_id.as_str().unwrap_or("").to_string()},
                "evse": {"id": connector_id},
                "idToken": {"idToken": id_tag, "type": "ISO14443"},
            }),
        ),
    }
}

impl crate::state::ChargerLink {
    async fn call_boot(&self) -> Result<Value, String> {
        self.call_raw("BootNotification", upstream_boot(self.version)?)
            .await
    }

    async fn call_raw(&self, action: &str, payload: Value) -> Result<Value, String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.outbound
            .send(Outbound::Call {
                action: action.to_string(),
                payload,
                reply,
            })
            .map_err(|_| format!("upstream socket closed before {action}"))?;
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => result,
            _ => Err(format!("upstream did not answer {action}")),
        }
    }
}
