//! Business rules for the OCPP proxy. No sockets, no database, no Home Assistant client.

mod arbiter;
mod config;
mod path;
mod session;
mod version;

pub use arbiter::{Arbiter, Probe, SafetyInput, SharingPolicy};
pub use config::{ConfigError, OcppServiceConfig, ProxyConfig};
pub use path::{
    parse_station_path, validate_charger_id, version_hint_path, PathError, StationRoute,
    DEFAULT_STATION_ID,
};
pub use session::{SessionLedger, SessionRow, UsageRecord};
pub use version::{
    detect_version, is_version_supported, resolve_version, supported_versions, ConnectionHints,
    OcppVersion, VersionError,
};
