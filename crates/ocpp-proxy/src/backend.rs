use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ocpp_proxy_application::{
    remote_start, remote_stop, ChargerRegistry, RemoteStart, RemoteStop, StartDecision,
    StopDecision,
};
use ocpp_proxy_domain::DEFAULT_STATION_ID;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::admin::parse_form;
use crate::state::{AppState, StationHandle};

pub async fn backend_ws(State(state): State<AppState>, request: Request) -> Response {
    let provider = provider_name(request.uri().query().unwrap_or(""));
    accept_provider(request, state, DEFAULT_STATION_ID, provider).await
}

pub async fn accept_provider(
    request: Request,
    state: AppState,
    charger_id: &str,
    provider_id: String,
) -> Response {
    let station = match gate_provider(&state, charger_id) {
        Ok(station) => station,
        Err(response) => return *response,
    };
    accept_socket(request, move |socket| {
        run_backend(socket, state, station, provider_id)
    })
    .await
}

pub fn gate_provider(state: &AppState, charger_id: &str) -> Result<StationHandle, Box<Response>> {
    match state.store().get_charger(charger_id) {
        Ok(Some(row)) if !row.enabled => Err(Box::new(StatusCode::FORBIDDEN.into_response())),
        Ok(Some(row)) => Ok(state.attach(&row)),
        Ok(None) => Err(Box::new(StatusCode::NOT_FOUND.into_response())),
        Err(err) => Err(Box::new(
            (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
        )),
    }
}

pub async fn extract_upgrade(
    request: Request,
) -> Result<(WebSocketUpgrade, HeaderMap), Box<Response>> {
    let (mut parts, _) = request.into_parts();
    let headers = parts.headers.clone();
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => Ok((upgrade, headers)),
        Err(rejection) => Err(Box::new(rejection.into_response())),
    }
}

async fn accept_socket<F, Fut>(request: Request, callback: F) -> Response
where
    F: FnOnce(WebSocket) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    match extract_upgrade(request).await {
        Ok((upgrade, _)) => upgrade.on_upgrade(callback),
        Err(response) => *response,
    }
}

fn provider_name(query: &str) -> String {
    parse_form(query.as_bytes())
        .get("id")
        .filter(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string())
}

async fn run_backend(mut socket: WebSocket, state: AppState, station: StationHandle, id: String) {
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
