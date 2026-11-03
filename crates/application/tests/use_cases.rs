use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use ocpp_proxy_application::{
    fault_message_v16, fault_message_v201, is_fault_status, list_sessions, note_fault, on_boot,
    on_heartbeat, on_meter, on_start, on_start_v201, on_status, on_transaction_stop, remote_start,
    remote_stop, session_ended_message, BackendHub, ChargerCommand, ChargerEvent, Clock,
    EntityState, Notifier, RemoteStart, RemoteStop, SessionStore, StartDecision, StartInput,
    StopDecision, StopInput, UpstreamHub,
};
use ocpp_proxy_domain::{Arbiter, OcppVersion, SessionLedger, SessionRow, SharingPolicy};
use serde_json::json;

struct ManualClock;

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        0
    }
    fn now_iso(&self) -> String {
        "2023-01-01T13:00:00+00:00".into()
    }
}

struct Entities {
    fail: bool,
    state: String,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl EntityState for Entities {
    async fn state_of(&self, entity_id: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push(entity_id.to_string());
        if self.fail {
            return Err(format!("down:{entity_id}"));
        }
        Ok(self.state.clone())
    }
}

struct Charger {
    start_ok: Mutex<Result<bool, String>>,
    stop_ok: Mutex<Result<bool, String>>,
    starts: Mutex<Vec<(i64, String)>>,
    stops: Mutex<Vec<String>>,
}

#[async_trait]
impl ChargerCommand for Charger {
    async fn remote_start(
        &self,
        _version: OcppVersion,
        connector_id: i64,
        id_tag: &str,
    ) -> Result<bool, String> {
        self.starts
            .lock()
            .unwrap()
            .push((connector_id, id_tag.to_string()));
        self.start_ok.lock().unwrap().clone()
    }

    async fn remote_stop(
        &self,
        _version: OcppVersion,
        transaction_id: &str,
    ) -> Result<bool, String> {
        self.stops.lock().unwrap().push(transaction_id.to_string());
        self.stop_ok.lock().unwrap().clone()
    }
}

struct Notes {
    fail: bool,
    items: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Notifier for Notes {
    async fn notify(&self, title: &str, message: &str) -> Result<(), String> {
        self.items
            .lock()
            .unwrap()
            .push((title.into(), message.into()));
        if self.fail {
            return Err("ha down".into());
        }
        Ok(())
    }
}

struct Memory {
    rows: Mutex<Vec<SessionRow>>,
    fail: bool,
}

impl SessionStore for Memory {
    fn insert(&self, row: &SessionRow) -> Result<(), String> {
        if self.fail {
            return Err("disk".into());
        }
        self.rows.lock().unwrap().push(row.clone());
        Ok(())
    }
    fn list(&self) -> Result<Vec<SessionRow>, String> {
        Ok(self.rows.lock().unwrap().clone())
    }
}

#[derive(Default)]
struct Sink {
    events: Mutex<Vec<ChargerEvent>>,
}

impl BackendHub for Sink {
    fn broadcast(&self, event: &ChargerEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

impl UpstreamHub for Sink {
    fn forward(&self, event: &ChargerEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn policy() -> SharingPolicy {
    SharingPolicy {
        allow_shared_charging: true,
        preferred_provider: "preferred".into(),
        blocked_providers: vec!["blocked".into()],
        allowed_providers: Vec::new(),
        presence_sensor: String::new(),
        override_input_boolean: String::new(),
        rate_limit_seconds: 10,
        lock_timeout: Duration::from_secs(60),
    }
}

fn start_req(id: &str) -> RemoteStart {
    RemoteStart {
        version: OcppVersion::V16,
        backend_id: id.into(),
        connector_id: 1,
        id_tag: "RFID".into(),
    }
}

fn charger_ok() -> Charger {
    Charger {
        start_ok: Mutex::new(Ok(true)),
        stop_ok: Mutex::new(Ok(true)),
        starts: Mutex::new(Vec::new()),
        stops: Mutex::new(Vec::new()),
    }
}

fn blank_entities() -> Entities {
    Entities {
        fail: false,
        state: "on".into(),
        calls: Mutex::new(Vec::new()),
    }
}

#[tokio::test]
async fn remote_start_grants_and_rejected_start_releases_the_lock() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    let entities = blank_entities();
    let charger = charger_ok();
    let decision = remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    assert_eq!(decision, StartDecision::Accepted);
    assert_eq!(arbiter.lock().unwrap().lock_owner(), Some("alpha"));
    assert_eq!(
        charger.starts.lock().unwrap().as_slice(),
        &[(1, "RFID".into())]
    );

    arbiter.lock().unwrap().release();
    *charger.start_ok.lock().unwrap() = Ok(false);
    let rejected = remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("beta"),
    )
    .await;
    assert_eq!(rejected, StartDecision::Rejected);
    assert!(arbiter.lock().unwrap().lock_owner().is_none());
    assert_eq!(charger.starts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn transport_error_releases_lock_and_each_safety_rule_blocks_the_charger() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    let entities = blank_entities();
    let charger = charger_ok();
    *charger.start_ok.lock().unwrap() = Err("socket".into());
    let decision = remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    assert_eq!(decision, StartDecision::Rejected);
    assert!(arbiter.lock().unwrap().lock_owner().is_none());

    let cases = [
        ("shared", {
            let mut rules = policy();
            rules.allow_shared_charging = false;
            rules
        }),
        ("blocked", policy()),
        ("allow", {
            let mut rules = policy();
            rules.blocked_providers.clear();
            rules.allowed_providers = vec!["good".into()];
            rules
        }),
        ("override", {
            let mut rules = policy();
            rules.override_input_boolean = "input_boolean.override".into();
            rules
        }),
        ("presence", {
            let mut rules = policy();
            rules.presence_sensor = "binary_sensor.home".into();
            rules
        }),
    ];
    for (name, rules) in cases {
        let arbiter = Mutex::new(Arbiter::new(rules));
        let charger = charger_ok();
        let entities = match name {
            "override" => Entities {
                fail: false,
                state: "off".into(),
                calls: Mutex::new(Vec::new()),
            },
            "presence" => Entities {
                fail: false,
                state: "home".into(),
                calls: Mutex::new(Vec::new()),
            },
            _ => blank_entities(),
        };
        let id = if name == "blocked" {
            "blocked"
        } else {
            "other"
        };
        let decision =
            remote_start(&arbiter, &entities, &charger, &ManualClock, &start_req(id)).await;
        assert_eq!(decision, StartDecision::Locked, "{name}");
        assert!(charger.starts.lock().unwrap().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn rate_limit_and_existing_owner_do_not_touch_the_charger() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    let entities = blank_entities();
    let charger = charger_ok();
    assert_eq!(
        remote_start(
            &arbiter,
            &entities,
            &charger,
            &ManualClock,
            &start_req("alpha")
        )
        .await,
        StartDecision::Accepted
    );
    assert_eq!(
        remote_start(
            &arbiter,
            &entities,
            &charger,
            &ManualClock,
            &start_req("beta")
        )
        .await,
        StartDecision::Locked
    );
    assert_eq!(charger.starts.lock().unwrap().len(), 1);
    arbiter.lock().unwrap().release();
    assert_eq!(
        remote_start(
            &arbiter,
            &entities,
            &charger,
            &ManualClock,
            &start_req("alpha")
        )
        .await,
        StartDecision::Locked
    );
    assert_eq!(charger.starts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn preferred_provider_preempts_through_the_use_case() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    let entities = blank_entities();
    let charger = charger_ok();
    remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    let decision = remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("preferred"),
    )
    .await;
    assert_eq!(decision, StartDecision::Accepted);
    assert_eq!(arbiter.lock().unwrap().lock_owner(), Some("preferred"));
    assert_eq!(charger.starts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn ha_errors_fail_open_and_empty_entities_are_not_read() {
    let mut rules = policy();
    rules.override_input_boolean = "input_boolean.override".into();
    rules.presence_sensor = "binary_sensor.home".into();
    let arbiter = Mutex::new(Arbiter::new(rules));
    let entities = Entities {
        fail: true,
        state: "off".into(),
        calls: Mutex::new(Vec::new()),
    };
    let charger = charger_ok();
    let decision = remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    assert_eq!(decision, StartDecision::Accepted);
    assert_eq!(entities.calls.lock().unwrap().len(), 2);

    let arbiter = Mutex::new(Arbiter::new(policy()));
    let entities = blank_entities();
    remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("beta"),
    )
    .await;
    assert!(entities.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn backend_stop_ignores_the_lock_and_upstream_stop_requires_it() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    let entities = blank_entities();
    let charger = charger_ok();
    remote_start(
        &arbiter,
        &entities,
        &charger,
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    let stop = RemoteStop {
        version: OcppVersion::V16,
        backend_id: "beta".into(),
        transaction_id: "7".into(),
        acquire_lock: false,
    };
    assert_eq!(
        remote_stop(&arbiter, &entities, &charger, &ManualClock, &stop).await,
        StopDecision::Accepted
    );
    assert_eq!(arbiter.lock().unwrap().lock_owner(), Some("alpha"));
    assert_eq!(charger.stops.lock().unwrap().as_slice(), &["7".to_string()]);

    let upstream = RemoteStop {
        version: OcppVersion::V201,
        backend_id: "ocpp_service_grid".into(),
        transaction_id: "tx".into(),
        acquire_lock: true,
    };
    assert_eq!(
        remote_stop(&arbiter, &entities, &charger, &ManualClock, &upstream).await,
        StopDecision::Rejected
    );
    assert_eq!(charger.stops.lock().unwrap().len(), 1);

    arbiter.lock().unwrap().release();
    let fresh = RemoteStop {
        backend_id: "ocpp_service_other".into(),
        ..upstream
    };
    assert_eq!(
        remote_stop(&arbiter, &entities, &charger, &ManualClock, &fresh).await,
        StopDecision::Accepted
    );
    assert_eq!(
        arbiter.lock().unwrap().lock_owner(),
        Some("ocpp_service_other")
    );
}

#[tokio::test]
async fn failed_upstream_stop_releases_the_lock_it_just_took() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    let charger = charger_ok();
    *charger.stop_ok.lock().unwrap() = Ok(false);
    let upstream = RemoteStop {
        version: OcppVersion::V16,
        backend_id: "ocpp_service_grid".into(),
        transaction_id: "9".into(),
        acquire_lock: true,
    };
    assert_eq!(
        remote_stop(
            &arbiter,
            &blank_entities(),
            &charger,
            &ManualClock,
            &upstream
        )
        .await,
        StopDecision::Rejected
    );
    assert!(arbiter.lock().unwrap().lock_owner().is_none());
}

#[tokio::test]
async fn poisoned_arbiter_still_grants() {
    let arbiter = Arc::new(Mutex::new(Arbiter::new(policy())));
    let clone = Arc::clone(&arbiter);
    let _ = thread::spawn(move || {
        let _guard = clone.lock().unwrap();
        panic!("poison");
    })
    .join();
    let charger = charger_ok();
    let decision = remote_start(
        &arbiter,
        &blank_entities(),
        &charger,
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    assert_eq!(decision, StartDecision::Accepted);
}

#[test]
fn messages_and_fault_status() {
    assert!(is_fault_status("Faulted"));
    assert!(is_fault_status("UNAVAILABLE"));
    assert!(!is_fault_status("Available"));
    assert_eq!(
        session_ended_message("alpha", 5.0, 1800.0),
        "Provider=alpha, kWh=5.00, duration=1800s"
    );
    assert_eq!(
        fault_message_v16("Faulted", "GroundFailure"),
        "Status=Faulted, Error=GroundFailure"
    );
    assert_eq!(
        fault_message_v201("Faulted", 1, 2),
        "Status=Faulted, EVSE=1, Connector=2"
    );
}

#[tokio::test]
async fn charger_events_reach_backends_and_upstream_except_boot() {
    let backends = Sink::default();
    let upstream = Sink::default();
    let boot = on_boot(
        "Vendor",
        "Model",
        Some("PowerUp".into()),
        "2023-01-01T00:00:00Z",
        &backends,
        &upstream,
    );
    assert_eq!(boot.interval, Some(10));
    assert_eq!(boot.payload_status, "Accepted");
    assert!(upstream.events.lock().unwrap().is_empty());
    assert_eq!(
        backends.events.lock().unwrap()[0].to_json(),
        json!({"type": "boot", "vendor": "Vendor", "model": "Model", "reason": "PowerUp"})
    );

    let beat = on_heartbeat("2023-01-01T00:00:01Z", &backends, &upstream);
    assert_eq!(beat.current_time.as_deref(), Some("2023-01-01T00:00:01Z"));
    assert_eq!(upstream.events.lock().unwrap().len(), 1);

    let status = ChargerEvent::Status {
        connector_id: 1,
        evse_id: None,
        error_code: Some("NoError".into()),
        status: "Available".into(),
        timestamp: None,
    };
    on_status(status, &backends, &upstream);
    let meter = ChargerEvent::Meter {
        connector_id: Some(1),
        evse_id: None,
        values: json!([{"value": 1}]),
    };
    on_meter(meter, &backends, &upstream);
    assert!(upstream.events.lock().unwrap().len() >= 3);
}

#[tokio::test]
async fn fault_revokes_control_even_when_notification_fails() {
    let arbiter = Mutex::new(Arbiter::new(policy()));
    remote_start(
        &arbiter,
        &blank_entities(),
        &charger_ok(),
        &ManualClock,
        &start_req("alpha"),
    )
    .await;
    let notes = Notes {
        fail: true,
        items: Mutex::new(Vec::new()),
    };
    assert!(!note_fault(&arbiter, &notes, "Available", "nope").await);
    assert_eq!(arbiter.lock().unwrap().lock_owner(), Some("alpha"));
    assert!(notes.items.lock().unwrap().is_empty());
    assert!(note_fault(&arbiter, &notes, "faulted", "Status=faulted, Error=NoError").await);
    assert!(arbiter.lock().unwrap().lock_owner().is_none());
    assert_eq!(notes.items.lock().unwrap()[0].0, "Charger Fault");
}

#[test]
fn session_math_is_logged_and_unknown_stop_is_silent() {
    let mut ledger = SessionLedger::new();
    let store = Memory {
        rows: Mutex::new(Vec::new()),
        fail: false,
    };
    let backends = Sink::default();
    let upstream = Sink::default();
    let tx = on_start(
        &mut ledger,
        StartInput {
            connector_id: 1,
            id_tag: "RFID",
            meter_start: 1_000,
            timestamp: "2023-01-01T12:00:00Z",
        },
        &backends,
        &upstream,
    );
    assert_eq!(tx, 1);
    let message = on_transaction_stop(
        &mut ledger,
        &store,
        StopInput {
            tx_id: "1",
            meter_stop: 6_000,
            timestamp: "2023-01-01T12:30:00Z",
            transaction_id: json!(1),
            owner: Some("alpha"),
            logged_at: "2023-01-01T12:30:01+00:00",
        },
        &backends,
        &upstream,
    )
    .unwrap();
    assert_eq!(message, "Provider=alpha, kWh=5.00, duration=1800s");
    let rows = list_sessions(&store).unwrap();
    assert_eq!(rows[0].energy_kwh, 5.0);
    assert_eq!(rows[0].backend_id, "alpha");
    assert!(on_transaction_stop(
        &mut ledger,
        &store,
        StopInput {
            tx_id: "missing",
            meter_stop: 0,
            timestamp: "2023-01-01T12:40:00Z",
            transaction_id: json!("missing"),
            owner: None,
            logged_at: "2023-01-01T12:40:00Z",
        },
        &backends,
        &upstream,
    )
    .is_none());
    assert_eq!(list_sessions(&store).unwrap().len(), 1);

    on_start_v201(
        &mut ledger,
        "tx-9",
        StartInput {
            connector_id: 2,
            id_tag: "tok",
            meter_start: 0,
            timestamp: "2023-01-01T12:00:00Z",
        },
        &backends,
        &upstream,
    );
    let events = backends.events.lock().unwrap();
    let found = events.iter().any(|event| {
        matches!(
            event,
            ChargerEvent::TransactionStarted { transaction_id, .. } if transaction_id == &json!("tx-9")
        )
    });
    assert!(found);
}

#[test]
fn event_json_omits_empty_optional_fields() {
    let boot = ChargerEvent::Boot {
        vendor: "V".into(),
        model: "M".into(),
        reason: None,
    };
    assert_eq!(
        boot.to_json(),
        json!({"type": "boot", "vendor": "V", "model": "M"})
    );
    let status = ChargerEvent::Status {
        connector_id: 2,
        evse_id: Some(3),
        error_code: None,
        status: "Faulted".into(),
        timestamp: Some("2023-01-01T00:00:00Z".into()),
    };
    assert_eq!(
        status.to_json(),
        json!({"type": "status", "connector_id": 2, "status": "Faulted", "evse_id": 3, "timestamp": "2023-01-01T00:00:00Z"})
    );
    assert!(status.forwards_upstream());
    assert!(!boot.forwards_upstream());
}
