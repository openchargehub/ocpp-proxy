use serde_json::{json, Value};

/// Charger event delivered to WebSocket backends. `None` fields are omitted.
#[derive(Debug, Clone, PartialEq)]
pub enum ChargerEvent {
    Boot {
        vendor: String,
        model: String,
        reason: Option<String>,
    },
    Heartbeat {
        current_time: String,
    },
    Status {
        connector_id: i64,
        evse_id: Option<i64>,
        error_code: Option<String>,
        status: String,
        timestamp: Option<String>,
    },
    Meter {
        connector_id: Option<i64>,
        evse_id: Option<i64>,
        values: Value,
    },
    TransactionStarted {
        transaction_id: Value,
        connector_id: i64,
        id_tag: String,
        meter_start: i64,
        timestamp: String,
    },
    TransactionStopped {
        transaction_id: Value,
        meter_stop: i64,
        timestamp: String,
    },
}

impl ChargerEvent {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Boot {
                vendor,
                model,
                reason,
            } => with_optional(
                json!({"type": "boot", "vendor": vendor, "model": model}),
                "reason",
                reason.as_ref(),
            ),
            Self::Heartbeat { current_time } => {
                json!({"type": "heartbeat", "current_time": current_time})
            }
            Self::Status {
                connector_id,
                evse_id,
                error_code,
                status,
                timestamp,
            } => {
                let mut body =
                    json!({"type": "status", "connector_id": connector_id, "status": status});
                insert_i64(&mut body, "evse_id", *evse_id);
                insert_str(&mut body, "error_code", error_code.as_deref());
                insert_str(&mut body, "timestamp", timestamp.as_deref());
                body
            }
            Self::Meter {
                connector_id,
                evse_id,
                values,
            } => {
                let mut body = json!({"type": "meter", "values": values});
                insert_i64(&mut body, "connector_id", *connector_id);
                insert_i64(&mut body, "evse_id", *evse_id);
                body
            }
            Self::TransactionStarted {
                transaction_id,
                connector_id,
                id_tag,
                meter_start,
                timestamp,
            } => json!({
                "type": "transaction_started",
                "transaction_id": transaction_id,
                "connector_id": connector_id,
                "id_tag": id_tag,
                "meter_start": meter_start,
                "timestamp": timestamp,
            }),
            Self::TransactionStopped {
                transaction_id,
                meter_stop,
                timestamp,
            } => json!({
                "type": "transaction_stopped",
                "transaction_id": transaction_id,
                "meter_stop": meter_stop,
                "timestamp": timestamp,
            }),
        }
    }

    pub fn forwards_upstream(&self) -> bool {
        !matches!(self, Self::Boot { .. })
    }
}

fn with_optional(mut body: Value, key: &str, value: Option<&String>) -> Value {
    if let Some(value) = value {
        body[key] = json!(value);
    }
    body
}

fn insert_i64(body: &mut Value, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        body[key] = json!(value);
    }
}

fn insert_str(body: &mut Value, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        body[key] = json!(value);
    }
}
