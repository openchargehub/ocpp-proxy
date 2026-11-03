use serde_json::Value;

/// One OCPP-J WebSocket text frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Call {
        id: String,
        action: String,
        payload: Value,
    },
    Result {
        id: String,
        payload: Value,
    },
    Error {
        id: String,
        code: String,
        description: String,
        details: Value,
    },
}

pub fn decode(text: &str) -> Result<Frame, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|err| format!("invalid OCPP-J frame: {err}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "invalid OCPP-J frame: expected a JSON array".to_string())?;
    let kind = items
        .first()
        .and_then(Value::as_i64)
        .ok_or_else(|| "invalid OCPP-J frame: missing message type".to_string())?;
    match kind {
        2 => decode_call(items),
        3 => decode_result(items),
        4 => decode_error(items),
        other => Err(format!("invalid OCPP-J frame: message type {other}")),
    }
}

pub fn encode(frame: &Frame) -> String {
    match frame {
        Frame::Call {
            id,
            action,
            payload,
        } => serde_json::json!([2, id, action, payload]).to_string(),
        Frame::Result { id, payload } => serde_json::json!([3, id, payload]).to_string(),
        Frame::Error {
            id,
            code,
            description,
            details,
        } => serde_json::json!([4, id, code, description, details]).to_string(),
    }
}

pub fn call_error(id: &str, code: &str, description: &str) -> Frame {
    Frame::Error {
        id: id.to_string(),
        code: code.to_string(),
        description: description.to_string(),
        details: serde_json::json!({}),
    }
}

fn decode_call(items: &[Value]) -> Result<Frame, String> {
    if items.len() != 4 {
        return Err(format!(
            "invalid OCPP-J CALL: expected 4 items, got {}",
            items.len()
        ));
    }
    Ok(Frame::Call {
        id: expect_str(&items[1], "unique id")?,
        action: expect_str(&items[2], "action")?,
        payload: items[3].clone(),
    })
}

fn decode_result(items: &[Value]) -> Result<Frame, String> {
    if items.len() != 3 {
        return Err(format!(
            "invalid OCPP-J CALLRESULT: expected 3 items, got {}",
            items.len()
        ));
    }
    Ok(Frame::Result {
        id: expect_str(&items[1], "unique id")?,
        payload: items[2].clone(),
    })
}

fn decode_error(items: &[Value]) -> Result<Frame, String> {
    if items.len() != 5 {
        return Err(format!(
            "invalid OCPP-J CALLERROR: expected 5 items, got {}",
            items.len()
        ));
    }
    Ok(Frame::Error {
        id: expect_str(&items[1], "unique id")?,
        code: expect_str(&items[2], "error code")?,
        description: expect_str(&items[3], "error description")?,
        details: items[4].clone(),
    })
}

fn expect_str(value: &Value, field: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("invalid OCPP-J frame: {field} must be a string, got {value}"))
}

pub fn status_accepted(payload: &Value) -> bool {
    payload.get("status").and_then(Value::as_str) == Some("Accepted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_ocpp::v1_6::messages::boot_notification::BootNotificationRequest;
    use rust_ocpp::v1_6::messages::remote_stop_transaction::RemoteStopTransactionRequest;
    use rust_ocpp::v2_0_1::datatypes::id_token_type::IdTokenType;
    use rust_ocpp::v2_0_1::enumerations::id_token_enum_type::IdTokenEnumType;
    use rust_ocpp::v2_0_1::messages::request_start_transaction::RequestStartTransactionRequest;
    use rust_ocpp::v2_0_1::messages::request_stop_transaction::RequestStopTransactionRequest;

    fn boot_request() -> BootNotificationRequest {
        BootNotificationRequest {
            charge_box_serial_number: None,
            charge_point_model: "EVProxy".into(),
            charge_point_serial_number: None,
            charge_point_vendor: "OCPPProxy".into(),
            firmware_version: None,
            iccid: None,
            imsi: None,
            meter_serial_number: None,
            meter_type: None,
        }
    }

    #[test]
    fn call_result_and_error_round_trip() {
        let payload = serde_json::to_value(boot_request()).unwrap();
        let call = Frame::Call {
            id: "boot-1".into(),
            action: "BootNotification".into(),
            payload: payload.clone(),
        };
        let decoded = decode(&encode(&call)).unwrap();
        assert_eq!(decoded, call);
        let Frame::Call { payload: back, .. } = decoded else {
            panic!("call")
        };
        let parsed: BootNotificationRequest = serde_json::from_value(back).unwrap();
        assert_eq!(parsed.charge_point_vendor, "OCPPProxy");
        assert_eq!(parsed.charge_point_model, "EVProxy");

        let result = Frame::Result {
            id: "boot-1".into(),
            payload: serde_json::json!({"status": "Accepted", "interval": 10}),
        };
        assert_eq!(decode(&encode(&result)).unwrap(), result);

        let error = call_error("9", "FormationViolation", "bad payload");
        let text = encode(&error);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value[0], 4);
        assert_eq!(value[1], "9");
        assert_eq!(value[2], "FormationViolation");
        assert_eq!(value[3], "bad payload");
        assert_eq!(decode(&text).unwrap(), error);
    }

    #[test]
    fn malformed_frames_name_the_problem() {
        assert!(decode("not-json")
            .unwrap_err()
            .contains("invalid OCPP-J frame"));
        assert!(decode("[1, \"x\"]").unwrap_err().contains("message type 1"));
        assert!(decode("[2, 1, \"BootNotification\", {}]")
            .unwrap_err()
            .contains("unique id"));
        assert!(decode("[2, \"1\", \"BootNotification\"]")
            .unwrap_err()
            .contains("4 items"));
        assert!(decode("[3, \"1\"]").unwrap_err().contains("3 items"));
        assert!(decode("[4, \"1\", \"Code\"]")
            .unwrap_err()
            .contains("5 items"));
    }

    #[test]
    fn remote_start_and_stop_round_trip_both_versions() {
        let start = RequestStartTransactionRequest {
            evse_id: Some(1),
            remote_start_id: 1,
            id_token: IdTokenType {
                id_token: "RFID".into(),
                kind: IdTokenEnumType::ISO14443,
                additional_info: None,
            },
            charging_profile: None,
            group_id_token: None,
        };
        let payload = serde_json::to_value(&start).unwrap();
        assert_eq!(payload["idToken"]["idToken"], "RFID");
        assert_eq!(payload["evseId"], 1);
        let back: RequestStartTransactionRequest = serde_json::from_value(payload).unwrap();
        assert_eq!(back.id_token.id_token, "RFID");

        let stop = RequestStopTransactionRequest {
            transaction_id: "tx-9".into(),
        };
        let payload = serde_json::to_value(&stop).unwrap();
        assert_eq!(payload["transactionId"], "tx-9");
        let v16 = RemoteStopTransactionRequest { transaction_id: 9 };
        let payload = serde_json::to_value(&v16).unwrap();
        assert_eq!(payload["transactionId"], 9);
        assert!(status_accepted(&serde_json::json!({"status": "Accepted"})));
        assert!(!status_accepted(&serde_json::json!({"status": "Rejected"})));
    }
}
