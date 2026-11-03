use std::sync::Mutex;

use ocpp_proxy_domain::{Arbiter, OcppVersion, Probe, SafetyInput, SharingPolicy};

use crate::ports::{ChargerCommand, Clock, EntityState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStart {
    pub version: OcppVersion,
    pub backend_id: String,
    pub connector_id: i64,
    pub id_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStop {
    pub version: OcppVersion,
    pub backend_id: String,
    pub transaction_id: String,
    pub acquire_lock: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDecision {
    Locked,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDecision {
    Accepted,
    Rejected,
}

pub async fn remote_start(
    arbiter: &Mutex<Arbiter>,
    entities: &dyn EntityState,
    charger: &dyn ChargerCommand,
    clock: &dyn Clock,
    request: &RemoteStart,
) -> StartDecision {
    let granted = acquire_control(arbiter, entities, clock, &request.backend_id).await;
    if !granted {
        return StartDecision::Locked;
    }
    let started = charger
        .remote_start(request.version, request.connector_id, &request.id_tag)
        .await;
    if matches!(started, Ok(true)) {
        return StartDecision::Accepted;
    }
    release_if_owner(arbiter, &request.backend_id);
    StartDecision::Rejected
}

pub async fn remote_stop(
    arbiter: &Mutex<Arbiter>,
    entities: &dyn EntityState,
    charger: &dyn ChargerCommand,
    clock: &dyn Clock,
    request: &RemoteStop,
) -> StopDecision {
    if request.acquire_lock && !acquire_control(arbiter, entities, clock, &request.backend_id).await
    {
        return StopDecision::Rejected;
    }
    let stopped = charger
        .remote_stop(request.version, &request.transaction_id)
        .await;
    if matches!(stopped, Ok(true)) {
        return StopDecision::Accepted;
    }
    if request.acquire_lock {
        release_if_owner(arbiter, &request.backend_id);
    }
    StopDecision::Rejected
}

pub async fn acquire_control(
    arbiter: &Mutex<Arbiter>,
    entities: &dyn EntityState,
    clock: &dyn Clock,
    backend_id: &str,
) -> bool {
    let policy = lock(arbiter).policy().clone();
    let safety = safety_input(&policy, entities).await;
    lock(arbiter).request_control(backend_id, clock.now_ms(), safety)
}

async fn safety_input(policy: &SharingPolicy, entities: &dyn EntityState) -> SafetyInput {
    SafetyInput {
        override_probe: probe(entities, &policy.override_input_boolean).await,
        presence_probe: probe(entities, &policy.presence_sensor).await,
    }
}

async fn probe(entities: &dyn EntityState, entity_id: &str) -> Probe {
    if entity_id.is_empty() {
        return Probe::Skip;
    }
    match entities.state_of(entity_id).await {
        Ok(state) => Probe::State(state),
        Err(_) => Probe::Failed,
    }
}

fn release_if_owner(arbiter: &Mutex<Arbiter>, backend_id: &str) {
    let mut guard = lock(arbiter);
    if guard.lock_owner() == Some(backend_id) {
        guard.release();
    }
}

fn lock(arbiter: &Mutex<Arbiter>) -> std::sync::MutexGuard<'_, Arbiter> {
    arbiter.lock().unwrap_or_else(|err| err.into_inner())
}
