use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::routing::{get, post};
use axum::Router;
use ocpp_proxy_application::{
    ChargerDefaults, ChargerRegistry, Clock, EnrollOutcome, ServiceRecord,
};
use ocpp_proxy_domain::{ProxyConfig, SharingPolicy, DEFAULT_STATION_ID};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::admin::{
    add_charger, charger_detail, charger_list, delete_charger, delete_service_route,
    disable_charger, enable_charger, save_policy, save_service_route,
};
use crate::backend::backend_ws;
use crate::charger::charger_ws;
use crate::ha::HaClient;
use crate::rest::{override_control, sessions_csv, sessions_json, status};
use crate::state::AppState;
use crate::store::SqliteStore;
use crate::upstream::spawn_services;

pub struct ProxyOptions {
    pub config: ProxyConfig,
    pub db_path: PathBuf,
    pub bind: SocketAddr,
    pub lock_timeout: Duration,
    pub ha_url: Option<String>,
    pub ha_token: Option<String>,
    pub admin_token: Option<String>,
}

impl ProxyOptions {
    pub fn from_env() -> Result<Self, String> {
        let config = ProxyConfig::load_default().map_err(|err| err.to_string())?;
        let port = match std::env::var("PORT") {
            Ok(value) => value
                .parse::<u16>()
                .map_err(|_| format!("invalid PORT: {value}"))?,
            Err(_) => 9000,
        };
        let db_path = std::env::var("LOG_DB_PATH").unwrap_or_else(|_| "usage_log.db".to_string());
        let ha_url = std::env::var("HA_URL")
            .ok()
            .filter(|value| !value.is_empty());
        let ha_token = std::env::var("HA_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let (ha_url, ha_token) = match (ha_url, ha_token) {
            (Some(url), Some(token)) => (Some(url), Some(token)),
            _ => (None, None),
        };
        let admin_token = std::env::var("ADMIN_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        Ok(Self {
            config,
            db_path: PathBuf::from(db_path),
            bind: SocketAddr::from(([0, 0, 0, 0], port)),
            lock_timeout: Duration::from_secs(60),
            ha_url,
            ha_token,
            admin_token,
        })
    }
}

pub struct RunningProxy {
    pub addr: SocketAddr,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl RunningProxy {
    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

pub async fn spawn(options: ProxyOptions) -> Result<RunningProxy, String> {
    let listener = TcpListener::bind(options.bind)
        .await
        .map_err(|err| format!("failed to bind {}: {err}", options.bind))?;
    let addr = listener
        .local_addr()
        .map_err(|err| format!("failed to read bound address: {err}"))?;
    let policy = SharingPolicy::from_config(&options.config)
        .map_err(|err| err.to_string())?
        .with_lock_timeout(options.lock_timeout);
    let defaults = charger_defaults(&options.config, &policy);
    let store = SqliteStore::open(&options.db_path)?;
    let ha = match (&options.ha_url, &options.ha_token) {
        (Some(url), Some(token)) => Some(HaClient::new(url, token)),
        _ => None,
    };
    let cancel = CancellationToken::new();
    let state = AppState::new(
        options.config,
        defaults,
        store,
        ha,
        cancel.clone(),
        options.lock_timeout,
        options.admin_token,
    );
    boot_saved(&state)?;
    let tick_cancel = cancel.clone();
    let tick_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tick_cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    let now = tick_state.clock().now_ms();
                    for station in tick_state.stations() {
                        station
                            .arbiter()
                            .lock()
                            .unwrap_or_else(|err| err.into_inner())
                            .poll_timeout(now);
                    }
                }
            }
        }
    });
    let app = router(state);
    let shutdown = cancel.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await;
    });
    Ok(RunningProxy {
        addr,
        cancel,
        handle,
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(charger_list))
        .route("/chargers", get(charger_list).post(add_charger))
        .route("/chargers/{id}", get(charger_detail))
        .route("/chargers/{id}/policy", post(save_policy))
        .route("/chargers/{id}/services", post(save_service_route))
        .route(
            "/chargers/{id}/services/{service_id}/delete",
            post(delete_service_route),
        )
        .route("/chargers/{id}/disable", post(disable_charger))
        .route("/chargers/{id}/enable", post(enable_charger))
        .route("/chargers/{id}/delete", post(delete_charger))
        .route("/charger", get(charger_ws))
        .route("/charger/{*rest}", get(charger_ws))
        .route("/backend", get(backend_ws))
        .route("/sessions", get(sessions_json))
        .route("/sessions.csv", get(sessions_csv))
        .route("/status", get(status))
        .route("/override", post(override_control))
        .with_state(state)
}

fn charger_defaults(config: &ProxyConfig, policy: &SharingPolicy) -> ChargerDefaults {
    ChargerDefaults {
        ocpp_version: config.ocpp_version(),
        auto_detect: config.auto_detect_ocpp_version(),
        allow_shared_charging: policy.allow_shared_charging,
        preferred_provider: policy.preferred_provider.clone(),
        blocked_providers: policy.blocked_providers.clone(),
        allowed_providers: policy.allowed_providers.clone(),
        presence_sensor: policy.presence_sensor.clone(),
        override_input_boolean: policy.override_input_boolean.clone(),
        rate_limit_seconds: policy.rate_limit_seconds,
    }
}

fn boot_saved(state: &AppState) -> Result<(), String> {
    if let EnrollOutcome::Created(_) = state
        .store()
        .enroll(DEFAULT_STATION_ID, &state.defaults())?
    {
        copy_yaml_services(state)?;
    }
    for record in state.store().list_chargers()? {
        let enabled = record.enabled;
        let station = state.attach(&record);
        if enabled {
            spawn_services(state, &station);
        }
    }
    Ok(())
}

fn copy_yaml_services(state: &AppState) -> Result<(), String> {
    for service in state.yaml_services() {
        let version = service.version().to_string();
        state.store().save_service(&ServiceRecord {
            charger_id: DEFAULT_STATION_ID.to_string(),
            id: service.id,
            url: service.url,
            version,
            auth_type: service.auth_type.unwrap_or_default(),
            username: service.username.unwrap_or_default(),
            password: service.password.unwrap_or_default(),
            token: service.token.unwrap_or_default(),
            enabled: service.enabled,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn from_env_reads_port_database_and_home_assistant() {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous: Vec<_> = [
            "PORT",
            "LOG_DB_PATH",
            "HA_URL",
            "HA_TOKEN",
            "ADDON_CONFIG_FILE",
        ]
        .into_iter()
        .map(|key| (key, std::env::var(key).ok()))
        .collect();
        std::env::set_var("PORT", "9001");
        std::env::set_var("LOG_DB_PATH", "/tmp/ocpp-usage.db");
        std::env::set_var("HA_URL", "http://ha.local/");
        std::env::set_var("HA_TOKEN", "token");
        std::env::set_var("ADDON_CONFIG_FILE", "/nonexistent/ocpp-proxy-options.yaml");
        let options = ProxyOptions::from_env().unwrap();
        assert_eq!(options.bind.port(), 9001);
        assert_eq!(options.db_path, PathBuf::from("/tmp/ocpp-usage.db"));
        assert_eq!(options.ha_url.as_deref(), Some("http://ha.local/"));
        assert_eq!(options.ha_token.as_deref(), Some("token"));
        assert!(!options.config.allow_shared_charging());

        std::env::set_var("PORT", "nope");
        match ProxyOptions::from_env() {
            Err(err) => assert!(err.contains("nope")),
            Ok(_) => panic!("invalid PORT was accepted"),
        }
        std::env::set_var("PORT", "9000");
        std::env::remove_var("HA_TOKEN");
        let partial = ProxyOptions::from_env().unwrap();
        assert!(partial.ha_url.is_none());
        assert!(partial.ha_token.is_none());

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
