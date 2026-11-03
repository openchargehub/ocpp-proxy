pub fn is_fault_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "faulted" | "unavailable"
    )
}

pub fn session_ended_message(backend_id: &str, energy_kwh: f64, duration_s: f64) -> String {
    format!("Provider={backend_id}, kWh={energy_kwh:.2}, duration={duration_s:.0}s")
}

pub fn fault_message_v16(status: &str, error_code: &str) -> String {
    format!("Status={status}, Error={error_code}")
}

pub fn fault_message_v201(status: &str, evse_id: i64, connector_id: i64) -> String {
    format!("Status={status}, EVSE={evse_id}, Connector={connector_id}")
}
