use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use ocpp_proxy_domain::{ProxyConfig, SharingPolicy};

static FILE_SEQ: AtomicU64 = AtomicU64::new(0);
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn write_file(ext: &str, body: &str) -> PathBuf {
    let n = FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("ocpp-proxy-cfg-{n}-{}.{ext}", std::process::id()));
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn valid_yaml_reads_every_field() {
    let path = write_file(
        "yaml",
        r#"
allow_shared_charging: true
preferred_provider: test_provider
blocked_providers: ["bad_provider"]
allowed_providers: ["good_provider"]
presence_sensor: binary_sensor.presence
override_input_boolean: input_boolean.override
rate_limit_seconds: 30
ocpp_version: "2.0.1"
auto_detect_ocpp_version: false
ocpp_services:
  - id: test_service
    url: wss://test.com/ocpp
    version: "1.6"
    auth_type: token
    token: test_token
    enabled: true
"#,
    );
    let config = ProxyConfig::load(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(config.allow_shared_charging());
    assert_eq!(config.preferred_provider(), "test_provider");
    assert_eq!(config.blocked_providers(), vec!["bad_provider"]);
    assert_eq!(config.allowed_providers(), vec!["good_provider"]);
    assert_eq!(config.presence_sensor(), "binary_sensor.presence");
    assert_eq!(config.override_input_boolean(), "input_boolean.override");
    assert_eq!(config.rate_limit_seconds().unwrap(), 30);
    assert_eq!(config.ocpp_version(), "2.0.1");
    assert!(!config.auto_detect_ocpp_version());
    let services = config.ocpp_services();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].id, "test_service");
    assert_eq!(services[0].version(), "1.6");
    assert_eq!(services[0].token.as_deref(), Some("test_token"));
    assert!(services[0].enabled);
}

#[test]
fn missing_file_is_defaults() {
    let config = ProxyConfig::load("/nonexistent/path/config.yaml").unwrap();
    assert_defaults(&config);
}

#[test]
fn empty_and_whitespace_documents_are_defaults() {
    for body in ["", "   \n\t"] {
        let config = ProxyConfig::from_document(body).unwrap();
        assert_defaults(&config);
    }
    let path = write_file("yaml", "");
    let config = ProxyConfig::load(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert_defaults(&config);
}

#[test]
fn partial_document_keeps_other_defaults() {
    let config =
        ProxyConfig::from_document("allow_shared_charging: true\nrate_limit_seconds: 60\n")
            .unwrap();
    assert!(config.allow_shared_charging());
    assert_eq!(config.preferred_provider(), "");
    assert!(config.blocked_providers().is_empty());
    assert!(config.allowed_providers().is_empty());
    assert_eq!(config.presence_sensor(), "");
    assert_eq!(config.override_input_boolean(), "");
    assert_eq!(config.rate_limit_seconds().unwrap(), 60);
    assert!(config.ocpp_services().is_empty());
    assert_eq!(config.ocpp_version(), "1.6");
    assert!(config.auto_detect_ocpp_version());
}

#[test]
fn type_coercion_matches_python() {
    let config = ProxyConfig::from_document(
        r#"
allow_shared_charging: "true"
rate_limit_seconds: "25"
preferred_provider: 123
blocked_providers: ["single_provider"]
allowed_providers: null
ocpp_services: null
"#,
    )
    .unwrap();
    assert!(config.allow_shared_charging());
    assert_eq!(config.rate_limit_seconds().unwrap(), 25);
    assert_eq!(config.preferred_provider(), "123");
    assert_eq!(config.blocked_providers(), vec!["single_provider"]);
    assert!(config.allowed_providers().is_empty());
    assert!(config.ocpp_services().is_empty());
}

#[test]
fn boolean_shapes() {
    let cases = [
        ("true", true),
        ("false", false),
        ("1", true),
        ("0", false),
        ("1.5", true),
        ("0.0", false),
        ("''", false),
        ("null", false),
        ("non-empty-string", true),
        ("[]", false),
        ("[1]", false),
    ];
    for (raw, expected) in cases {
        let config =
            ProxyConfig::from_document(&format!("allow_shared_charging: {raw}\n")).unwrap();
        assert_eq!(config.allow_shared_charging(), expected, "input {raw}");
    }
}

#[test]
fn string_shapes_including_null() {
    let cases = [
        ("test_string", "test_string"),
        ("123", "123"),
        ("true", "True"),
        ("false", "False"),
        ("''", ""),
        ("null", "None"),
        ("[1]", "[1]"),
    ];
    for (raw, expected) in cases {
        let config = ProxyConfig::from_document(&format!("preferred_provider: {raw}\n")).unwrap();
        assert_eq!(config.preferred_provider(), expected, "input {raw}");
    }
    let missing = ProxyConfig::from_document("allow_shared_charging: false\n").unwrap();
    assert_eq!(missing.preferred_provider(), "");
    assert_eq!(missing.presence_sensor(), "");
    assert_eq!(missing.override_input_boolean(), "");
}

#[test]
fn integer_shapes_and_invalid() {
    for (raw, expected) in [("10", 10), ("'15'", 15), ("0", 0), ("-5", -5), ("'0'", 0)] {
        let config = ProxyConfig::from_document(&format!("rate_limit_seconds: {raw}\n")).unwrap();
        assert_eq!(
            config.rate_limit_seconds().unwrap(),
            expected,
            "input {raw}"
        );
    }
    let config = ProxyConfig::from_document("rate_limit_seconds: invalid_number\n").unwrap();
    let err = config.rate_limit_seconds().unwrap_err();
    assert!(err.to_string().contains("invalid_number"));
    let _: &dyn Error = &err;

    let fraction = ProxyConfig::from_document("rate_limit_seconds: 1.5\n").unwrap();
    assert!(fraction.rate_limit_seconds().is_err());
    let flag = ProxyConfig::from_document("rate_limit_seconds: true\n").unwrap();
    assert!(flag
        .rate_limit_seconds()
        .unwrap_err()
        .to_string()
        .contains("true"));
}

#[test]
fn list_shapes() {
    let items = ProxyConfig::from_document("blocked_providers: [a, b, c]\n").unwrap();
    assert_eq!(items.blocked_providers(), vec!["a", "b", "c"]);
    let empty = ProxyConfig::from_document("blocked_providers: []\n").unwrap();
    assert!(empty.blocked_providers().is_empty());
    let null_list = ProxyConfig::from_document("blocked_providers: null\n").unwrap();
    assert!(null_list.blocked_providers().is_empty());
    let not_a_list = ProxyConfig::from_document("allowed_providers: 5\n").unwrap();
    assert!(not_a_list.allowed_providers().is_empty());
    let numbers = ProxyConfig::from_document("allowed_providers: [1, x]\n").unwrap();
    assert_eq!(numbers.allowed_providers(), vec!["1", "x"]);
}

#[test]
fn disallowed_providers_fall_back_until_blocked_key_exists() {
    let old = ProxyConfig::from_document(
        "disallowed_providers: [old_blocked_provider]\nallowed_providers: [allowed_provider]\n",
    )
    .unwrap();
    assert_eq!(old.disallowed_providers(), vec!["old_blocked_provider"]);
    assert_eq!(old.blocked_providers(), vec!["old_blocked_provider"]);

    let both = ProxyConfig::from_document(
        "blocked_providers: [new_blocked]\ndisallowed_providers: [old_blocked]\n",
    )
    .unwrap();
    assert_eq!(both.blocked_providers(), vec!["new_blocked"]);
    assert_eq!(both.disallowed_providers(), vec!["new_blocked"]);

    let null_blocked = ProxyConfig::from_document(
        "blocked_providers: null\ndisallowed_providers: [old_blocked]\n",
    )
    .unwrap();
    assert!(null_blocked.blocked_providers().is_empty());
}

#[test]
fn ocpp_services_three_auth_shapes_and_skips() {
    let config = ProxyConfig::from_document(
        r#"
ocpp_services:
  - id: service1
    url: wss://service1.com/ocpp
    auth_type: basic
    username: user1
    password: pass1
    enabled: true
  - id: service2
    url: wss://service2.com/ocpp
    auth_type: token
    token: token123
    enabled: false
  - id: service3
    url: wss://service3.com/ocpp
    auth_type: none
  - skip-me
"#,
    )
    .unwrap();
    let services = config.ocpp_services();
    assert_eq!(services.len(), 3);
    assert_eq!(services[0].auth_type.as_deref(), Some("basic"));
    assert_eq!(services[0].username.as_deref(), Some("user1"));
    assert_eq!(services[0].password.as_deref(), Some("pass1"));
    assert!(services[0].enabled);
    assert_eq!(services[0].version(), "1.6");
    assert_eq!(services[1].token.as_deref(), Some("token123"));
    assert!(!services[1].enabled);
    assert_eq!(services[2].auth_type.as_deref(), Some("none"));
    assert!(services[2].enabled);
    assert!(services[2].username.is_none());

    let weird = ProxyConfig::from_document("ocpp_services: nope\n").unwrap();
    assert!(weird.ocpp_services().is_empty());
}

#[test]
fn non_string_mapping_key_and_float_are_read() {
    let config = ProxyConfig::from_document("true: 1\nrate_limit_seconds: 1.5\n").unwrap();
    assert!(config
        .rate_limit_seconds()
        .unwrap_err()
        .to_string()
        .contains("1.5"));
    let huge = ProxyConfig::from_document("rate_limit_seconds: 18446744073709551615\n").unwrap();
    assert!(huge.rate_limit_seconds().is_err());
    let huge_flag =
        ProxyConfig::from_document("allow_shared_charging: 18446744073709551615\n").unwrap();
    assert!(huge_flag.allow_shared_charging());
}

#[test]
fn json_options_document() {
    let config = ProxyConfig::from_document(
        r#"{"allow_shared_charging": true, "preferred_provider": null, "rate_limit_seconds": "8"}"#,
    )
    .unwrap();
    assert!(config.allow_shared_charging());
    assert_eq!(config.preferred_provider(), "None");
    assert_eq!(config.rate_limit_seconds().unwrap(), 8);
}

#[test]
fn invalid_documents_and_unreadable_path() {
    let yaml = ProxyConfig::from_document(":\n  :").unwrap_err();
    assert!(yaml.to_string().contains("failed to parse config"));
    let _: &dyn Error = &yaml;

    let json = ProxyConfig::from_document("{").unwrap_err();
    assert!(json.to_string().contains("failed to parse config"));

    let root = ProxyConfig::from_document("- 1\n").unwrap_err();
    assert!(root.to_string().contains("mapping"));
    let array = ProxyConfig::from_document("[1]").unwrap_err();
    assert!(array.to_string().contains("mapping"));

    let yaml_null = ProxyConfig::from_document("null\n").unwrap();
    assert_defaults(&yaml_null);

    let err = ProxyConfig::load(std::env::temp_dir()).unwrap_err();
    assert!(err.to_string().contains("failed to read config"));
}

#[test]
fn default_path_follows_addon_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = std::env::var("ADDON_CONFIG_FILE").ok();
    let path = write_file(
        "yaml",
        "allow_shared_charging: true\nrate_limit_seconds: 4\n",
    );
    std::env::set_var("ADDON_CONFIG_FILE", &path);
    let loaded = ProxyConfig::load_default().unwrap();
    assert!(loaded.allow_shared_charging());
    assert_eq!(loaded.rate_limit_seconds().unwrap(), 4);

    std::env::set_var("ADDON_CONFIG_FILE", "/nonexistent/ocpp-proxy-missing.yaml");
    assert_defaults(&ProxyConfig::load_default().unwrap());

    std::env::remove_var("ADDON_CONFIG_FILE");
    assert!(ProxyConfig::load_default().is_ok());

    fs::remove_file(&path).unwrap();
    match previous {
        Some(value) => std::env::set_var("ADDON_CONFIG_FILE", value),
        None => std::env::remove_var("ADDON_CONFIG_FILE"),
    }

    let policy = SharingPolicy::from_config(&loaded)
        .unwrap()
        .with_lock_timeout(Duration::from_millis(50));
    assert_eq!(policy.lock_timeout, Duration::from_millis(50));
    assert_eq!(policy.rate_limit_seconds, 4);
    let broken = ProxyConfig::from_document("rate_limit_seconds: nope\n").unwrap();
    assert!(SharingPolicy::from_config(&broken).is_err());
}

fn assert_defaults(config: &ProxyConfig) {
    assert!(!config.allow_shared_charging());
    assert_eq!(config.preferred_provider(), "");
    assert!(config.blocked_providers().is_empty());
    assert!(config.allowed_providers().is_empty());
    assert_eq!(config.presence_sensor(), "");
    assert_eq!(config.override_input_boolean(), "");
    assert_eq!(config.rate_limit_seconds().unwrap(), 10);
    assert!(config.ocpp_services().is_empty());
    assert_eq!(config.ocpp_version(), "1.6");
    assert!(config.auto_detect_ocpp_version());
}
