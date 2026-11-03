use std::time::Duration;

use crate::config::{ConfigError, ProxyConfig};

/// Outcome of a Home Assistant read. `Failed` fails open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Skip,
    State(String),
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyInput {
    pub override_probe: Probe,
    pub presence_probe: Probe,
}

impl SafetyInput {
    pub fn skip() -> Self {
        Self {
            override_probe: Probe::Skip,
            presence_probe: Probe::Skip,
        }
    }
}

/// Sharing rules copied from config. The lock timeout is injected by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharingPolicy {
    pub allow_shared_charging: bool,
    pub preferred_provider: String,
    pub blocked_providers: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub presence_sensor: String,
    pub override_input_boolean: String,
    pub rate_limit_seconds: i64,
    pub lock_timeout: Duration,
}

impl SharingPolicy {
    pub fn from_config(config: &ProxyConfig) -> Result<Self, ConfigError> {
        Ok(Self {
            allow_shared_charging: config.allow_shared_charging(),
            preferred_provider: config.preferred_provider(),
            blocked_providers: config.blocked_providers(),
            allowed_providers: config.allowed_providers(),
            presence_sensor: config.presence_sensor(),
            override_input_boolean: config.override_input_boolean(),
            rate_limit_seconds: config.rate_limit_seconds()?,
            lock_timeout: Duration::from_secs(60),
        })
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
}

/// Single control lock. `now_ms` is an injected clock so tests never sleep.
#[derive(Debug)]
pub struct Arbiter {
    policy: SharingPolicy,
    owner: Option<String>,
    last_request_ms: Vec<(String, i64)>,
    subscribers: Vec<(String, u64)>,
    next_token: u64,
    deadline_ms: Option<i64>,
}

impl Arbiter {
    pub fn new(policy: SharingPolicy) -> Self {
        Self {
            policy,
            owner: None,
            last_request_ms: Vec::new(),
            subscribers: Vec::new(),
            next_token: 0,
            deadline_ms: None,
        }
    }

    pub fn policy(&self) -> &SharingPolicy {
        &self.policy
    }

    pub fn policy_mut(&mut self) -> &mut SharingPolicy {
        &mut self.policy
    }

    pub fn lock_owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub fn deadline_ms(&self) -> Option<i64> {
        self.deadline_ms
    }

    pub fn websocket_backends(&self) -> Vec<String> {
        self.subscribers.iter().map(|(id, _)| id.clone()).collect()
    }

    pub fn subscriber_token(&self, backend_id: &str) -> Option<u64> {
        self.subscribers
            .iter()
            .find(|(id, _)| id == backend_id)
            .map(|(_, token)| *token)
    }

    pub fn subscribe(&mut self, backend_id: &str) -> u64 {
        self.next_token += 1;
        let token = self.next_token;
        if let Some(slot) = self.subscribers.iter_mut().find(|(id, _)| id == backend_id) {
            slot.1 = token;
        } else {
            self.subscribers.push((backend_id.to_string(), token));
        }
        token
    }

    pub fn unsubscribe(&mut self, backend_id: &str) {
        self.subscribers.retain(|(id, _)| id != backend_id);
        if self.owner.as_deref() == Some(backend_id) {
            self.release();
        }
    }

    pub fn release(&mut self) {
        self.owner = None;
        self.deadline_ms = None;
    }

    pub fn poll_timeout(&mut self, now_ms: i64) {
        if let Some(deadline) = self.deadline_ms {
            if now_ms >= deadline {
                self.release();
            }
        }
    }

    /// Rate limit is stamped before safety checks, including denials.
    pub fn request_control(&mut self, backend_id: &str, now_ms: i64, safety: SafetyInput) -> bool {
        self.poll_timeout(now_ms);
        if !self.stamp_rate_limit(backend_id, now_ms) {
            return false;
        }
        if !self.safety_allows(backend_id, &safety) {
            return false;
        }
        self.try_acquire(backend_id, now_ms)
    }

    fn stamp_rate_limit(&mut self, backend_id: &str, now_ms: i64) -> bool {
        if let Some(last) = self.last_ms(backend_id) {
            let elapsed = now_ms - last;
            let limit_ms = self.policy.rate_limit_seconds.saturating_mul(1000);
            if elapsed < limit_ms {
                return false;
            }
        }
        self.remember(backend_id, now_ms);
        true
    }

    fn last_ms(&self, backend_id: &str) -> Option<i64> {
        self.last_request_ms
            .iter()
            .find(|(id, _)| id == backend_id)
            .map(|(_, stamp)| *stamp)
    }

    fn remember(&mut self, backend_id: &str, now_ms: i64) {
        if let Some(slot) = self
            .last_request_ms
            .iter_mut()
            .find(|(id, _)| id == backend_id)
        {
            slot.1 = now_ms;
        } else {
            self.last_request_ms.push((backend_id.to_string(), now_ms));
        }
    }

    fn safety_allows(&self, backend_id: &str, safety: &SafetyInput) -> bool {
        if !self.policy.allow_shared_charging {
            return false;
        }
        if !self.override_allows(safety) {
            return false;
        }
        if !self.presence_allows(safety) {
            return false;
        }
        self.provider_allows(backend_id)
    }

    fn override_allows(&self, safety: &SafetyInput) -> bool {
        if self.policy.override_input_boolean.is_empty() {
            return true;
        }
        match &safety.override_probe {
            Probe::Skip | Probe::Failed => true,
            Probe::State(state) if state == "on" => true,
            Probe::State(_) => false,
        }
    }

    fn presence_allows(&self, safety: &SafetyInput) -> bool {
        if self.policy.presence_sensor.is_empty() {
            return true;
        }
        match &safety.presence_probe {
            Probe::Skip | Probe::Failed => true,
            Probe::State(state) if state == "home" => false,
            Probe::State(_) => true,
        }
    }

    fn provider_allows(&self, backend_id: &str) -> bool {
        if backend_id.starts_with("ocpp_service_") {
            return true;
        }
        if self
            .policy
            .blocked_providers
            .iter()
            .any(|id| id == backend_id)
        {
            return false;
        }
        if self.policy.allowed_providers.is_empty() {
            return true;
        }
        self.policy
            .allowed_providers
            .iter()
            .any(|id| id == backend_id)
    }

    fn try_acquire(&mut self, backend_id: &str, now_ms: i64) -> bool {
        let preferred = backend_id == self.policy.preferred_provider;
        let owns = self.owner.as_deref() == Some(backend_id);
        if self.owner.is_some() && preferred && !owns {
            self.release();
        }
        if self.owner.is_some() {
            return false;
        }
        self.owner = Some(backend_id.to_string());
        self.arm_timer(now_ms);
        true
    }

    fn arm_timer(&mut self, now_ms: i64) {
        let timeout_ms = i64::try_from(self.policy.lock_timeout.as_millis()).unwrap_or(i64::MAX);
        self.deadline_ms = Some(now_ms.saturating_add(timeout_ms));
    }
}
