use async_trait::async_trait;
use ocpp_proxy_domain::{OcppVersion, SessionRow};

use crate::events::ChargerEvent;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    fn now_iso(&self) -> String;
}

#[async_trait]
pub trait EntityState: Send + Sync {
    async fn state_of(&self, entity_id: &str) -> Result<String, String>;
}

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, title: &str, message: &str) -> Result<(), String>;
}

#[async_trait]
pub trait ChargerCommand: Send + Sync {
    async fn remote_start(
        &self,
        version: OcppVersion,
        connector_id: i64,
        id_tag: &str,
    ) -> Result<bool, String>;

    async fn remote_stop(&self, version: OcppVersion, transaction_id: &str)
        -> Result<bool, String>;
}

pub trait SessionStore: Send + Sync {
    fn insert(&self, row: &SessionRow) -> Result<(), String>;
    fn list(&self) -> Result<Vec<SessionRow>, String>;
}

pub trait BackendHub: Send + Sync {
    fn broadcast(&self, event: &ChargerEvent);
}

pub trait UpstreamHub: Send + Sync {
    fn forward(&self, event: &ChargerEvent);
}
