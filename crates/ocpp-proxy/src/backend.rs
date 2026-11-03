use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ocpp_proxy_application::{
    remote_start, remote_stop, ChargerRegistry, RemoteStart, RemoteStop, StartDecision,
    StopDecision,
};
use ocpp_proxy_domain::DEFAULT_STATION_ID;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::state::{AppState, StationHandle};

#[derive(Debug, Deserialize, Default)]
pub(crate) struct BackendQuery {
    id: Option<String>,
}

pub async fn backend_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<BackendQuery>,
) -> Response {
    let provider = query
        .id
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    open_provider(ws, state, DEFAULT_STATION_ID.to_string(), provider)
}

pub fn open_provider(
    ws: WebSocketUpgrade,
    state: AppState,
    charger_id: String,
    provider_id: String,
) -> Response {
    let record = match state.store().get_charger(&charger_id) {
        Ok(Some(row)) if !row.enabled => return StatusCode::FORBIDDEN.into_response(),
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let station = state.attach(&record);
    ws.on_upgrade(move |socket| run_backend(socket, state, station, provider_id))
        .into_response()
}

async fn run_backend(
    mut socket: WebSocket,
    state: AppState,
    station: StationHandle,
    id: String,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    station.backends().insert(id.clone(), tx);
    station
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .subscribe(&id);
    tracing::info!(backend = %id, "backend connected");
    loop {
        tokio::select! {
            _ = station.cancel().cancelled() => break,
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let reply = on_backend_text(&state, &station, &id, &text).await;
                        if socket.send(Message::Text(reply.into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
            event = rx.recv() => {
                match event {
                    Some(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
    station.backends().remove(&id);
    station
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .unsubscribe(&id);
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 1001,
            reason: "".into(),
        })))
        .await;
    tracing::info!(backend = %id, "backend disconnected");
}

async fn on_backend_text(
    state: &AppState,
    station: &StationHandle,
    id: &str,
    text: &str,
) -> String {
    let Ok(body) = serde_json::from_str::<Value>(text) else {
        return json!({"error": "unknown_action"}).to_string();
    };
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    let Some(charger) = station.charger_link() else {
        return json!({"error": "unknown_action"}).to_string();
    };
    match action {
        "RemoteStartTransaction" => {
            let decision = remote_start(
                station.arbiter(),
                state,
                &charger,
                state.clock(),
                &RemoteStart {
                    version: charger.version,
                    backend_id: id.to_string(),
                    connector_id: body
                        .get("connector_id")
                        .and_then(Value::as_i64)
                        .unwrap_or(1),
                    id_tag: body
                        .get("id_tag")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                },
            )
            .await;
            start_reply(decision)
        }
        "RemoteStopTransaction" => {
            let decision = remote_stop(
                station.arbiter(),
                state,
                &charger,
                state.clock(),
                &RemoteStop {
                    version: charger.version,
                    backend_id: id.to_string(),
                    transaction_id: json_tx(&body),
                    acquire_lock: false,
                },
            )
            .await;
            stop_reply(decision)
        }
        _ => json!({"error": "unknown_action"}).to_string(),
    }
}

fn start_reply(decision: StartDecision) -> String {
    match decision {
        StartDecision::Locked => json!({"error": "control_locked"}).to_string(),
        StartDecision::Accepted => {
            json!({"action": "RemoteStartTransaction", "result": {"status": "Accepted"}})
                .to_string()
        }
        StartDecision::Rejected => {
            json!({"action": "RemoteStartTransaction", "result": {"status": "Rejected"}})
                .to_string()
        }
    }
}

fn stop_reply(decision: StopDecision) -> String {
    let status = match decision {
        StopDecision::Accepted => "Accepted",
        StopDecision::Rejected => "Rejected",
    };
    json!({"action": "RemoteStopTransaction", "result": {"status": status}}).to_string()
}

fn json_tx(body: &Value) -> String {
    match body.get("transaction_id") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}
