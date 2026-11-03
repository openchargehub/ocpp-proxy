use serde_json::Value;

/// Home Assistant REST and WebSocket client. One HTTP client is reused.
#[derive(Clone, Debug)]
pub struct HaClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl HaClient {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            base: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn websocket_url(&self) -> String {
        websocket_url(&self.base)
    }

    pub async fn entity_state(&self, entity_id: &str) -> Result<String, String> {
        let body = self.get_state(entity_id).await?;
        Ok(body
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    pub async fn get_state(&self, entity_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/states/{entity_id}", self.base);
        let response = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|err| format!("Home Assistant state request for {entity_id} failed: {err}"))?;
        response
            .json()
            .await
            .map_err(|err| format!("Home Assistant state body for {entity_id} was not JSON: {err}"))
    }

    pub async fn notify(&self, title: &str, message: &str) -> Result<Value, String> {
        let url = format!("{}/api/services/persistent_notification/create", self.base);
        let response = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&serde_json::json!({"title": title, "message": message}))
            .send()
            .await
            .map_err(|err| format!("Home Assistant notification failed: {err}"))?;
        response
            .json()
            .await
            .map_err(|err| format!("Home Assistant notification body was not JSON: {err}"))
    }

    pub async fn connect_websocket(&self) -> Result<(), String> {
        let (mut socket, _) = tokio_tungstenite::connect_async(self.websocket_url())
            .await
            .map_err(|err| format!("Home Assistant websocket connect failed: {err}"))?;
        use futures_util::{SinkExt, StreamExt};
        let _hello = socket.next().await;
        let auth = serde_json::json!({"type": "auth", "access_token": self.token}).to_string();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(auth.into()))
            .await
            .map_err(|err| format!("Home Assistant auth send failed: {err}"))?;
        let reply = socket
            .next()
            .await
            .ok_or_else(|| "Home Assistant closed during auth".to_string())?
            .map_err(|err| format!("Home Assistant auth read failed: {err}"))?;
        let text = match reply {
            tokio_tungstenite::tungstenite::Message::Text(text) => text.to_string(),
            other => return Err(format!("Home Assistant auth reply was not text: {other:?}")),
        };
        let body: Value = serde_json::from_str(&text)
            .map_err(|err| format!("Home Assistant auth reply was not JSON: {err}; body {text}"))?;
        if body.get("type").and_then(Value::as_str) != Some("auth_ok") {
            return Err(format!("Home Assistant authentication failed: {text}"));
        }
        Ok(())
    }
}

pub fn websocket_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    let root = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else {
        format!("ws://{trimmed}")
    };
    format!("{root}/api/websocket")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[test]
    fn websocket_urls_follow_the_http_scheme() {
        assert_eq!(
            websocket_url("http://ha.local:8123/"),
            "ws://ha.local:8123/api/websocket"
        );
        assert_eq!(
            websocket_url("https://ha.local"),
            "wss://ha.local/api/websocket"
        );
        assert_eq!(
            websocket_url("ws://ha.local"),
            "ws://ha.local/api/websocket"
        );
        assert_eq!(
            websocket_url("wss://ha.local"),
            "wss://ha.local/api/websocket"
        );
        assert_eq!(websocket_url("ha.local"), "ws://ha.local/api/websocket");
    }

    #[tokio::test]
    async fn state_and_notification_use_the_bearer_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0_u8; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let lower = request.to_ascii_lowercase();
                assert!(lower.contains("authorization: bearer token-1"), "{request}");
                let body = if request.contains("GET /api/states/") {
                    r#"{"state":"home","attributes":{"friendly_name":"Presence"}}"#
                } else {
                    r#"[{"title":"Charger Fault"}]"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = HaClient::new(&format!("http://{addr}/"), "token-1");
        assert_eq!(client.base(), format!("http://{addr}"));
        let state = client.get_state("binary_sensor.presence").await.unwrap();
        assert_eq!(state["state"], "home");
        assert_eq!(
            client.entity_state("binary_sensor.presence").await.unwrap(),
            "home"
        );
        let note = client
            .notify("Charger Fault", "Status=Faulted")
            .await
            .unwrap();
        assert!(note.to_string().contains("Charger Fault"));
    }

    #[tokio::test]
    async fn missing_state_and_connection_errors_are_distinct() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0_u8; 1024];
            let _ = socket.read(&mut buf).await;
            let body = r#"{"message":"not found"}"#;
            let response = format!(
                "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = HaClient::new(&format!("http://{addr}"), "token");
        assert_eq!(client.entity_state("missing.entity").await.unwrap(), "");
        let down = HaClient::new("http://127.0.0.1:1", "token");
        let err = down.get_state("sensor.x").await.unwrap_err();
        assert!(err.contains("sensor.x"));
    }

    #[tokio::test]
    async fn websocket_auth_accepts_only_auth_ok() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    r#"{"type":"auth_required"}"#.into(),
                ))
                .await
                .unwrap();
            let message = socket
                .next()
                .await
                .unwrap()
                .unwrap()
                .to_text()
                .unwrap()
                .to_string();
            assert!(message.contains("token-ok"));
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    r#"{"type":"auth_ok"}"#.into(),
                ))
                .await
                .unwrap();
        });
        let client = HaClient::new(&format!("http://{addr}"), "token-ok");
        client.connect_websocket().await.unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    r#"{"type":"auth_required"}"#.into(),
                ))
                .await
                .unwrap();
            let _ = socket.next().await;
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    r#"{"type":"auth_invalid"}"#.into(),
                ))
                .await
                .unwrap();
        });
        let client = HaClient::new(&format!("http://{addr}"), "nope");
        let err = client.connect_websocket().await.unwrap_err();
        assert!(err.contains("auth_invalid"));
    }
}
