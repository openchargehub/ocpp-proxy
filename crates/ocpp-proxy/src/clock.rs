use chrono::{SecondsFormat, Utc};
use ocpp_proxy_application::Clock;

#[derive(Clone, Copy, Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        Utc::now().timestamp_millis()
    }

    fn now_iso(&self) -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
    }
}
