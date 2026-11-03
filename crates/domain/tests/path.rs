use ocpp_proxy_domain::{
    parse_station_path, validate_charger_id, PathError, StationRoute, DEFAULT_STATION_ID,
};

#[test]
fn bare_charger_is_the_default_charge_point() {
    assert_eq!(
        parse_station_path("/charger").unwrap(),
        StationRoute::ChargePoint {
            id: DEFAULT_STATION_ID.into(),
        }
    );
    assert_eq!(
        parse_station_path("/charger/").unwrap(),
        StationRoute::ChargePoint {
            id: DEFAULT_STATION_ID.into(),
        }
    );
}

#[test]
fn version_suffixes_stay_on_the_default_station() {
    for path in ["/charger/v1.6", "/charger/v2.0", "/charger/V2.0.1"] {
        assert_eq!(
            parse_station_path(path).unwrap(),
            StationRoute::ChargePoint {
                id: DEFAULT_STATION_ID.into(),
            },
            "{path}"
        );
    }
}

#[test]
fn named_id_is_a_charge_point_and_backend_subpath_is_a_provider() {
    assert_eq!(
        parse_station_path("/charger/CPCYBER01").unwrap(),
        StationRoute::ChargePoint {
            id: "CPCYBER01".into(),
        }
    );
    assert_eq!(
        parse_station_path("/charger/CPVIARIO001").unwrap(),
        StationRoute::ChargePoint {
            id: "CPVIARIO001".into(),
        }
    );
    assert_eq!(
        parse_station_path("/charger/CPCYBER01/backend").unwrap(),
        StationRoute::Provider {
            id: "CPCYBER01".into(),
        }
    );
    assert_eq!(
        parse_station_path("/charger/v1.6/backend").unwrap(),
        StationRoute::Provider {
            id: DEFAULT_STATION_ID.into(),
        }
    );
}

#[test]
fn charger_ids_accept_letters_digits_underscore_and_hyphen() {
    assert!(validate_charger_id("A").is_ok());
    assert!(validate_charger_id("CPCYBER01").is_ok());
    assert!(validate_charger_id("cp_via-rio001").is_ok());
    assert!(validate_charger_id(&"a".repeat(48)).is_ok());
}

#[test]
fn charger_ids_reject_empty_long_dotted_and_non_ascii() {
    for id in ["", &"a".repeat(49), "bad id", "cp.01", "v1.6", "v2.0", "v2.0.1", "充電", "a/b"] {
        let err = validate_charger_id(id).unwrap_err();
        assert_eq!(err, PathError { segment: id.into() }, "{id}");
    }
    let err = parse_station_path("/charger/bad id").unwrap_err();
    assert_eq!(err.segment, "bad id");
    assert!(parse_station_path("/charger/CPCYBER01/backend/extra").is_err());
    assert!(parse_station_path("/other").is_err());
}

#[test]
fn named_id_is_not_a_version_hint() {
    use ocpp_proxy_domain::version_hint_path;

    assert_eq!(version_hint_path("/charger/ocpp16"), None);
    assert_eq!(
        version_hint_path("/charger/v2.0.1").as_deref(),
        Some("/charger/v2.0.1")
    );
    assert_eq!(version_hint_path("/charger").as_deref(), Some("/charger"));
    assert_eq!(
        version_hint_path("/charger/v1.6").as_deref(),
        Some("/charger/v1.6")
    );
}
