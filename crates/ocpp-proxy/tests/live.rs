use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ocpp_proxy::{spawn, ProxyOptions, RunningProxy};
use ocpp_proxy_domain::ProxyConfig;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_hdr_async, connect_async, WebSocketStream};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ocpp-live-{name}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn boot_proxy(yaml: &str) -> RunningProxy {
    boot_proxy_ha(yaml, None, None).await
}

async fn boot_proxy_ha(
    yaml: &str,
    ha_url: Option<String>,
    ha_token: Option<String>,
) -> RunningProxy {
    let config = ProxyConfig::from_document(yaml).unwrap();
    spawn(ProxyOptions {
        config,
        db_path: temp_path("db"),
        bind: "127.0.0.1:0".parse().unwrap(),
        lock_timeout: Duration::from_millis(200),
        ha_url,
        ha_token,
    })
    .await
    .unwrap()
}

fn shared_yaml() -> &'static str {
    "allow_shared_charging: true\nrate_limit_seconds: 30\npreferred_provider: preferred\n"
}

async fn connect(
    addr: SocketAddr,
    path: &str,
    protocol: Option<&str>,
    extra: &[(&str, &str)],
) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}{path}");
    let mut request = url.into_client_request().unwrap();
    if let Some(protocol) = protocol {
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(protocol).unwrap(),
        );
    }
    for (name, value) in extra {
        let name = HeaderName::from_bytes(name.as_bytes()).unwrap();
        let value = HeaderValue::from_str(value).unwrap();
        request.headers_mut().insert(name, value);
    }
    let (socket, _) = connect_async(request).await.unwrap();
    socket
}

async fn text_from(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> String {
    timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return text.to_string(),
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                Message::Close(_) => panic!("socket closed"),
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for a text frame")
}

fn call(id: &str, action: &str, payload: Value) -> String {
    json!([2, id, action, payload]).to_string()
}

async fn rpc(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    id: &str,
    action: &str,
    payload: Value,
) -> Value {
    socket
        .send(Message::Text(call(id, action, payload).into()))
        .await
        .unwrap();
    let text = text_from(socket).await;
    serde_json::from_str(&text).unwrap()
}

async fn get(url: &str) -> reqwest::Response {
    reqwest::Client::new().get(url).send().await.unwrap()
}

#[tokio::test]
async fn rest_documents_sessions_status_and_override() {
    let proxy = boot_proxy(shared_yaml()).await;
    let base = format!("http://{}", proxy.addr);
    let home = get(&format!("{base}/")).await;
    assert_eq!(home.status(), 200);
    let html = home.text().await.unwrap();
    for path in [
        "/charger",
        "/backend",
        "/sessions",
        "/sessions.csv",
        "/status",
        "/override",
    ] {
        assert!(html.contains(path), "{path} missing from welcome page");
    }
    assert_eq!(
        get(&format!("{base}/sessions"))
            .await
            .json::<Value>()
            .await
            .unwrap(),
        json!([])
    );
    let csv = get(&format!("{base}/sessions.csv"))
        .await
        .text()
        .await
        .unwrap();
    assert!(csv.starts_with("timestamp,backend_id,duration_s,energy_kwh,revenue\n"));
    let status = get(&format!("{base}/status"))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(status["websocket_backends"], json!([]));
    assert!(status["lock_owner"].is_null());
    assert_eq!(status["ocpp_services"], json!({}));

    let client = reqwest::Client::new();
    let bad = client
        .post(format!("{base}/override"))
        .body("not-json")
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert_eq!(bad.text().await.unwrap(), "Invalid JSON");
    let missing = client
        .post(format!("{base}/override"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 400);
    let number = client
        .post(format!("{base}/override"))
        .json(&json!({"backend_id": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(number.status(), 400);
    let ok = client
        .post(format!("{base}/override"))
        .json(&json!({"backend_id": "alpha"}))
        .send()
        .await
        .unwrap();
    let body: Value = ok.json().await.unwrap();
    assert_eq!(body["success"], true);
    assert_eq!(body["owner"], "alpha");

    let blocked = boot_proxy("allow_shared_charging: false\n").await;
    let denied = reqwest::Client::new()
        .post(format!("http://{}/override", blocked.addr))
        .json(&json!({"backend_id": "alpha"}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(denied["success"], false);
    assert!(denied["owner"].is_null());
    proxy.shutdown().await;
    blocked.shutdown().await;
}

#[tokio::test]
async fn charger_and_backends_exchange_ocpp_and_json() {
    let proxy = boot_proxy(shared_yaml()).await;
    let mut backend = connect(proxy.addr, "/backend?id=alpha", None, &[]).await;
    let mut charger = connect(proxy.addr, "/charger", Some("ocpp1.6"), &[]).await;
    let boot = rpc(
        &mut charger,
        "b1",
        "BootNotification",
        json!({"chargePointVendor": "Vendor", "chargePointModel": "Box"}),
    )
    .await;
    assert_eq!(boot[0], 3);
    assert_eq!(boot[2]["status"], "Accepted");
    assert_eq!(boot[2]["interval"], 10);
    let event: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(event["type"], "boot");
    assert_eq!(event["vendor"], "Vendor");
    assert_eq!(event["model"], "Box");

    let beat = rpc(&mut charger, "h1", "Heartbeat", json!({})).await;
    assert!(beat[2]["currentTime"].as_str().unwrap().contains("T"));
    let heartbeat: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(heartbeat["type"], "heartbeat");

    let _ = rpc(
        &mut charger,
        "s1",
        "StatusNotification",
        json!({"connectorId": 1, "errorCode": "NoError", "status": "Available"}),
    )
    .await;
    let status: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(status["type"], "status");
    assert_eq!(status["status"], "Available");

    let _ = rpc(
        &mut charger,
        "m1",
        "MeterValues",
        json!({"connectorId": 1, "meterValue": [{"value": 10}]}),
    )
    .await;
    let meter: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(meter["type"], "meter");
    assert_eq!(meter["values"][0]["value"], 10);

    let started = rpc(&mut charger, "t1", "StartTransaction", json!({"connectorId": 1, "idTag": "RFID", "meterStart": 1000, "timestamp": "2023-01-01T12:00:00Z"})).await;
    assert_eq!(started[2]["transactionId"], 1);
    assert_eq!(started[2]["idTagInfo"]["status"], "Accepted");
    let started_event: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(started_event["type"], "transaction_started");

    let stopped = rpc(
        &mut charger,
        "t2",
        "StopTransaction",
        json!({"transactionId": 1, "meterStop": 6000, "timestamp": "2023-01-01T12:30:00Z"}),
    )
    .await;
    assert_eq!(stopped[2]["idTagInfo"]["status"], "Accepted");
    let stopped_event: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(stopped_event["type"], "transaction_stopped");
    let unknown = rpc(
        &mut charger,
        "t3",
        "StopTransaction",
        json!({"transactionId": 99, "meterStop": 1, "timestamp": "2023-01-01T12:40:00Z"}),
    )
    .await;
    assert_eq!(unknown[2]["idTagInfo"]["status"], "Accepted");
    let _unknown_event = text_from(&mut backend).await;

    let sessions = get(&format!("http://{}/sessions", proxy.addr))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(sessions[0]["energy_kwh"], 5.0);
    assert_eq!(sessions[0]["duration_s"], 1800.0);
    assert_eq!(sessions[0]["revenue"], 0.0);
    assert_eq!(sessions.as_array().unwrap().len(), 1);

    backend
        .send(Message::Text(json!({"action": "Nope"}).to_string().into()))
        .await
        .unwrap();
    let reply: Value = serde_json::from_str(&text_from(&mut backend).await).unwrap();
    assert_eq!(reply["error"], "unknown_action");
    proxy.shutdown().await;
}

#[tokio::test]
async fn remote_start_lock_rate_limit_preferred_and_stop() {
    let proxy = boot_proxy(shared_yaml()).await;
    let mut charger = connect(proxy.addr, "/charger", Some("ocpp1.6"), &[]).await;
    let mut alpha = connect(proxy.addr, "/backend?id=alpha", None, &[]).await;
    let mut beta = connect(proxy.addr, "/backend?id=beta", None, &[]).await;
    alpha
        .send(Message::Text(
            json!({"action": "RemoteStartTransaction", "connector_id": 1, "id_tag": "RFID"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let outbound = text_from(&mut charger).await;
    let frame: Value = serde_json::from_str(&outbound).unwrap();
    assert_eq!(frame[2], "RemoteStartTransaction");
    assert_eq!(frame[3]["connectorId"], 1);
    assert_eq!(frame[3]["idTag"], "RFID");
    charger
        .send(Message::Text(
            json!([3, frame[1], {"status": "Accepted"}])
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let reply: Value = serde_json::from_str(&text_from(&mut alpha).await).unwrap();
    assert_eq!(reply["result"]["status"], "Accepted");

    beta.send(Message::Text(
        json!({"action": "RemoteStartTransaction", "connector_id": 1, "id_tag": "OTHER"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let denied: Value = serde_json::from_str(&text_from(&mut beta).await).unwrap();
    assert_eq!(denied["error"], "control_locked");

    alpha
        .send(Message::Text(
            json!({"action": "RemoteStartTransaction", "connector_id": 1, "id_tag": "AGAIN"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let limited: Value = serde_json::from_str(&text_from(&mut alpha).await).unwrap();
    assert_eq!(limited["error"], "control_locked");

    let mut preferred = connect(proxy.addr, "/backend?id=preferred", None, &[]).await;
    preferred
        .send(Message::Text(
            json!({"action": "RemoteStartTransaction", "connector_id": 2, "id_tag": "PREF"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let stolen = text_from(&mut charger).await;
    let stolen: Value = serde_json::from_str(&stolen).unwrap();
    assert_eq!(stolen[3]["idTag"], "PREF");
    charger
        .send(Message::Text(
            json!([3, stolen[1], {"status": "Rejected"}])
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let preferred_reply: Value = serde_json::from_str(&text_from(&mut preferred).await).unwrap();
    assert_eq!(preferred_reply["result"]["status"], "Rejected");
    let status = get(&format!("http://{}/status", proxy.addr))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert!(status["lock_owner"].is_null());

    alpha
        .send(Message::Text(
            json!({"action": "RemoteStopTransaction", "transaction_id": 7})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let stop = text_from(&mut charger).await;
    let stop: Value = serde_json::from_str(&stop).unwrap();
    assert_eq!(stop[2], "RemoteStopTransaction");
    assert_eq!(stop[3]["transactionId"], 7);
    charger
        .send(Message::Text(
            json!([3, stop[1], {"status": "Accepted"}])
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let stop_reply: Value = serde_json::from_str(&text_from(&mut alpha).await).unwrap();
    assert_eq!(stop_reply["result"]["status"], "Accepted");
    proxy.shutdown().await;
}

#[tokio::test]
async fn missing_charger_default_backend_and_fault_release() {
    let ha = ha_server().await;
    let proxy = boot_proxy_ha(
        shared_yaml(),
        Some(format!("http://{ha}")),
        Some("token".into()),
    )
    .await;
    let mut lonely = connect(proxy.addr, "/backend", None, &[]).await;
    lonely
        .send(Message::Text(
            json!({"action": "RemoteStartTransaction"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let reply: Value = serde_json::from_str(&text_from(&mut lonely).await).unwrap();
    assert_eq!(reply["error"], "unknown_action");
    let status = get(&format!("http://{}/status", proxy.addr))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(status["websocket_backends"], json!(["unknown"]));
    drop(lonely);

    let mut charger = connect(proxy.addr, "/charger", Some("ocpp1.6"), &[]).await;
    let mut owner = connect(proxy.addr, "/backend?id=alpha", None, &[]).await;
    owner
        .send(Message::Text(
            json!({"action": "RemoteStartTransaction", "id_tag": "RFID"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let frame: Value = serde_json::from_str(&text_from(&mut charger).await).unwrap();
    charger
        .send(Message::Text(
            json!([3, frame[1], {"status": "Accepted"}])
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let _ = text_from(&mut owner).await;
    let _ = rpc(
        &mut charger,
        "f1",
        "StatusNotification",
        json!({"connectorId": 1, "errorCode": "GroundFailure", "status": "Faulted"}),
    )
    .await;
    let status = get(&format!("http://{}/status", proxy.addr))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert!(status["lock_owner"].is_null());
    let notes = ha_notes();
    assert!(notes
        .lock()
        .unwrap()
        .iter()
        .any(|note| note.contains("Charger Fault") && note.contains("GroundFailure")));
    proxy.shutdown().await;
}

fn ha_notes() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    HA_NOTES
        .get_or_init(|| std::sync::Arc::new(std::sync::Mutex::new(Vec::new())))
        .clone()
}

static HA_NOTES: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Vec<String>>>> =
    std::sync::OnceLock::new();

async fn ha_server() -> SocketAddr {
    let notes = ha_notes();
    notes.lock().unwrap().clear();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let notes = notes.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0_u8; 4096];
            let Ok(n) = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await else {
                break;
            };
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            if request.contains("persistent_notification") {
                notes.lock().unwrap().push(request.clone());
            }
            let body = if request.contains("/api/states/") {
                r#"{"state":"not_home"}"#
            } else {
                "[]"
            };
            let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        }
    });
    addr
}

#[tokio::test]
async fn version_detection_uses_subprotocol_header_path_and_query() {
    let proxy = boot_proxy(
        "allow_shared_charging: true\nauto_detect_ocpp_version: true\nocpp_version: \"1.6\"\n",
    )
    .await;
    let mut by_protocol = connect(proxy.addr, "/charger", Some("ocpp2.0.1"), &[]).await;
    let boot = rpc(
        &mut by_protocol,
        "1",
        "BootNotification",
        json!({"reason": "PowerUp", "chargingStation": {"vendorName": "V", "model": "M"}}),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");
    assert_eq!(boot[2]["interval"], 10);
    let started = rpc(
        &mut by_protocol,
        "e1",
        "TransactionEvent",
        json!({
            "eventType": "Started",
            "timestamp": "2023-01-01T12:00:00Z",
            "triggerReason": "RemoteStart",
            "seqNo": 0,
            "transactionInfo": {"transactionId": "tx-1"},
            "idToken": {"idToken": "RFID", "type": "ISO14443"},
            "evse": {"id": 1},
            "meterValue": [{"sampledValue": [{"value": 1000}]}]
        }),
    )
    .await;
    assert_eq!(started[0], 3);
    let ended = rpc(
        &mut by_protocol,
        "e2",
        "TransactionEvent",
        json!({
            "eventType": "Ended",
            "timestamp": "2023-01-01T12:30:00Z",
            "triggerReason": "RemoteStop",
            "seqNo": 1,
            "transactionInfo": {"transactionId": "tx-1"},
            "meterValue": [{"sampledValue": [{"value": 6000}]}]
        }),
    )
    .await;
    assert_eq!(ended[0], 3);
    let updated = rpc(
        &mut by_protocol,
        "e3",
        "TransactionEvent",
        json!({
            "eventType": "Updated",
            "timestamp": "2023-01-01T12:15:00Z",
            "triggerReason": "MeterValuePeriodic",
            "seqNo": 2,
            "transactionInfo": {"transactionId": "tx-1"}
        }),
    )
    .await;
    assert_eq!(updated[0], 3);
    let _ = rpc(&mut by_protocol, "st", "StatusNotification", json!({"timestamp": "2023-01-01T12:00:00Z", "connectorStatus": "Faulted", "evseId": 1, "connectorId": 1})).await;
    let _ = rpc(
        &mut by_protocol,
        "mv",
        "MeterValues",
        json!({"evseId": 1, "meterValue": [{"value": 1}]}),
    )
    .await;

    let mut by_header = connect(proxy.addr, "/charger", None, &[("X-OCPP-Version", "2.0.1")]).await;
    let boot = rpc(
        &mut by_header,
        "1",
        "BootNotification",
        json!({"reason": "PowerUp", "chargingStation": {"vendorName": "V", "model": "M"}}),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");

    let mut by_path = connect(proxy.addr, "/charger/v2.0.1", None, &[]).await;
    let boot = rpc(
        &mut by_path,
        "1",
        "BootNotification",
        json!({"reason": "PowerUp", "chargingStation": {"vendorName": "V", "model": "M"}}),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");

    let mut by_query = connect(proxy.addr, "/charger?version=2.0", None, &[]).await;
    let boot = rpc(
        &mut by_query,
        "1",
        "BootNotification",
        json!({"reason": "PowerUp", "chargingStation": {"vendorName": "V", "model": "M"}}),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");

    let fixed = boot_proxy(
        "allow_shared_charging: true\nauto_detect_ocpp_version: false\nocpp_version: \"1.6\"\n",
    )
    .await;
    let mut forced = connect(
        fixed.addr,
        "/charger?version=2.0.1",
        Some("ocpp1.6, ocpp2.0.1"),
        &[],
    )
    .await;
    let boot = rpc(
        &mut forced,
        "1",
        "BootNotification",
        json!({"chargePointVendor": "Vendor", "chargePointModel": "Box"}),
    )
    .await;
    assert_eq!(boot[2]["status"], "Accepted");
    proxy.shutdown().await;
    fixed.shutdown().await;
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn upstream_speaks_ocpp_and_a_dead_url_does_not_stop_the_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let csms_addr = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_task = seen.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut headers = None;
        let socket = accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::http::Request<()>,
             mut response: tokio_tungstenite::tungstenite::http::Response<()>| {
                headers = Some(request.headers().clone());
                if let Some(protocol) = request.headers().get("sec-websocket-protocol") {
                    response
                        .headers_mut()
                        .insert("sec-websocket-protocol", protocol.clone());
                }
                Ok(response)
            },
        )
        .await
        .unwrap();
        let headers = headers.unwrap();
        assert_eq!(headers.get("sec-websocket-protocol").unwrap(), "ocpp1.6");
        let auth = headers.get("authorization").unwrap().to_str().unwrap();
        assert!(auth.starts_with("Basic "));
        let (mut write, mut read) = socket.split();
        while let Some(Ok(Message::Text(text))) = read.next().await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame[0] == 2 {
                seen_task
                    .lock()
                    .unwrap()
                    .push(frame[2].as_str().unwrap().to_string());
                let reply = json!([3, frame[1], {"status": "Accepted", "currentTime": "2023-01-01T00:00:00Z", "interval": 10}]).to_string();
                write.send(Message::Text(reply.into())).await.unwrap();
            }
        }
    });
    let yaml = format!(
        "allow_shared_charging: true\nrate_limit_seconds: 1\nocpp_services:\n  - id: grid\n    url: ws://{csms_addr}\n    version: \"1.6\"\n    auth_type: basic\n    username: user\n    password: pass\n    enabled: true\n  - id: off\n    url: ws://127.0.0.1:1\n    enabled: false\n  - id: blank\n    url: \"\"\n    enabled: true\n  - id: down\n    url: ws://127.0.0.1:1\n    enabled: true\n"
    );
    let proxy = boot_proxy(&yaml).await;
    let home = get(&format!("http://{}/", proxy.addr)).await;
    assert_eq!(home.status(), 200);
    timeout(Duration::from_secs(2), async {
        loop {
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|action| action == "BootNotification")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("upstream boot");
    let mut charger = connect(proxy.addr, "/charger", Some("ocpp1.6"), &[]).await;
    let _ = rpc(
        &mut charger,
        "b",
        "BootNotification",
        json!({"chargePointVendor": "V", "chargePointModel": "M"}),
    )
    .await;
    let _ = rpc(
        &mut charger,
        "s",
        "StatusNotification",
        json!({"connectorId": 1, "errorCode": "NoError", "status": "Charging"}),
    )
    .await;
    let _ = rpc(&mut charger, "h", "Heartbeat", json!({})).await;
    let _ = rpc(
        &mut charger,
        "m",
        "MeterValues",
        json!({"connectorId": 1, "meterValue": [{"value": 4}]}),
    )
    .await;
    let _ = rpc(&mut charger, "a", "StartTransaction", json!({"connectorId": 1, "idTag": "RFID", "meterStart": 0, "timestamp": "2023-01-01T12:00:00Z"})).await;
    let _ = rpc(
        &mut charger,
        "b2",
        "StopTransaction",
        json!({"transactionId": 1, "meterStop": 10, "timestamp": "2023-01-01T12:01:00Z"}),
    )
    .await;
    timeout(Duration::from_secs(2), async {
        loop {
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|action| action == "StopTransaction")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let boots = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|action| action.as_str() == "BootNotification")
        .count();
    assert_eq!(boots, 1);
    let status = get(&format!("http://{}/status", proxy.addr))
        .await
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(status["ocpp_services"]["grid"]["connected"], true);
    assert_eq!(status["ocpp_services"]["grid"]["authenticated"], true);
    assert_eq!(status["ocpp_services"]["grid"]["version"], "1.6");
    assert!(status["ocpp_services"].get("off").is_none());
    assert!(status["ocpp_services"].get("blank").is_none());
    proxy.shutdown().await;
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn upstream_remote_start_reaches_the_charger() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let csms_addr = listener.local_addr().unwrap();
    let (go_tx, mut go_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let socket = accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::http::Request<()>,
             mut response: tokio_tungstenite::tungstenite::http::Response<()>| {
                if let Some(protocol) = request.headers().get("sec-websocket-protocol") {
                    response
                        .headers_mut()
                        .insert("sec-websocket-protocol", protocol.clone());
                }
                assert!(request
                    .headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("bearer-token"));
                Ok(response)
            },
        )
        .await
        .unwrap();
        let (mut write, mut read) = socket.split();
        let mut ready = false;
        loop {
            tokio::select! {
                incoming = read.next() => {
                    let Some(Ok(Message::Text(text))) = incoming else { break };
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame[0] != 2 {
                        continue;
                    }
                    let reply = json!([3, frame[1], {"status": "Accepted", "currentTime": "2023-01-01T00:00:00Z", "interval": 10}]).to_string();
                    write.send(Message::Text(reply.into())).await.unwrap();
                    if frame[2] == "BootNotification" {
                        ready = true;
                    }
                }
                _ = &mut go_rx, if ready => {
                    let remote = json!([2, "up-1", "RemoteStartTransaction", {"connectorId": 1, "idTag": "UP"}]).to_string();
                    write.send(Message::Text(remote.into())).await.unwrap();
                    break;
                }
            }
        }
        while let Some(Ok(Message::Text(text))) = read.next().await {
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame[0] == 2 {
                let reply = json!([3, frame[1], {"status": "Accepted"}]).to_string();
                write.send(Message::Text(reply.into())).await.unwrap();
            }
        }
    });
    let yaml = format!(
        "allow_shared_charging: true\nrate_limit_seconds: 1\nocpp_services:\n  - id: grid\n    url: ws://{csms_addr}\n    auth_type: token\n    token: bearer-token\n    enabled: true\n"
    );
    let proxy = boot_proxy(&yaml).await;
    let mut charger = connect(proxy.addr, "/charger", Some("ocpp1.6"), &[]).await;
    go_tx.send(()).unwrap();
    let frame: Value = serde_json::from_str(&text_from(&mut charger).await).unwrap();
    assert_eq!(frame[2], "RemoteStartTransaction");
    assert_eq!(frame[3]["idTag"], "UP");
    charger
        .send(Message::Text(
            json!([3, frame[1], {"status": "Accepted"}])
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    proxy.shutdown().await;
}
