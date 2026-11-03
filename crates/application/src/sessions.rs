use std::sync::Mutex;

use ocpp_proxy_domain::{Arbiter, SessionLedger, SessionRow};
use serde_json::Value;

use crate::events::ChargerEvent;
use crate::messages::{is_fault_status, session_ended_message};
use crate::ports::{BackendHub, Notifier, SessionStore, UpstreamHub};

pub struct AcceptedCall {
    pub payload_status: &'static str,
    pub interval: Option<u32>,
    pub current_time: Option<String>,
    pub transaction_id: Option<i64>,
}

pub fn on_boot(
    vendor: &str,
    model: &str,
    reason: Option<String>,
    current_time: &str,
    backends: &dyn BackendHub,
    upstream: &dyn UpstreamHub,
) -> AcceptedCall {
    let event = ChargerEvent::Boot {
        vendor: vendor.to_string(),
        model: model.to_string(),
        reason,
    };
    // The proxy already booted toward upstream as the charge point.
    let _ = upstream;
    debug_assert!(!event.forwards_upstream());
    backends.broadcast(&event);
    accepted(current_time, Some(10), None)
}

pub fn on_heartbeat(
    current_time: &str,
    backends: &dyn BackendHub,
    upstream: &dyn UpstreamHub,
) -> AcceptedCall {
    let event = ChargerEvent::Heartbeat {
        current_time: current_time.to_string(),
    };
    emit(&event, backends, upstream);
    accepted(current_time, None, None)
}

pub fn on_status(
    event: ChargerEvent,
    backends: &dyn BackendHub,
    upstream: &dyn UpstreamHub,
) -> &'static str {
    emit(&event, backends, upstream);
    "Accepted"
}

pub async fn note_fault(
    arbiter: &Mutex<Arbiter>,
    notifier: &dyn Notifier,
    status: &str,
    message: &str,
) -> bool {
    if !is_fault_status(status) {
        return false;
    }
    arbiter
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .release();
    let _ = notifier.notify("Charger Fault", message).await;
    true
}

pub fn on_meter(event: ChargerEvent, backends: &dyn BackendHub, upstream: &dyn UpstreamHub) {
    emit(&event, backends, upstream);
}

pub struct StartInput<'a> {
    pub connector_id: i64,
    pub id_tag: &'a str,
    pub meter_start: i64,
    pub timestamp: &'a str,
}

pub struct StopInput<'a> {
    pub tx_id: &'a str,
    pub meter_stop: i64,
    pub timestamp: &'a str,
    pub transaction_id: Value,
    pub owner: Option<&'a str>,
    pub logged_at: &'a str,
}

pub fn on_start(
    ledger: &mut SessionLedger,
    input: StartInput<'_>,
    backends: &dyn BackendHub,
    upstream: &dyn UpstreamHub,
) -> i64 {
    let tx_id = ledger.start_v16(
        input.connector_id,
        input.id_tag,
        input.timestamp,
        input.meter_start,
    );
    emit(
        &started_event(serde_json::json!(tx_id), &input),
        backends,
        upstream,
    );
    tx_id
}

pub fn on_start_v201(
    ledger: &mut SessionLedger,
    tx_id: &str,
    input: StartInput<'_>,
    backends: &dyn BackendHub,
    upstream: &dyn UpstreamHub,
) {
    ledger.start_v201(
        tx_id,
        input.connector_id,
        input.id_tag,
        input.timestamp,
        input.meter_start,
    );
    emit(
        &started_event(serde_json::json!(tx_id), &input),
        backends,
        upstream,
    );
}

pub fn on_transaction_stop(
    ledger: &mut SessionLedger,
    store: &dyn SessionStore,
    input: StopInput<'_>,
    backends: &dyn BackendHub,
    upstream: &dyn UpstreamHub,
) -> Option<String> {
    let event = ChargerEvent::TransactionStopped {
        transaction_id: input.transaction_id,
        meter_stop: input.meter_stop,
        timestamp: input.timestamp.to_string(),
    };
    emit(&event, backends, upstream);
    let usage = ledger.stop(input.tx_id, input.meter_stop, input.timestamp, input.owner)?;
    let row = usage.to_row(input.logged_at);
    store.insert(&row).ok()?;
    Some(session_ended_message(
        &usage.backend_id,
        usage.energy_kwh,
        usage.duration_s,
    ))
}

fn started_event(transaction_id: Value, input: &StartInput<'_>) -> ChargerEvent {
    ChargerEvent::TransactionStarted {
        transaction_id,
        connector_id: input.connector_id,
        id_tag: input.id_tag.to_string(),
        meter_start: input.meter_start,
        timestamp: input.timestamp.to_string(),
    }
}

pub async fn notify_session(notifier: &dyn Notifier, message: &str) {
    let _ = notifier.notify("Charging session ended", message).await;
}

pub fn list_sessions(store: &dyn SessionStore) -> Result<Vec<SessionRow>, String> {
    store.list()
}

fn emit(event: &ChargerEvent, backends: &dyn BackendHub, upstream: &dyn UpstreamHub) {
    backends.broadcast(event);
    if event.forwards_upstream() {
        upstream.forward(event);
    }
}

fn accepted(
    current_time: &str,
    interval: Option<u32>,
    transaction_id: Option<i64>,
) -> AcceptedCall {
    AcceptedCall {
        payload_status: "Accepted",
        interval,
        current_time: Some(current_time.to_string()),
        transaction_id,
    }
}
