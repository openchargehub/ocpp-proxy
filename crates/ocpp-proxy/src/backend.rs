use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use ocpp_proxy_application::{
    remote_start, remote_stop, RemoteStart, RemoteStop, StartDecision, StopDecision,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub(crate) struct BackendQuery {
    id: Option<String>,
}

pub async fn backend_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<BackendQuery>,
) -> impl IntoResponse {
    let id = query
        .id
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    ws.on_upgrade(move |socket| run_backend(socket, state, id))
}

async fn run_backend(mut socket: WebSocket, state: AppState, id: String) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    state.backends().insert(id.clone(), tx);
    state
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .subscribe(&id);
    tracing::info!(backend = %id, "backend connected");
    loop {
        tokio::select! {
            _ = state.root_cancel().cancelled() => break,
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let reply = on_backend_text(&state, &id, &text).await;
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
    state.backends().remove(&id);
    state
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

async fn on_backend_text(state: &AppState, id: &str, text: &str) -> String {
    let Ok(body) = serde_json::from_str::<Value>(text) else {
        return json!({"error": "unknown_action"}).to_string();
    };
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    let Some(charger) = state.charger_link() else {
        return json!({"error": "unknown_action"}).to_string();
    };
    match action {
        "RemoteStartTransaction" => {
            let decision = remote_start(
                state.arbiter(),
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
                state.arbiter(),
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
