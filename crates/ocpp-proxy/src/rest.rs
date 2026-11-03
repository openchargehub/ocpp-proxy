use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ocpp_proxy_application::ChargerRegistry;
use ocpp_proxy_domain::DEFAULT_STATION_ID;
use serde_json::{json, Value};

use crate::state::AppState;

pub async fn sessions_json(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match state.store().sessions_for(&requested_charger(&query)) {
        Ok(rows) => {
            let body: Vec<Value> = rows.into_iter().map(session_json).collect();
            Json(body).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    }
}

pub async fn sessions_csv(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let rows = match state.store().sessions_for(&requested_charger(&query)) {
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

pub async fn status(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let id = requested_charger(&query);
    let record = match state.store().get_charger(&id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Json(json!({
                "websocket_backends": [],
                "lock_owner": null,
                "ocpp_services": {}
            }))
            .into_response()
        }
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let station = state.attach(&record);
    let guard = station
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let body = json!({
        "websocket_backends": guard.websocket_backends(),
        "lock_owner": guard.lock_owner(),
        "ocpp_services": station.upstream().snapshot(),
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
    let charger_id = match value.get("charger_id") {
        None => DEFAULT_STATION_ID.to_string(),
        Some(Value::String(id)) if id.is_empty() => DEFAULT_STATION_ID.to_string(),
        Some(Value::String(id)) => id.clone(),
        Some(_) => return (StatusCode::BAD_REQUEST, "charger_id must be a string").into_response(),
    };
    let record = match state.store().get_charger(&charger_id) {
        Ok(Some(row)) => row,
        Ok(None) => return Json(json!({"success": false, "owner": null})).into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let station = state.attach(&record);
    station
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .release();
    let granted = ocpp_proxy_application::acquire_control(
        station.arbiter(),
        &state,
        state.clock(),
        backend_id,
    )
    .await;
    let owner = station
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
        "charger_id": row.charger_id,
    })
}

fn requested_charger(query: &HashMap<String, String>) -> String {
    query
        .get("charger_id")
        .filter(|id| !id.is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_STATION_ID.to_string())
}
