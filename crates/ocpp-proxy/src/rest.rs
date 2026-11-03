use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ocpp_proxy_application::SessionStore;
use serde_json::{json, Value};

use crate::html::WELCOME_HTML;
use crate::state::AppState;

pub async fn welcome() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        WELCOME_HTML,
    )
        .into_response()
}

pub async fn sessions_json(State(state): State<AppState>) -> Response {
    match state.store().list() {
        Ok(rows) => {
            let body: Vec<Value> = rows.into_iter().map(session_json).collect();
            Json(body).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    }
}

pub async fn sessions_csv(State(state): State<AppState>) -> Response {
    let rows = match state.store().list() {
        Ok(rows) => rows,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let mut buffer = Vec::new();
    {
        let mut writer = csv::Writer::from_writer(&mut buffer);
        if writer
            .write_record([
                "timestamp",
                "backend_id",
                "duration_s",
                "energy_kwh",
                "revenue",
            ])
            .is_err()
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to write csv header",
            )
                .into_response();
        }
        for row in rows {
            let record = [
                row.timestamp,
                row.backend_id,
                row.duration_s.to_string(),
                row.energy_kwh.to_string(),
                row.revenue.to_string(),
            ];
            if writer.write_record(record).is_err() {
                return (StatusCode::INTERNAL_SERVER_ERROR, "failed to write csv row")
                    .into_response();
            }
        }
        if writer.flush().is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "failed to finish csv").into_response();
        }
    }
    ([(header::CONTENT_TYPE, "text/csv; charset=utf-8")], buffer).into_response()
}

pub async fn status(State(state): State<AppState>) -> Response {
    let guard = state
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let body = json!({
        "websocket_backends": guard.websocket_backends(),
        "lock_owner": guard.lock_owner(),
        "ocpp_services": state.upstream().snapshot(),
    });
    Json(body).into_response()
}

pub async fn override_control(State(state): State<AppState>, body: Bytes) -> Response {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid JSON").into_response(),
    };
    let Some(backend_id) = value.get("backend_id").and_then(Value::as_str) else {
        return (StatusCode::BAD_REQUEST, "backend_id must be a string").into_response();
    };
    state
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .release();
    let granted =
        ocpp_proxy_application::acquire_control(state.arbiter(), &state, state.clock(), backend_id)
            .await;
    let owner = state
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .lock_owner()
        .map(str::to_string);
    Json(json!({"success": granted, "owner": owner})).into_response()
}

fn session_json(row: ocpp_proxy_domain::SessionRow) -> Value {
    json!({
        "timestamp": row.timestamp,
        "backend_id": row.backend_id,
        "duration_s": row.duration_s,
        "energy_kwh": row.energy_kwh,
        "revenue": row.revenue,
    })
}
