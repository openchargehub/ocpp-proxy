use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use ocpp_proxy_application::{BackendHub, ChargerEvent, Clock, UpstreamHub};
use ocpp_proxy_domain::{Arbiter, OcppVersion, ProxyConfig, SessionLedger, SharingPolicy};
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
}

impl UpstreamHub for UpstreamSet {
    fn forward(&self, event: &ChargerEvent) {
        let _ = self.events.send(event.clone());
    }
}

#[derive(Clone)]
pub struct AppState {
    inner: std::sync::Arc<Inner>,
}

pub struct Inner {
    pub config: ProxyConfig,
    pub arbiter: Mutex<Arbiter>,
    pub ledger: Mutex<SessionLedger>,
    pub store: SqliteStore,
    pub ha: Option<HaClient>,
    pub clock: SystemClock,
    pub backends: BackendSockets,
    pub upstream: UpstreamSet,
    pub cancel: CancellationToken,
    charger: Mutex<Option<ActiveCharger>>,
    charger_generation: Mutex<u64>,
}

impl AppState {
    pub fn new(
        config: ProxyConfig,
        policy: SharingPolicy,
        store: SqliteStore,
        ha: Option<HaClient>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                config,
                arbiter: Mutex::new(Arbiter::new(policy)),
                ledger: Mutex::new(SessionLedger::new()),
                store,
                ha,
                clock: SystemClock,
                backends: BackendSockets::new(),
                upstream: UpstreamSet::new(),
                cancel,
                charger: Mutex::new(None),
                charger_generation: Mutex::new(0),
            }),
        }
    }

    pub fn config_version(&self) -> String {
        self.inner.config.ocpp_version()
    }

    pub fn auto_detect(&self) -> bool {
        self.inner.config.auto_detect_ocpp_version()
    }

    pub fn services(&self) -> Vec<ocpp_proxy_domain::OcppServiceConfig> {
        self.inner.config.ocpp_services()
    }

    pub fn root_cancel(&self) -> &CancellationToken {
        &self.inner.cancel
    }

    pub fn ledger(&self) -> MutexGuard<'_, SessionLedger> {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(|err| err.into_inner())
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

    pub fn arbiter(&self) -> &Mutex<Arbiter> {
        &self.inner.arbiter
    }

    pub fn backends(&self) -> &BackendSockets {
        &self.inner.backends
    }

    pub fn upstream(&self) -> &UpstreamSet {
        &self.inner.upstream
    }

    pub fn replace_charger(&self, link: ChargerLink, cancel: CancellationToken) -> u64 {
        let mut generation = self
            .inner
            .charger_generation
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
}
