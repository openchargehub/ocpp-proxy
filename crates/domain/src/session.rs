use std::collections::HashMap;

use chrono::DateTime;

#[derive(Debug, Clone, PartialEq)]
struct OpenSession {
    start_time: String,
    start_meter: i64,
}

/// Row written to the sessions table.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub timestamp: String,
    pub backend_id: String,
    pub duration_s: f64,
    pub energy_kwh: f64,
    pub revenue: f64,
    pub charger_id: String,
}

/// Computed when a transaction ends. Revenue stays zero.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    pub backend_id: String,
    pub duration_s: f64,
    pub energy_kwh: f64,
    pub revenue: f64,
}

impl UsageRecord {
    pub fn to_row(&self, timestamp: impl Into<String>) -> SessionRow {
        SessionRow {
            timestamp: timestamp.into(),
            backend_id: self.backend_id.clone(),
            duration_s: self.duration_s,
            energy_kwh: self.energy_kwh,
            revenue: self.revenue,
            charger_id: String::new(),
        }
    }
}

/// Open charging sessions. OCPP 1.6 ids are assigned here; 2.0.1 keeps the charger's id.
#[derive(Debug, Default)]
pub struct SessionLedger {
    open: HashMap<String, OpenSession>,
    next_v16: i64,
}

impl SessionLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_v16(
        &mut self,
        connector_id: i64,
        id_tag: &str,
        timestamp: &str,
        meter_start: i64,
    ) -> i64 {
        let _kept = (connector_id, id_tag);
        self.next_v16 += 1;
        let id = self.next_v16;
        self.store(id.to_string(), timestamp, meter_start);
        id
    }

    pub fn start_v201(
        &mut self,
        tx_id: &str,
        connector_id: i64,
        id_tag: &str,
        timestamp: &str,
        meter_start: i64,
    ) {
        let _kept = (connector_id, id_tag);
        self.store(tx_id.to_string(), timestamp, meter_start);
    }

    pub fn stop(
        &mut self,
        tx_id: &str,
        meter_stop: i64,
        timestamp: &str,
        backend_id: Option<&str>,
    ) -> Option<UsageRecord> {
        let info = self.open.remove(tx_id)?;
        Some(UsageRecord {
            backend_id: backend_id.unwrap_or("").to_string(),
            duration_s: duration_between(&info.start_time, timestamp),
            energy_kwh: energy_kwh(info.start_meter, meter_stop),
            revenue: 0.0,
        })
    }

    fn store(&mut self, tx_id: String, timestamp: &str, meter_start: i64) {
        self.open.insert(
            tx_id,
            OpenSession {
                start_time: timestamp.to_string(),
                start_meter: meter_start,
            },
        );
    }
}

fn energy_kwh(meter_start: i64, meter_stop: i64) -> f64 {
    (meter_stop - meter_start) as f64 / 1000.0
}

fn duration_between(start: &str, end: &str) -> f64 {
    let Ok(t0) = DateTime::parse_from_rfc3339(start) else {
        return 0.0;
    };
    let Ok(t1) = DateTime::parse_from_rfc3339(end) else {
        return 0.0;
    };
    (t1 - t0).num_milliseconds() as f64 / 1000.0
}
