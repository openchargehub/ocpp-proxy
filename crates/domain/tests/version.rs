use std::error::Error;

use ocpp_proxy_domain::{
    detect_version, is_version_supported, resolve_version, supported_versions, ConnectionHints,
    OcppVersion,
};

fn hints() -> ConnectionHints {
    ConnectionHints::default()
}

#[test]
fn version_labels() {
    assert_eq!(OcppVersion::V16.as_str(), "1.6");
    assert_eq!(OcppVersion::V201.as_str(), "2.0.1");
    assert_eq!(OcppVersion::V16.subprotocol(), "ocpp1.6");
    assert_eq!(OcppVersion::V201.subprotocol(), "ocpp2.0.1");
    assert_eq!(supported_versions(), ["1.6", "2.0.1"]);
    assert!(is_version_supported("1.6"));
    assert!(is_version_supported("2.0.1"));
    assert!(!is_version_supported("2.0"));
    assert!(!is_version_supported(" 1.6 "));
}

#[test]
fn subprotocol_detection() {
    let mut sample = hints();
    sample.subprotocol = Some("ocpp1.6".into());
    sample.query = vec![("version".into(), "2.0.1".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));

    sample.subprotocol = Some("OCPP2.0.1".into());
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.subprotocol = Some("ocpp2.0".into());
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.subprotocol = Some("chat".into());
    sample.query.clear();
    assert_eq!(detect_version(&sample), None);

    sample.subprotocol = Some(String::new());
    assert_eq!(detect_version(&sample), None);
}

#[test]
fn header_detection() {
    let mut sample = hints();
    sample.headers = vec![("Sec-WebSocket-Protocol".into(), "1.6".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));

    sample.headers = vec![("Sec-WebSocket-Protocol".into(), "2.0.1".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.headers = vec![("Sec-WebSocket-Protocol".into(), "2.0".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.headers = vec![
        ("Sec-WebSocket-Protocol".into(), "chat".into()),
        ("X-OCPP-Version".into(), "1.6".into()),
    ];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));

    sample.headers = vec![("x-ocpp-version".into(), "2.0.1".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.headers = vec![("X-OCPP-Version".into(), "2.0".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.headers = vec![
        ("X-OCPP-Version".into(), "1.6".into()),
        ("OCPP-Version".into(), "2.0.1".into()),
    ];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));

    sample.headers = vec![("OCPP-Version".into(), "2.0.1".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.headers = vec![("X-OCPP-Version".into(), String::new())];
    assert_eq!(detect_version(&sample), None);

    sample.headers = vec![
        ("Sec-WebSocket-Protocol".into(), "ocpp1.6".into()),
        ("X-OCPP-Version".into(), "2.0.1".into()),
    ];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));
}

#[test]
fn path_detection_each_token() {
    let paths = [
        ("/charger/v1.6", Some(OcppVersion::V16)),
        ("/ocpp16", Some(OcppVersion::V16)),
        ("/cp/1.6", Some(OcppVersion::V16)),
        ("/charger/v2.0.1", Some(OcppVersion::V201)),
        ("/ocpp201", Some(OcppVersion::V201)),
        ("/cp/2.0.1", Some(OcppVersion::V201)),
        ("/charger/v2.0", Some(OcppVersion::V201)),
        ("/ocpp20", Some(OcppVersion::V201)),
        ("/charger", None),
    ];
    for (path, expected) in paths {
        let sample = ConnectionHints {
            path: Some(path.into()),
            ..hints()
        };
        assert_eq!(detect_version(&sample), expected, "path {path}");
    }
}

#[test]
fn query_detection() {
    let mut sample = hints();
    sample.query = vec![("version".into(), "1.6".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));

    sample.query = vec![("version".into(), "2.0.1".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.query = vec![("version".into(), "2.0".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.query = vec![
        ("version".into(), "1.6".into()),
        ("ocpp_version".into(), "2.0.1".into()),
    ];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V16));

    sample.query = vec![("ocpp_version".into(), "2.0.1".into())];
    assert_eq!(detect_version(&sample), Some(OcppVersion::V201));

    sample.query = vec![("version".into(), String::new())];
    assert_eq!(detect_version(&sample), None);

    sample.query = vec![("other".into(), "1.6".into())];
    assert_eq!(detect_version(&sample), None);
}

#[test]
fn no_hints_and_resolve_rules() {
    assert_eq!(detect_version(&hints()), None);

    let mut detected = hints();
    detected.subprotocol = Some("ocpp2.0.1".into());
    assert_eq!(
        resolve_version("1.6", true, &detected).unwrap(),
        OcppVersion::V201
    );
    assert_eq!(
        resolve_version("1.6", false, &detected).unwrap(),
        OcppVersion::V16
    );
    assert_eq!(
        resolve_version("2.0.1", true, &hints()).unwrap(),
        OcppVersion::V201
    );
    assert_eq!(
        resolve_version("  1.6  ", true, &hints()).unwrap(),
        OcppVersion::V16
    );

    let err = resolve_version("2.0", false, &hints()).unwrap_err();
    assert!(err.to_string().contains("2.0"));
    let _: &dyn Error = &err;
    assert!(resolve_version("   ", true, &hints()).is_err());
}
