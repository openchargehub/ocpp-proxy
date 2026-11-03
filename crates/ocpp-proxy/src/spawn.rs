use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::routing::{get, post};
use axum::Router;
use ocpp_proxy_application::Clock;
use ocpp_proxy_domain::{ProxyConfig, SharingPolicy};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::backend::backend_ws;
use crate::charger::charger_ws;
use crate::ha::HaClient;
use crate::rest::{override_control, sessions_csv, sessions_json, status, welcome};
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
        Ok(Self {
            config,
            db_path: PathBuf::from(db_path),
            bind: SocketAddr::from(([0, 0, 0, 0], port)),
            lock_timeout: Duration::from_secs(60),
            ha_url,
            ha_token,
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
    let store = SqliteStore::open(&options.db_path)?;
    let ha = match (&options.ha_url, &options.ha_token) {
        (Some(url), Some(token)) => Some(HaClient::new(url, token)),
        _ => None,
    };
    let cancel = CancellationToken::new();
    let state = AppState::new(options.config, policy, store, ha, cancel.clone());
    spawn_services(&state);
    let tick_cancel = cancel.clone();
    let tick_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tick_cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    let now = tick_state.clock().now_ms();
                    tick_state.arbiter().lock().unwrap_or_else(|err| err.into_inner()).poll_timeout(now);
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
        .route("/", get(welcome))
        .route("/charger", get(charger_ws))
        .route("/charger/{*rest}", get(charger_ws))
        .route("/backend", get(backend_ws))
        .route("/sessions", get(sessions_json))
        .route("/sessions.csv", get(sessions_csv))
        .route("/status", get(status))
        .route("/override", post(override_control))
        .with_state(state)
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
