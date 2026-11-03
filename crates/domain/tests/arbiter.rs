use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ocpp_proxy_domain::{Arbiter, Probe, SafetyInput, SharingPolicy};

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

fn skip() -> SafetyInput {
    SafetyInput::skip()
}

#[test]
fn first_request_grants_and_second_id_is_locked() {
    let mut arbiter = Arbiter::new(policy());
    assert!(arbiter.policy().allow_shared_charging);
    assert!(arbiter.request_control("alpha", 0, skip()));
    assert_eq!(arbiter.lock_owner(), Some("alpha"));
    assert_eq!(arbiter.deadline_ms(), Some(60_000));
    assert!(!arbiter.request_control("beta", 10_000, skip()));
    assert_eq!(arbiter.lock_owner(), Some("alpha"));
}

#[test]
fn rate_limit_boundary_and_stamp_survives_safety_denial() {
    let mut arbiter = Arbiter::new(policy());
    assert!(arbiter.request_control("alpha", 0, skip()));
    arbiter.release();
    assert!(!arbiter.request_control("alpha", 9_999, skip()));
    assert!(arbiter.request_control("alpha", 10_000, skip()));

    let mut denied = Arbiter::new(policy());
    denied.policy_mut().blocked_providers = vec!["alpha".into()];
    assert!(!denied.request_control("alpha", 0, skip()));
    denied.policy_mut().blocked_providers.clear();
    assert!(!denied.request_control("alpha", 1_000, skip()));
    assert!(denied.request_control("alpha", 10_000, skip()));
}

#[test]
fn negative_rate_limit_does_not_deny() {
    let mut arbiter = Arbiter::new(policy());
    arbiter.policy_mut().rate_limit_seconds = -5;
    assert!(arbiter.request_control("alpha", 0, skip()));
    arbiter.release();
    assert!(arbiter.request_control("alpha", 1, skip()));
}

#[test]
fn shared_charging_off_denies_everyone() {
    let mut arbiter = Arbiter::new(policy());
    arbiter.policy_mut().allow_shared_charging = false;
    assert!(!arbiter.request_control("alpha", 0, skip()));
    assert!(!arbiter.request_control("ocpp_service_grid", 0, skip()));
    assert!(arbiter.lock_owner().is_none());
}

#[test]
fn provider_lists_and_service_bypass() {
    let mut arbiter = Arbiter::new(policy());
    assert!(!arbiter.request_control("blocked", 0, skip()));
    assert!(arbiter.request_control("ocpp_service_blocked", 0, skip()));

    let mut allow = Arbiter::new(policy());
    allow.policy_mut().blocked_providers.clear();
    allow.policy_mut().allowed_providers = vec!["good".into()];
    assert!(allow.request_control("good", 0, skip()));
    allow.release();
    assert!(!allow.request_control("other", 0, skip()));
    assert!(allow.request_control("ocpp_service_other", 0, skip()));
}

#[test]
fn home_assistant_override_and_presence() {
    let mut arbiter = Arbiter::new(policy());
    arbiter.policy_mut().override_input_boolean = "input_boolean.override".into();
    arbiter.policy_mut().presence_sensor = "binary_sensor.presence".into();

    let off = SafetyInput {
        override_probe: Probe::State("off".into()),
        presence_probe: Probe::State("not_home".into()),
    };
    assert!(!arbiter.request_control("alpha", 0, off));

    let on = SafetyInput {
        override_probe: Probe::State("on".into()),
        presence_probe: Probe::State("not_home".into()),
    };
    assert!(arbiter.request_control("alpha-on", 0, on));
    arbiter.release();

    let home = SafetyInput {
        override_probe: Probe::State("on".into()),
        presence_probe: Probe::State("home".into()),
    };
    assert!(!arbiter.request_control("beta", 0, home));

    let failed = SafetyInput {
        override_probe: Probe::Failed,
        presence_probe: Probe::Failed,
    };
    assert!(arbiter.request_control("gamma", 0, failed));
    arbiter.release();

    let skipped = SafetyInput {
        override_probe: Probe::Skip,
        presence_probe: Probe::Skip,
    };
    assert!(arbiter.request_control("delta", 0, skipped));
    arbiter.release();

    arbiter.policy_mut().override_input_boolean.clear();
    arbiter.policy_mut().presence_sensor.clear();
    let ignored = SafetyInput {
        override_probe: Probe::State("off".into()),
        presence_probe: Probe::State("home".into()),
    };
    assert!(arbiter.request_control("epsilon", 0, ignored));
}

#[test]
fn preferred_preempts_and_does_not_refresh() {
    let mut arbiter = Arbiter::new(policy());
    assert!(arbiter.request_control("alpha", 0, skip()));
    assert!(arbiter.request_control("preferred", 5_000, skip()));
    assert_eq!(arbiter.lock_owner(), Some("preferred"));
    assert_eq!(arbiter.deadline_ms(), Some(65_000));
    assert!(!arbiter.request_control("preferred", 20_000, skip()));
    assert_eq!(arbiter.lock_owner(), Some("preferred"));
}

#[test]
fn release_with_and_without_timer() {
    let mut arbiter = Arbiter::new(policy());
    arbiter.release();
    assert!(arbiter.lock_owner().is_none());
    assert!(arbiter.deadline_ms().is_none());
    assert!(arbiter.request_control("alpha", 0, skip()));
    arbiter.release();
    assert!(arbiter.lock_owner().is_none());
    assert!(arbiter.deadline_ms().is_none());
}

#[test]
fn timeout_releases_on_poll_and_on_next_request() {
    let mut arbiter = Arbiter::new(policy().with_lock_timeout(Duration::from_millis(1_000)));
    assert!(arbiter.request_control("alpha", 0, skip()));
    arbiter.poll_timeout(999);
    assert_eq!(arbiter.lock_owner(), Some("alpha"));
    arbiter.poll_timeout(1_000);
    assert!(arbiter.lock_owner().is_none());

    let mut again = Arbiter::new(policy().with_lock_timeout(Duration::from_millis(1_000)));
    assert!(again.request_control("alpha", 0, skip()));
    assert!(again.request_control("beta", 1_000, skip()));
    assert_eq!(again.lock_owner(), Some("beta"));

    let mut idle = Arbiter::new(policy());
    idle.poll_timeout(0);
    assert!(idle.lock_owner().is_none());
}

#[test]
fn huge_timeout_saturates_deadline() {
    let mut arbiter = Arbiter::new(policy().with_lock_timeout(Duration::MAX));
    assert!(arbiter.request_control("alpha", 10, skip()));
    assert_eq!(arbiter.deadline_ms(), Some(i64::MAX));
}

#[test]
fn subscribers_replace_and_unsubscribe_releases_only_owner() {
    let mut arbiter = Arbiter::new(policy());
    assert_eq!(arbiter.subscribe("alpha"), 1);
    assert_eq!(arbiter.subscribe("beta"), 2);
    assert_eq!(arbiter.subscribe("alpha"), 3);
    assert_eq!(arbiter.subscriber_token("alpha"), Some(3));
    assert_eq!(arbiter.websocket_backends(), vec!["alpha", "beta"]);
    assert!(arbiter.request_control("alpha", 0, skip()));
    arbiter.unsubscribe("beta");
    assert_eq!(arbiter.lock_owner(), Some("alpha"));
    assert_eq!(arbiter.websocket_backends(), vec!["alpha"]);
    arbiter.unsubscribe("alpha");
    assert!(arbiter.lock_owner().is_none());
    assert!(arbiter.websocket_backends().is_empty());
    arbiter.unsubscribe("missing");
}

#[test]
fn concurrent_requests_have_one_winner() {
    let arbiter = Arc::new(Mutex::new(Arbiter::new(policy())));
    let mut threads = Vec::new();
    for index in 0..8 {
        let arbiter = Arc::clone(&arbiter);
        threads.push(thread::spawn(move || {
            arbiter
                .lock()
                .unwrap()
                .request_control(&format!("backend-{index}"), 0, skip())
        }));
    }
    let wins = threads
        .into_iter()
        .filter_map(|thread| thread.join().ok())
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);
}
