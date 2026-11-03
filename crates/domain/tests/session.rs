use ocpp_proxy_domain::SessionLedger;

#[test]
fn v16_ids_increment_and_energy_is_kilowatt_hours() {
    let mut ledger = SessionLedger::new();
    let first = ledger.start_v16(1, "RFID123", "2023-01-01T12:00:00Z", 1_000);
    let second = ledger.start_v16(1, "RFID456", "2023-01-01T12:00:00Z", 0);
    assert_eq!(first, 1);
    assert_eq!(second, 2);

    let usage = ledger
        .stop("1", 6_000, "2023-01-01T13:00:00Z", Some("provider"))
        .unwrap();
    assert_eq!(usage.energy_kwh, 5.0);
    assert_eq!(usage.duration_s, 3_600.0);
    assert_eq!(usage.backend_id, "provider");
    assert_eq!(usage.revenue, 0.0);
    let row = usage.to_row("2023-01-01T13:00:01+00:00");
    assert_eq!(row.timestamp, "2023-01-01T13:00:01+00:00");
    assert_eq!(row.energy_kwh, 5.0);
}

#[test]
fn thirty_minutes_is_1800_seconds() {
    let mut ledger = SessionLedger::new();
    ledger.start_v16(1, "RFID123", "2023-01-01T12:00:00Z", 0);
    let usage = ledger
        .stop("1", 1_000, "2023-01-01T12:30:00Z", Some("alpha"))
        .unwrap();
    assert_eq!(usage.duration_s, 1_800.0);
    assert_eq!(usage.energy_kwh, 1.0);
}

#[test]
fn bad_timestamps_log_zero_duration_and_unknown_id_does_not() {
    let mut ledger = SessionLedger::new();
    ledger.start_v16(1, "tag", "not-a-date", 1_000);
    let usage = ledger.stop("1", 6_000, "also-bad", None).unwrap();
    assert_eq!(usage.duration_s, 0.0);
    assert_eq!(usage.energy_kwh, 5.0);
    assert_eq!(usage.backend_id, "");
    assert!(ledger.stop("1", 0, "2023-01-01T12:00:00Z", None).is_none());

    ledger.start_v16(1, "tag", "2023-01-01T12:00:00Z", 0);
    let partial = ledger.stop("2", 0, "not-a-date", Some("x")).unwrap();
    assert_eq!(partial.duration_s, 0.0);
}

#[test]
fn v201_keeps_charger_id_and_does_not_consume_v16_counter() {
    let mut ledger = SessionLedger::new();
    ledger.start_v201("tx-9", 2, "token", "2023-01-01T12:00:00Z", 0);
    assert_eq!(ledger.start_v16(1, "tag", "2023-01-01T12:00:00Z", 0), 1);
    let usage = ledger
        .stop(
            "tx-9",
            2_500,
            "2023-01-01T12:30:00+00:00",
            Some("ocpp_service_grid"),
        )
        .unwrap();
    assert_eq!(usage.energy_kwh, 2.5);
    assert_eq!(usage.duration_s, 1_800.0);
    assert_eq!(usage.backend_id, "ocpp_service_grid");
}
