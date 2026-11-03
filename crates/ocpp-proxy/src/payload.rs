use chrono::{DateTime, Utc};
use ocpp_proxy_domain::OcppVersion;
use rust_ocpp::v1_6::messages::boot_notification::{
    BootNotificationRequest, BootNotificationResponse,
};
use rust_ocpp::v1_6::messages::heart_beat::HeartbeatResponse;
use rust_ocpp::v1_6::messages::remote_start_transaction::RemoteStartTransactionRequest;
use rust_ocpp::v1_6::messages::remote_stop_transaction::RemoteStopTransactionRequest;
use rust_ocpp::v1_6::messages::start_transaction::StartTransactionResponse;
use rust_ocpp::v1_6::messages::stop_transaction::StopTransactionResponse;
use rust_ocpp::v1_6::types::{
    AuthorizationStatus, IdTagInfo, RegistrationStatus, RemoteStartStopStatus,
};
use rust_ocpp::v2_0_1::datatypes::charging_station_type::ChargingStationType;
use rust_ocpp::v2_0_1::datatypes::id_token_info_type::IdTokenInfoType;
use rust_ocpp::v2_0_1::datatypes::id_token_type::IdTokenType;
use rust_ocpp::v2_0_1::enumerations::authorization_status_enum_type::AuthorizationStatusEnumType;
use rust_ocpp::v2_0_1::enumerations::boot_reason_enum_type::BootReasonEnumType;
use rust_ocpp::v2_0_1::enumerations::id_token_enum_type::IdTokenEnumType;
use rust_ocpp::v2_0_1::enumerations::registration_status_enum_type::RegistrationStatusEnumType;
use rust_ocpp::v2_0_1::enumerations::request_start_stop_status_enum_type::RequestStartStopStatusEnumType;
use rust_ocpp::v2_0_1::messages::boot_notification::BootNotificationResponse as BootResponse201;
use rust_ocpp::v2_0_1::messages::heartbeat::HeartbeatResponse as HeartbeatResponse201;
use rust_ocpp::v2_0_1::messages::request_start_transaction::RequestStartTransactionRequest;
use rust_ocpp::v2_0_1::messages::request_start_transaction::RequestStartTransactionResponse;
use rust_ocpp::v2_0_1::messages::request_stop_transaction::RequestStopTransactionRequest;
use rust_ocpp::v2_0_1::messages::request_stop_transaction::RequestStopTransactionResponse;
use rust_ocpp::v2_0_1::messages::transaction_event::TransactionEventResponse;
use serde::Serialize;
use serde_json::Value;

use crate::codec::call_error;
use crate::codec::Frame;

pub fn to_json<T: Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| format!("failed to encode OCPP payload: {err}"))
}

pub fn boot_result(version: OcppVersion, now: DateTime<Utc>) -> Result<Value, String> {
    match version {
        OcppVersion::V16 => to_json(&BootNotificationResponse {
            current_time: now,
            interval: 10,
            status: RegistrationStatus::Accepted,
        }),
        OcppVersion::V201 => to_json(&BootResponse201 {
            current_time: now,
            interval: 10,
            status: RegistrationStatusEnumType::Accepted,
            status_info: None,
        }),
    }
}

pub fn heartbeat_result(version: OcppVersion, now: DateTime<Utc>) -> Result<Value, String> {
    match version {
        OcppVersion::V16 => to_json(&HeartbeatResponse { current_time: now }),
        OcppVersion::V201 => to_json(&HeartbeatResponse201 { current_time: now }),
    }
}

pub fn start_result_v16(transaction_id: i64) -> Result<Value, String> {
    let transaction_id = i32::try_from(transaction_id).unwrap_or(i32::MAX);
    to_json(&StartTransactionResponse {
        transaction_id,
        id_tag_info: accepted_tag(),
    })
}

pub fn stop_result_v16() -> Result<Value, String> {
    to_json(&StopTransactionResponse {
        id_tag_info: Some(accepted_tag()),
    })
}

pub fn transaction_event_result(include_token: bool) -> Result<Value, String> {
    to_json(&TransactionEventResponse {
        total_cost: None,
        charging_priority: None,
        id_token_info: include_token.then(accepted_token_info),
        updated_personal_message: None,
    })
}

pub fn remote_start_payload(
    version: OcppVersion,
    connector_id: i64,
    id_tag: &str,
) -> Result<Value, String> {
    match version {
        OcppVersion::V16 => to_json(&RemoteStartTransactionRequest {
            connector_id: u32::try_from(connector_id).ok(),
            id_tag: id_tag.to_string(),
            charging_profile: None,
        }),
        OcppVersion::V201 => to_json(&RequestStartTransactionRequest {
            evse_id: i32::try_from(connector_id).ok(),
            remote_start_id: 1,
            id_token: id_token(id_tag),
            charging_profile: None,
            group_id_token: None,
        }),
    }
}

pub fn remote_stop_payload(version: OcppVersion, transaction_id: &str) -> Result<Value, String> {
    match version {
        OcppVersion::V16 => {
            let transaction_id = transaction_id
                .parse::<i32>()
                .map_err(|_| format!("invalid OCPP 1.6 transaction id: {transaction_id}"))?;
            to_json(&RemoteStopTransactionRequest { transaction_id })
        }
        OcppVersion::V201 => to_json(&RequestStopTransactionRequest {
            transaction_id: transaction_id.to_string(),
        }),
    }
}

pub fn remote_result(version: OcppVersion, accepted: bool) -> Result<Value, String> {
    let status_v16 = if accepted {
        RemoteStartStopStatus::Accepted
    } else {
        RemoteStartStopStatus::Rejected
    };
    let status_v201 = if accepted {
        RequestStartStopStatusEnumType::Accepted
    } else {
        RequestStartStopStatusEnumType::Rejected
    };
    match version {
        OcppVersion::V16 => to_json(
            &rust_ocpp::v1_6::messages::remote_start_transaction::RemoteStartTransactionResponse {
                status: status_v16,
            },
        ),
        OcppVersion::V201 => to_json(&RequestStartTransactionResponse {
            status: status_v201,
            transaction_id: None,
            status_info: None,
        }),
    }
}

pub fn stop_remote_result(version: OcppVersion, accepted: bool) -> Result<Value, String> {
    match version {
        OcppVersion::V16 => remote_result(version, accepted),
        OcppVersion::V201 => to_json(&RequestStopTransactionResponse {
            status: if accepted {
                RequestStartStopStatusEnumType::Accepted
            } else {
                RequestStartStopStatusEnumType::Rejected
            },
            status_info: None,
        }),
    }
}

pub fn upstream_boot(version: OcppVersion) -> Result<Value, String> {
    match version {
        OcppVersion::V16 => to_json(&BootNotificationRequest {
            charge_box_serial_number: None,
            charge_point_model: "EVProxy".into(),
            charge_point_serial_number: None,
            charge_point_vendor: "OCPPProxy".into(),
            firmware_version: None,
            iccid: None,
            imsi: None,
            meter_serial_number: None,
            meter_type: None,
        }),
        OcppVersion::V201 => to_json(
            &rust_ocpp::v2_0_1::messages::boot_notification::BootNotificationRequest {
                reason: BootReasonEnumType::PowerUp,
                charging_station: ChargingStationType {
                    serial_number: None,
                    model: "EVProxy".into(),
                    vendor_name: "OCPPProxy".into(),
                    firmware_version: None,
                    modem: None,
                },
            },
        ),
    }
}

pub fn read_boot(
    version: OcppVersion,
    payload: &Value,
) -> Result<(String, String, Option<String>), String> {
    match version {
        OcppVersion::V16 => {
            let request: BootNotificationRequest = serde_json::from_value(payload.clone())
                .map_err(|err| format!("invalid BootNotification: {err}"))?;
            Ok((
                request.charge_point_vendor,
                request.charge_point_model,
                None,
            ))
        }
        OcppVersion::V201 => {
            let request: rust_ocpp::v2_0_1::messages::boot_notification::BootNotificationRequest =
                serde_json::from_value(payload.clone())
                    .map_err(|err| format!("invalid BootNotification: {err}"))?;
            let reason = payload
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok((
                request.charging_station.vendor_name,
                request.charging_station.model,
                reason,
            ))
        }
    }
}

pub fn formation(id: &str, detail: &str) -> Frame {
    call_error(id, "FormationViolation", detail)
}

fn accepted_tag() -> IdTagInfo {
    IdTagInfo {
        expiry_date: None,
        parent_id_tag: None,
        status: AuthorizationStatus::Accepted,
    }
}

fn id_token(id_tag: &str) -> IdTokenType {
    IdTokenType {
        id_token: id_tag.to_string(),
        kind: IdTokenEnumType::ISO14443,
        additional_info: None,
    }
}

fn accepted_token_info() -> IdTokenInfoType {
    IdTokenInfoType {
        status: AuthorizationStatusEnumType::Accepted,
        cache_expiry_date_time: None,
        charging_priority: None,
        language1: None,
        evse_id: None,
        language2: None,
        group_id_token: None,
        personal_message: None,
    }
}

pub fn json_i64(payload: &Value, key: &str, default: i64) -> i64 {
    payload.get(key).and_then(json_number).unwrap_or(default)
}

pub fn json_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn json_number(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn responses_and_calls_cover_both_versions() {
        for version in [OcppVersion::V16, OcppVersion::V201] {
            let boot = boot_result(version, now()).unwrap();
            assert_eq!(boot["status"], "Accepted");
            assert_eq!(boot["interval"], 10);
            let beat = heartbeat_result(version, now()).unwrap();
            assert!(beat["currentTime"].as_str().unwrap().contains("2023"));
            let start = remote_start_payload(version, 2, "RFID").unwrap();
            let stop = remote_stop_payload(version, "4").unwrap();
            assert_eq!(remote_result(version, true).unwrap()["status"], "Accepted");
            assert_eq!(remote_result(version, false).unwrap()["status"], "Rejected");
            let boot_call = upstream_boot(version).unwrap();
            match version {
                OcppVersion::V16 => {
                    assert_eq!(start["idTag"], "RFID");
                    assert_eq!(stop["transactionId"], 4);
                    assert_eq!(boot_call["chargePointVendor"], "OCPPProxy");
                }
                OcppVersion::V201 => {
                    assert_eq!(start["idToken"]["idToken"], "RFID");
                    assert_eq!(start["evseId"], 2);
                    assert_eq!(stop["transactionId"], "4");
                    assert_eq!(boot_call["chargingStation"]["model"], "EVProxy");
                    assert_eq!(
                        stop_remote_result(version, false).unwrap()["status"],
                        "Rejected"
                    );
                }
            }
        }
        assert!(remote_stop_payload(OcppVersion::V16, "nope").is_err());
        assert_eq!(start_result_v16(1).unwrap()["transactionId"], 1);
        assert_eq!(
            stop_result_v16().unwrap()["idTagInfo"]["status"],
            "Accepted"
        );
        assert!(transaction_event_result(true)
            .unwrap()
            .get("idTokenInfo")
            .is_some());
        assert!(transaction_event_result(false)
            .unwrap()
            .get("idTokenInfo")
            .is_none());
        assert_eq!(
            start_result_v16(i64::from(i32::MAX) + 1).unwrap()["transactionId"],
            i32::MAX
        );
    }

    #[test]
    fn read_boot_and_helpers() {
        let v16 = serde_json::json!({"chargePointVendor": "V", "chargePointModel": "M"});
        let (vendor, model, reason) = read_boot(OcppVersion::V16, &v16).unwrap();
        assert_eq!((vendor, model, reason), ("V".into(), "M".into(), None));
        let v201 = serde_json::json!({
            "reason": "PowerUp",
            "chargingStation": {"vendorName": "V", "model": "M"}
        });
        let (vendor, model, reason) = read_boot(OcppVersion::V201, &v201).unwrap();
        assert_eq!(vendor, "V");
        assert_eq!(model, "M");
        assert_eq!(reason.as_deref(), Some("PowerUp"));
        assert!(read_boot(OcppVersion::V16, &serde_json::json!({})).is_err());
        assert_eq!(json_i64(&serde_json::json!({"n": "3"}), "n", 0), 3);
        assert_eq!(json_i64(&serde_json::json!({}), "n", 9), 9);
        assert_eq!(json_str(&serde_json::json!({"s": "x"}), "s"), Some("x"));
        let frame = formation("1", "bad");
        assert!(matches!(frame, Frame::Error { code, .. } if code == "FormationViolation"));
    }
}
