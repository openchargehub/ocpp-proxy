use std::collections::HashMap;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use chrono::Utc;
use ocpp_proxy_application::{
    fault_message_v16, fault_message_v201, note_fault, notify_session, on_boot, on_heartbeat,
    on_meter, on_start, on_start_v201, on_status, on_transaction_stop, ChargerEvent, ChargerRegistry,
    EnrollOutcome, Notifier, StartInput, StopInput,
};
use ocpp_proxy_domain::{
    parse_station_path, resolve_version, version_hint_path, ConnectionHints, OcppVersion,
    StationRoute,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::codec::Frame;
use crate::payload::{
    boot_result, formation, heartbeat_result, json_i64, json_str, read_boot, start_result_v16,
    stop_result_v16, transaction_event_result,
};
use crate::peer::{run_peer, AxumIo, InboundCall, Outbound};
use crate::backend::open_provider;
use crate::state::{AppState, ChargerLink, StationHandle};

pub async fn charger_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    uri: OriginalUri,
) -> impl IntoResponse {
    let route = match parse_station_path(uri.path()) {
        Ok(route) => route,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };
    let id = match route {
        StationRoute::Provider { id } => {
            let provider = query
                .get("id")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            return open_provider(ws, state, id, provider).into_response();
        }
        StationRoute::ChargePoint { id } => id,
    };
    let record = match prepare_charger(&state, &id) {
        Ok(record) => record,
        Err(response) => return response,
    };
    let station = state.attach(&record);
    let offered = ws
        .requested_protocols()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join(",");
    let mut hints = connection_hints(&headers, &query, uri.path());
    hints.path = version_hint_path(uri.path());
    if !offered.is_empty() {
        hints.subprotocol = Some(offered.clone());
    }
    let version = match resolve_version(&record.ocpp_version, record.auto_detect, &hints) {
        Ok(version) => version,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };
    let upgrade = if offered.to_ascii_lowercase().contains(version.subprotocol()) {
        ws.protocols([version.subprotocol()])
    } else {
        ws
    };
    upgrade
        .on_upgrade(move |socket| run_charger(socket, state, station, version))
        .into_response()
}

fn prepare_charger(
    state: &AppState,
    id: &str,
) -> Result<ocpp_proxy_application::ChargerRecord, axum::response::Response> {
    match state.store().get_charger(id) {
        Ok(Some(row)) if !row.enabled => Err(StatusCode::FORBIDDEN.into_response()),
        Ok(Some(row)) => Ok(row),
        Ok(None) => match state.store().enroll(id, &state.defaults()) {
            Ok(EnrollOutcome::Created(row) | EnrollOutcome::Existing(row)) if row.enabled => {
                Ok(row)
            }
            Ok(_) => Err(StatusCode::FORBIDDEN.into_response()),
            Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err).into_response()),
        },
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err).into_response()),
    }
}

async fn run_charger(
    socket: WebSocket,
    state: AppState,
    station: StationHandle,
    version: OcppVersion,
) {
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let cancel = station.cancel().child_token();
    let generation = station.replace_charger(
        ChargerLink {
            version,
            outbound: out_tx.clone(),
        },
        cancel.clone(),
    );
    station.set_version(version);
    let _ = state
        .store()
        .touch_last_seen(station.id(), &state.clock_iso());
    let cancel_peer = cancel.clone();
    let reader = tokio::spawn(async move {
        run_peer(AxumIo { socket }, out_rx, in_tx, cancel_peer).await;
    });
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            call = in_rx.recv() => {
                let Some(call) = call else { break };
                dispatch(&state, &station, version, call, &out_tx).await;
            }
        }
    }
    station.clear_charger(generation);
    station.clear_version();
    let _ = reader.await;
    tracing::info!(charger = station.id(), version = version.as_str(), "charger disconnected");
}

async fn dispatch(
    state: &AppState,
    station: &StationHandle,
    version: OcppVersion,
    call: InboundCall,
    outbound: &mpsc::UnboundedSender<Outbound>,
) {
    let frame = match handle(state, station, version, &call).await {
        Ok(payload) => Frame::Result {
            id: call.id,
            payload,
        },
        Err(err) => formation(&call.id, &err),
    };
    let _ = outbound.send(match frame {
        Frame::Result { id, payload } => Outbound::Result { id, payload },
        Frame::Error {
            id,
            code,
            description,
            ..
        } => Outbound::Error {
            id,
            code,
            description,
        },
        Frame::Call { .. } => return,
    });
}

async fn handle(
    state: &AppState,
    station: &StationHandle,
    version: OcppVersion,
    call: &InboundCall,
) -> Result<Value, String> {
    match (version, call.action.as_str()) {
        (_, "BootNotification") => boot(state, station, version, &call.payload).await,
        (_, "Heartbeat") => heartbeat(state, station, version).await,
        (OcppVersion::V16, "StatusNotification") => status_v16(state, station, &call.payload).await,
        (OcppVersion::V201, "StatusNotification") => {
            status_v201(state, station, &call.payload).await
        }
        (OcppVersion::V16, "MeterValues") => meter_v16(station, &call.payload).await,
        (OcppVersion::V201, "MeterValues") => meter_v201(station, &call.payload).await,
        (OcppVersion::V16, "StartTransaction") => start_v16(station, &call.payload).await,
        (OcppVersion::V16, "StopTransaction") => stop_v16(state, station, &call.payload).await,
        (OcppVersion::V201, "TransactionEvent") => {
            transaction_event(state, station, &call.payload).await
        }
        _ => Err(format!(
            "unsupported action {} for OCPP {}",
            call.action,
            version.as_str()
        )),
    }
}

async fn boot(
    state: &AppState,
    station: &StationHandle,
    version: OcppVersion,
    payload: &Value,
) -> Result<Value, String> {
    let (vendor, model, reason) = read_boot(version, payload)?;
    let now = Utc::now();
    let body = boot_result(version, now)?;
    let current = body
        .get("currentTime")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    station.set_boot(&vendor, &model);
    let _ = state
        .store()
        .touch_last_seen(station.id(), &state.clock_iso());
    on_boot(
        &vendor,
        &model,
        reason,
        &current,
        station.backends(),
        station.upstream(),
    );
    tracing::info!(%vendor, %model, version = version.as_str(), "charger booted");
    Ok(body)
}

async fn heartbeat(
    state: &AppState,
    station: &StationHandle,
    version: OcppVersion,
) -> Result<Value, String> {
    let body = heartbeat_result(version, Utc::now())?;
    let current = body
        .get("currentTime")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let _ = state
        .store()
        .touch_last_seen(station.id(), &state.clock_iso());
    on_heartbeat(&current, station.backends(), station.upstream());
    Ok(body)
}

async fn status_v16(
    state: &AppState,
    station: &StationHandle,
    payload: &Value,
) -> Result<Value, String> {
    let status = json_str(payload, "status").unwrap_or("").to_string();
    let error = json_str(payload, "errorCode").unwrap_or("").to_string();
    let event = ChargerEvent::Status {
        connector_id: json_i64(payload, "connectorId", 0),
        evse_id: None,
        error_code: Some(error.clone()),
        status: status.clone(),
        timestamp: None,
    };
    on_status(event, station.backends(), station.upstream());
    if note_fault(
        station.arbiter(),
        state,
        &status,
        &fault_message_v16(&status, &error),
    )
    .await
    {
        tracing::info!(%status, %error, "charger fault revoked control");
    }
    Ok(serde_json::json!({}))
}

async fn status_v201(
    state: &AppState,
    station: &StationHandle,
    payload: &Value,
) -> Result<Value, String> {
    let status = json_str(payload, "connectorStatus")
        .unwrap_or("")
        .to_string();
    let evse = json_i64(payload, "evseId", 0);
    let connector = json_i64(payload, "connectorId", 0);
    let timestamp = json_str(payload, "timestamp").map(str::to_string);
    let event = ChargerEvent::Status {
        connector_id: connector,
        evse_id: Some(evse),
        error_code: None,
        status: status.clone(),
        timestamp,
    };
    on_status(event, station.backends(), station.upstream());
    let message = fault_message_v201(&status, evse, connector);
    let _ = note_fault(station.arbiter(), state, &status, &message).await;
    Ok(serde_json::json!({}))
}

async fn meter_v16(station: &StationHandle, payload: &Value) -> Result<Value, String> {
    let event = ChargerEvent::Meter {
        connector_id: Some(json_i64(payload, "connectorId", 0)),
        evse_id: None,
        values: payload
            .get("meterValue")
            .cloned()
            .unwrap_or(serde_json::json!([])),
    };
    on_meter(event, station.backends(), station.upstream());
    Ok(serde_json::json!({}))
}

async fn meter_v201(station: &StationHandle, payload: &Value) -> Result<Value, String> {
    let event = ChargerEvent::Meter {
        connector_id: None,
        evse_id: Some(json_i64(payload, "evseId", 0)),
        values: payload
            .get("meterValue")
            .cloned()
            .unwrap_or(serde_json::json!([])),
    };
    on_meter(event, station.backends(), station.upstream());
    Ok(serde_json::json!({}))
}

async fn start_v16(station: &StationHandle, payload: &Value) -> Result<Value, String> {
    let timestamp = json_str(payload, "timestamp").unwrap_or("").to_string();
    let id_tag = json_str(payload, "idTag").unwrap_or("").to_string();
    let tx = {
        let mut ledger = station.ledger();
        on_start(
            &mut ledger,
            StartInput {
                connector_id: json_i64(payload, "connectorId", 1),
                id_tag: &id_tag,
                meter_start: json_i64(payload, "meterStart", 0),
                timestamp: &timestamp,
            },
            station.backends(),
            station.upstream(),
        )
    };
    start_result_v16(tx)
}

async fn stop_v16(
    state: &AppState,
    station: &StationHandle,
    payload: &Value,
) -> Result<Value, String> {
    let tx = json_i64(payload, "transactionId", 0);
    finish_stop(
        state,
        station,
        &tx.to_string(),
        json_i64(payload, "meterStop", 0),
        payload,
        serde_json::json!(tx),
    )
    .await?;
    stop_result_v16()
}

async fn transaction_event(
    state: &AppState,
    station: &StationHandle,
    payload: &Value,
) -> Result<Value, String> {
    let event_type = json_str(payload, "eventType").unwrap_or("");
    let timestamp = json_str(payload, "timestamp").unwrap_or("").to_string();
    let tx = payload
        .pointer("/transactionInfo/transactionId")
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_default();
    let meter = first_meter(payload);
    let connector = payload
        .pointer("/evse/id")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let id_tag = payload
        .pointer("/idToken/idToken")
        .and_then(Value::as_str)
        .unwrap_or("");
    match event_type {
        "Started" => {
            {
                let mut ledger = station.ledger();
                on_start_v201(
                    &mut ledger,
                    &tx,
                    StartInput {
                        connector_id: connector,
                        id_tag,
                        meter_start: meter,
                        timestamp: &timestamp,
                    },
                    station.backends(),
                    station.upstream(),
                );
            }
            transaction_event_result(true)
        }
        "Ended" => {
            let tx_json = payload
                .pointer("/transactionInfo/transactionId")
                .cloned()
                .unwrap_or(serde_json::json!(tx));
            finish_stop(state, station, &tx, meter, payload, tx_json).await?;
            transaction_event_result(true)
        }
        _ => transaction_event_result(false),
    }
}

async fn finish_stop(
    state: &AppState,
    station: &StationHandle,
    tx_id: &str,
    meter_stop: i64,
    payload: &Value,
    tx_json: Value,
) -> Result<(), String> {
    let timestamp = json_str(payload, "timestamp").unwrap_or("").to_string();
    let meter_stop = if payload.get("meterStop").is_some() {
        meter_stop
    } else {
        first_meter(payload)
    };
    let owner = station
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .lock_owner()
        .map(str::to_string);
    let message = {
        let mut ledger = station.ledger();
        let logged_at = state.clock_iso();
        on_transaction_stop(
            &mut ledger,
            state.store(),
            StopInput {
                tx_id,
                meter_stop,
                timestamp: &timestamp,
                transaction_id: tx_json,
                owner: owner.as_deref(),
                logged_at: &logged_at,
                charger_id: station.id(),
            },
            station.backends(),
            station.upstream(),
        )
    };
    if let Some(message) = message {
        notify_session(state, &message).await;
    }
    Ok(())
}

fn first_meter(payload: &Value) -> i64 {
    payload
        .pointer("/meterValue/0/sampledValue/0/value")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .unwrap_or(0)
}

fn connection_hints(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    path: &str,
) -> ConnectionHints {
    let subprotocol = header_text(headers, "sec-websocket-protocol");
    let mut header_pairs = Vec::new();
    for (name, value) in headers.iter() {
        if let Ok(text) = value.to_str() {
            header_pairs.push((name.as_str().to_string(), text.to_string()));
        }
    }
    ConnectionHints {
        subprotocol: if subprotocol.is_empty() {
            None
        } else {
            Some(subprotocol)
        },
        headers: header_pairs,
        path: Some(path.to_string()),
        query: query
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[async_trait::async_trait]
impl Notifier for AppState {
    async fn notify(&self, title: &str, message: &str) -> Result<(), String> {
        match self.ha() {
            Some(client) => client.notify(title, message).await.map(|_| ()),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl ocpp_proxy_application::EntityState for AppState {
    async fn state_of(&self, entity_id: &str) -> Result<String, String> {
        match self.ha() {
            Some(client) => client.entity_state(entity_id).await,
            None => Err(format!("Home Assistant is not configured for {entity_id}")),
        }
    }
}
