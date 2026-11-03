/// Values copied from YAML the first time a charger id appears.
#[derive(Debug, Clone, PartialEq)]
pub struct ChargerDefaults {
    pub ocpp_version: String,
    pub auto_detect: bool,
    pub allow_shared_charging: bool,
    pub preferred_provider: String,
    pub blocked_providers: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub presence_sensor: String,
    pub override_input_boolean: String,
    pub rate_limit_seconds: i64,
}

/// One charger row. Secrets for outbound providers live on [`ServiceRecord`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChargerRecord {
    pub id: String,
    pub enabled: bool,
    pub ocpp_version: String,
    pub auto_detect: bool,
    pub allow_shared_charging: bool,
    pub preferred_provider: String,
    pub blocked_providers: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub presence_sensor: String,
    pub override_input_boolean: String,
    pub rate_limit_seconds: i64,
    pub last_seen: String,
}

/// Outbound OCPP service stored for one charger.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRecord {
    pub charger_id: String,
    pub id: String,
    pub url: String,
    pub version: String,
    pub auth_type: String,
    pub username: String,
    pub password: String,
    pub token: String,
    pub enabled: bool,
}

/// First connect inserts. A later connect leaves the saved row alone.
#[derive(Debug, Clone, PartialEq)]
pub enum EnrollOutcome {
    Created(ChargerRecord),
    Existing(ChargerRecord),
}

pub trait ChargerRegistry: Send + Sync {
    fn list_chargers(&self) -> Result<Vec<ChargerRecord>, String>;
    fn get_charger(&self, id: &str) -> Result<Option<ChargerRecord>, String>;
    fn enroll(&self, id: &str, defaults: &ChargerDefaults) -> Result<EnrollOutcome, String>;
    fn update_charger(&self, record: &ChargerRecord) -> Result<(), String>;
    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String>;
    fn delete_charger(&self, id: &str) -> Result<(), String>;
    fn list_services(&self, charger_id: &str) -> Result<Vec<ServiceRecord>, String>;
    fn save_service(&self, service: &ServiceRecord) -> Result<(), String>;
    fn delete_service(&self, charger_id: &str, service_id: &str) -> Result<(), String>;
    fn touch_last_seen(&self, id: &str, seen: &str) -> Result<(), String>;
}
