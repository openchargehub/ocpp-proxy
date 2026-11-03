use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use ocpp_proxy_application::{
    BackendHub, ChargerDefaults, ChargerEvent, ChargerRecord, Clock, UpstreamHub,
};
use ocpp_proxy_domain::{
    Arbiter, OcppServiceConfig, OcppVersion, ProxyConfig, SessionLedger, SharingPolicy,
};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::clock::SystemClock;
use crate::ha::HaClient;
use crate::peer::Outbound;
use crate::store::SqliteStore;

#[derive(Clone)]
pub struct ChargerLink {
    pub version: OcppVersion,
    pub outbound: mpsc::UnboundedSender<Outbound>,
}

struct ActiveCharger {
    link: ChargerLink,
    cancel: CancellationToken,
    generation: u64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ServiceStatus {
    pub connected: bool,
    pub authenticated: bool,
    pub version: String,
}

type BackendSender = mpsc::UnboundedSender<String>;

#[derive(Clone)]
pub struct BackendSockets {
    senders: std::sync::Arc<Mutex<Vec<(String, BackendSender)>>>,
}

impl BackendSockets {
    fn new() -> Self {
        Self {
            senders: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn insert(&self, id: String, tx: mpsc::UnboundedSender<String>) {
        let mut guard = self.senders.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(slot) = guard.iter_mut().find(|(name, _)| name == &id) {
            slot.1 = tx;
        } else {
            guard.push((id, tx));
        }
    }

    pub fn remove(&self, id: &str) {
        self.senders
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .retain(|(name, _)| name != id);
    }

    pub fn ids(&self) -> Vec<String> {
        self.senders
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn broadcast_text(&self, text: &str) {
        self.senders
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .retain(|(_, tx)| tx.send(text.to_string()).is_ok());
    }
}

impl BackendHub for BackendSockets {
    fn broadcast(&self, event: &ChargerEvent) {
        self.broadcast_text(&event.to_json().to_string());
    }
}

#[derive(Clone)]
pub struct UpstreamSet {
    events: broadcast::Sender<ChargerEvent>,
    status: std::sync::Arc<Mutex<BTreeMap<String, ServiceStatus>>>,
}

impl UpstreamSet {
    fn new() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            events,
            status: std::sync::Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChargerEvent> {
        self.events.subscribe()
    }

    pub fn set_status(&self, id: &str, connected: bool, authenticated: bool, version: &str) {
        self.status
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(
                id.to_string(),
                ServiceStatus {
                    connected,
                    authenticated,
                    version: version.to_string(),
                },
            );
    }

    pub fn remove(&self, id: &str) {
        self.status
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(id);
    }

    pub fn snapshot(&self) -> BTreeMap<String, ServiceStatus> {
        self.status
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub fn clear(&self) {
        self.status
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clear();
    }
}

impl UpstreamHub for UpstreamSet {
    fn forward(&self, event: &ChargerEvent) {
        let _ = self.events.send(event.clone());
    }
}

#[derive(Clone)]
pub struct StationHandle {
    inner: std::sync::Arc<StationInner>,
}

struct StationInner {
    id: String,
    arbiter: Mutex<Arbiter>,
    ledger: Mutex<SessionLedger>,
    backends: BackendSockets,
    upstream: UpstreamSet,
    charger: Mutex<Option<ActiveCharger>>,
    generation: Mutex<u64>,
    cancel: CancellationToken,
    services: Mutex<CancellationToken>,
    boot: Mutex<Option<(String, String)>>,
    live_version: Mutex<Option<OcppVersion>>,
}

impl StationHandle {
    fn open(record: &ChargerRecord, lock_timeout: Duration, cancel: CancellationToken) -> Self {
        let services = cancel.child_token();
        Self {
            inner: std::sync::Arc::new(StationInner {
                id: record.id.clone(),
                arbiter: Mutex::new(Arbiter::new(policy_from(record, lock_timeout))),
                ledger: Mutex::new(SessionLedger::new()),
                backends: BackendSockets::new(),
                upstream: UpstreamSet::new(),
                charger: Mutex::new(None),
                generation: Mutex::new(0),
                cancel,
                services: Mutex::new(services),
                boot: Mutex::new(None),
                live_version: Mutex::new(None),
            }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn arbiter(&self) -> &Mutex<Arbiter> {
        &self.inner.arbiter
    }

    pub fn ledger(&self) -> MutexGuard<'_, SessionLedger> {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    pub fn backends(&self) -> &BackendSockets {
        &self.inner.backends
    }

    pub fn upstream(&self) -> &UpstreamSet {
        &self.inner.upstream
    }

    pub fn cancel(&self) -> &CancellationToken {
        &self.inner.cancel
    }

    pub fn connected(&self) -> bool {
        self.charger_link().is_some()
    }

    pub fn boot(&self) -> Option<(String, String)> {
        self.inner
            .boot
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub fn set_boot(&self, vendor: &str, model: &str) {
        *self
            .inner
            .boot
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some((vendor.to_string(), model.to_string()));
    }

    pub fn set_version(&self, version: OcppVersion) {
        *self
            .inner
            .live_version
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(version);
    }

    pub fn clear_version(&self) {
        *self
            .inner
            .live_version
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = None;
    }

    pub fn version_label(&self, fallback: &str) -> String {
        self.inner
            .live_version
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .map(|version| version.as_str().to_string())
            .unwrap_or_else(|| fallback.to_string())
    }

    pub fn apply_policy(&self, policy: SharingPolicy) {
        *self
            .arbiter()
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .policy_mut() = policy;
    }

    pub fn replace_charger(&self, link: ChargerLink, cancel: CancellationToken) -> u64 {
        let mut generation = self
            .inner
            .generation
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        *generation += 1;
        let current = *generation;
        drop(generation);
        let mut slot = self
            .inner
            .charger
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(previous) = slot.take() {
            previous.cancel.cancel();
        }
        *slot = Some(ActiveCharger {
            link,
            cancel,
            generation: current,
        });
        current
    }

    pub fn charger_link(&self) -> Option<ChargerLink> {
        self.inner
            .charger
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
            .map(|active| active.link.clone())
    }

    pub fn clear_charger(&self, generation: u64) {
        let mut slot = self
            .inner
            .charger
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if slot
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            if let Some(active) = slot.take() {
                active.cancel.cancel();
            }
        }
    }

    pub fn replace_service_cancel(&self) -> CancellationToken {
        let mut slot = self
            .inner
            .services
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        slot.cancel();
        let next = self.inner.cancel.child_token();
        *slot = next.clone();
        self.upstream().clear();
        next
    }

    pub fn stop_services(&self) {
        self.inner
            .services
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .cancel();
        self.upstream().clear();
    }

    pub fn shutdown(&self) {
        self.inner.cancel.cancel();
    }
}

pub fn policy_from(record: &ChargerRecord, lock_timeout: Duration) -> SharingPolicy {
    SharingPolicy {
        allow_shared_charging: record.allow_shared_charging,
        preferred_provider: record.preferred_provider.clone(),
        blocked_providers: record.blocked_providers.clone(),
        allowed_providers: record.allowed_providers.clone(),
        presence_sensor: record.presence_sensor.clone(),
        override_input_boolean: record.override_input_boolean.clone(),
        rate_limit_seconds: record.rate_limit_seconds,
        lock_timeout,
    }
}

#[derive(Clone)]
pub struct AppState {
    inner: std::sync::Arc<Inner>,
}

pub struct Inner {
    pub config: ProxyConfig,
    pub defaults: ChargerDefaults,
    pub store: SqliteStore,
    pub ha: Option<HaClient>,
    pub clock: SystemClock,
    pub cancel: CancellationToken,
    pub lock_timeout: Duration,
    pub admin_token: Option<String>,
    stations: Mutex<BTreeMap<String, StationHandle>>,
}

impl AppState {
    pub fn new(
        config: ProxyConfig,
        defaults: ChargerDefaults,
        store: SqliteStore,
        ha: Option<HaClient>,
        cancel: CancellationToken,
        lock_timeout: Duration,
        admin_token: Option<String>,
    ) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                config,
                defaults,
                store,
                ha,
                clock: SystemClock,
                cancel,
                lock_timeout,
                admin_token,
                stations: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    pub fn defaults(&self) -> ChargerDefaults {
        self.inner.defaults.clone()
    }

    pub fn yaml_services(&self) -> Vec<OcppServiceConfig> {
        self.inner.config.ocpp_services()
    }

    pub fn root_cancel(&self) -> &CancellationToken {
        &self.inner.cancel
    }

    pub fn store(&self) -> &SqliteStore {
        &self.inner.store
    }

    pub fn clock_iso(&self) -> String {
        self.inner.clock.now_iso()
    }

    pub fn clock(&self) -> &SystemClock {
        &self.inner.clock
    }

    pub fn ha(&self) -> Option<&HaClient> {
        self.inner.ha.as_ref()
    }

    pub fn lock_timeout(&self) -> Duration {
        self.inner.lock_timeout
    }

    pub fn authorize(&self, presented: Option<&str>) -> bool {
        match (self.inner.admin_token.as_deref(), presented) {
            (Some(expected), Some(presented)) => tokens_match(expected, presented),
            _ => false,
        }
    }

    pub fn attach(&self, record: &ChargerRecord) -> StationHandle {
        let mut stations = self
            .inner
            .stations
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(existing) = stations.get(&record.id) {
            return existing.clone();
        }
        let station = StationHandle::open(
            record,
            self.inner.lock_timeout,
            self.inner.cancel.child_token(),
        );
        stations.insert(record.id.clone(), station.clone());
        station
    }

    pub fn station(&self, id: &str) -> Option<StationHandle> {
        self.inner
            .stations
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(id)
            .cloned()
    }

    pub fn stations(&self) -> Vec<StationHandle> {
        self.inner
            .stations
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn remove_station(&self, id: &str) -> Option<StationHandle> {
        self.inner
            .stations
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(id)
    }
}

fn tokens_match(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    if expected.len() != presented.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (left, right) in expected.iter().zip(presented.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}
