use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ocpp_proxy_application::{ChargerRegistry, EnrollOutcome, ServiceRecord};
use ocpp_proxy_domain::validate_charger_id;

use crate::html::{charger_detail_page, charger_list_page, ListedCharger};
use crate::state::{policy_from, AppState};
use crate::upstream::spawn_services;

pub async fn charger_list(State(state): State<AppState>) -> Response {
    match listed(&state) {
        Ok(rows) => html(charger_list_page(&rows)),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    }
}

pub async fn charger_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let token = presented_token(&headers, None, query.get("token").map(String::as_str));
    if !state.authorize(token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    render_detail(&state, &id, token.as_deref().unwrap_or(""))
}

pub async fn add_charger(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = parse_form(&body);
    let token = presented_token(&headers, form.get("token").map(String::as_str), None);
    if !state.authorize(token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let id = form.get("id").cloned().unwrap_or_default();
    if let Err(err) = validate_charger_id(&id) {
        return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
    }
    match state.store().enroll(&id, &state.defaults()) {
        Ok(EnrollOutcome::Created(row) | EnrollOutcome::Existing(row)) => {
            state.attach(&row);
            see_detail(&id, token.as_deref().unwrap_or(""))
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    }
}

pub async fn save_policy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = parse_form(&body);
    let Some(token) = authorized(&state, &headers, &form) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut record = match state.store().get_charger(&id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let version = form
        .get("ocpp_version")
        .cloned()
        .unwrap_or(record.ocpp_version.clone());
    if version != "1.6" && version != "2.0.1" {
        return (
            StatusCode::BAD_REQUEST,
            format!("unsupported OCPP version: {version}"),
        )
            .into_response();
    }
    let rate = match form
        .get("rate_limit_seconds")
        .map(String::as_str)
        .unwrap_or("10")
        .parse::<i64>()
    {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "rate_limit_seconds must be an integer",
            )
                .into_response()
        }
    };
    record.ocpp_version = version;
    record.auto_detect = form.contains_key("auto_detect");
    record.allow_shared_charging = form.contains_key("allow_shared_charging");
    record.preferred_provider = form.get("preferred_provider").cloned().unwrap_or_default();
    record.blocked_providers = split_csv(form.get("blocked_providers").map(String::as_str));
    record.allowed_providers = split_csv(form.get("allowed_providers").map(String::as_str));
    record.presence_sensor = form.get("presence_sensor").cloned().unwrap_or_default();
    record.override_input_boolean = form
        .get("override_input_boolean")
        .cloned()
        .unwrap_or_default();
    record.rate_limit_seconds = rate;
    if let Err(err) = state.store().update_charger(&record) {
        return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response();
    }
    if let Some(station) = state.station(&id) {
        station.apply_policy(policy_from(&record, state.lock_timeout()));
    }
    see_detail(&id, &token)
}

pub async fn save_service_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = parse_form(&body);
    let Some(token) = authorized(&state, &headers, &form) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let service_id = form.get("id").cloned().unwrap_or_default();
    if let Err(err) = validate_charger_id(&service_id) {
        return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
    }
    if let Err(err) = state.store().save_service(&ServiceRecord {
        charger_id: id.clone(),
        id: service_id,
        url: form.get("url").cloned().unwrap_or_default(),
        version: form
            .get("version")
            .cloned()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "1.6".to_string()),
        auth_type: form.get("auth_type").cloned().unwrap_or_default(),
        username: form.get("username").cloned().unwrap_or_default(),
        password: form.get("password").cloned().unwrap_or_default(),
        token: form.get("auth_token").cloned().unwrap_or_default(),
        enabled: form.contains_key("enabled"),
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response();
    }
    if let Err(response) = restart_if_enabled(&state, &id) {
        return *response;
    }
    see_detail(&id, &token)
}

pub async fn delete_service_route(
    State(state): State<AppState>,
    Path((id, service_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = parse_form(&body);
    let Some(token) = authorized(&state, &headers, &form) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Err(err) = state.store().delete_service(&id, &service_id) {
        return (StatusCode::BAD_REQUEST, err).into_response();
    }
    if let Err(response) = restart_if_enabled(&state, &id) {
        return *response;
    }
    see_detail(&id, &token)
}

pub async fn disable_charger(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    set_enabled(state, id, headers, body, false).await
}

pub async fn enable_charger(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    set_enabled(state, id, headers, body, true).await
}

pub async fn delete_charger(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = parse_form(&body);
    if authorized(&state, &headers, &form).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if let Err(err) = state.store().delete_charger(&id) {
        return (StatusCode::NOT_FOUND, err).into_response();
    }
    if let Some(station) = state.remove_station(&id) {
        station.shutdown();
    }
    see_other("/")
}

async fn set_enabled(
    state: AppState,
    id: String,
    headers: HeaderMap,
    body: Bytes,
    enabled: bool,
) -> Response {
    let form = parse_form(&body);
    let Some(token) = authorized(&state, &headers, &form) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Err(err) = state.store().set_enabled(&id, enabled) {
        return (StatusCode::NOT_FOUND, err).into_response();
    }
    if enabled {
        if let Err(response) = restart_if_enabled(&state, &id) {
            return *response;
        }
    } else if let Some(station) = state.station(&id) {
        station.stop_services();
    }
    see_detail(&id, &token)
}

fn restart_if_enabled(state: &AppState, id: &str) -> Result<(), Box<Response>> {
    let record = match state.store().get_charger(id) {
        Ok(Some(row)) => row,
        Ok(None) => return Err(Box::new(StatusCode::NOT_FOUND.into_response())),
        Err(err) => {
            return Err(Box::new(
                (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
            ))
        }
    };
    if !record.enabled {
        return Ok(());
    }
    let station = state.attach(&record);
    spawn_services(state, &station);
    Ok(())
}

fn render_detail(state: &AppState, id: &str, token: &str) -> Response {
    let record = match state.store().get_charger(id) {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let services = match state.store().list_services(id) {
        Ok(rows) => rows,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let sessions = match state.store().sessions_for(id) {
        Ok(rows) => rows,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    };
    let station = state.station(id);
    html(charger_detail_page(
        &record,
        station.as_ref(),
        &services,
        &sessions,
        token,
    ))
}

fn listed(state: &AppState) -> Result<Vec<ListedCharger>, String> {
    let mut rows = Vec::new();
    for record in state.store().list_chargers()? {
        let station = state.station(&record.id);
        let (connected, version, lock_owner, inbound) = match &station {
            Some(station) => {
                let owner = station
                    .arbiter()
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .lock_owner()
                    .unwrap_or("")
                    .to_string();
                (
                    station.connected(),
                    station.version_label(&record.ocpp_version),
                    owner,
                    station.backends().ids().len(),
                )
            }
            None => (false, record.ocpp_version.clone(), String::new(), 0),
        };
        rows.push(ListedCharger {
            id: record.id.clone(),
            connected,
            version,
            lock_owner,
            inbound,
            outbound: state.store().list_services(&record.id)?.len(),
            last_seen: record.last_seen,
        });
    }
    Ok(rows)
}

fn authorized(
    state: &AppState,
    headers: &HeaderMap,
    form: &HashMap<String, String>,
) -> Option<String> {
    let token = presented_token(headers, form.get("token").map(String::as_str), None)?;
    if state.authorize(Some(&token)) {
        Some(token)
    } else {
        None
    }
}

fn presented_token(headers: &HeaderMap, form: Option<&str>, query: Option<&str>) -> Option<String> {
    bearer(headers)
        .or_else(|| header_value(headers, "x-admin-token"))
        .or_else(|| form.filter(|value| !value.is_empty()).map(str::to_string))
        .or_else(|| query.filter(|value| !value.is_empty()).map(str::to_string))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn see_detail(id: &str, token: &str) -> Response {
    see_other(&format!(
        "/chargers/{}?token={}",
        url_escape(id),
        url_escape(token)
    ))
}

fn see_other(location: &str) -> Response {
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}

fn html(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

fn split_csv(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn parse_form(body: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    let mut map = HashMap::new();
    for pair in text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(decode_component(key), decode_component(value));
    }
    map
}

fn decode_component(text: &str) -> String {
    let text = text.replace('+', " ");
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &text[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_escape(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
