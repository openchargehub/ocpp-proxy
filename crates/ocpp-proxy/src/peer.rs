use std::collections::HashMap;

use async_trait::async_trait;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungClose;
use tokio_tungstenite::tungstenite::Message as TungMessage;
use tokio_util::sync::CancellationToken;

use crate::codec::{decode, encode, Frame};

pub struct InboundCall {
    pub id: String,
    pub action: String,
    pub payload: Value,
}

pub enum Outbound {
    Call {
        action: String,
        payload: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Result {
        id: String,
        payload: Value,
    },
    Error {
        id: String,
        code: String,
        description: String,
    },
}

#[async_trait]
pub trait WsIo: Send {
    async fn recv_text(&mut self) -> Option<Result<String, String>>;
    async fn send_text(&mut self, text: String) -> Result<(), String>;
    async fn close_away(&mut self) -> Result<(), String>;
}

pub struct AxumIo {
    pub socket: WebSocket,
}

#[async_trait]
impl WsIo for AxumIo {
    async fn recv_text(&mut self) -> Option<Result<String, String>> {
        loop {
            match self.socket.recv().await? {
                Ok(Message::Text(text)) => return Some(Ok(text.to_string())),
                Ok(Message::Ping(data)) => {
                    if self.socket.send(Message::Pong(data)).await.is_err() {
                        return Some(Err("failed to pong charger".into()));
                    }
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn send_text(&mut self, text: String) -> Result<(), String> {
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|err| format!("failed to send OCPP frame: {err}"))
    }

    async fn close_away(&mut self) -> Result<(), String> {
        self.socket
            .send(Message::Close(Some(CloseFrame {
                code: 1001,
                reason: "".into(),
            })))
            .await
            .map_err(|err| format!("failed to close websocket: {err}"))
    }
}

pub struct TungsteniteIo<S>
where
    S: futures_util::Stream<Item = Result<TungMessage, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<TungMessage>
        + Unpin
        + Send,
{
    pub socket: S,
}

#[async_trait]
impl<S> WsIo for TungsteniteIo<S>
where
    S: futures_util::Stream<Item = Result<TungMessage, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<TungMessage, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin
        + Send,
{
    async fn recv_text(&mut self) -> Option<Result<String, String>> {
        loop {
            match self.socket.next().await? {
                Ok(TungMessage::Text(text)) => return Some(Ok(text.to_string())),
                Ok(TungMessage::Ping(data)) => {
                    if self.socket.send(TungMessage::Pong(data)).await.is_err() {
                        return Some(Err("failed to pong upstream".into()));
                    }
                }
                Ok(TungMessage::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn send_text(&mut self, text: String) -> Result<(), String> {
        self.socket
            .send(TungMessage::Text(text.into()))
            .await
            .map_err(|err| format!("failed to send OCPP frame: {err}"))
    }

    async fn close_away(&mut self) -> Result<(), String> {
        self.socket
            .send(TungMessage::Close(Some(TungClose {
                code: 1001.into(),
                reason: "".into(),
            })))
            .await
            .map_err(|err| format!("failed to close websocket: {err}"))
    }
}

pub async fn run_peer(
    mut io: impl WsIo,
    mut outbound: mpsc::UnboundedReceiver<Outbound>,
    inbound: mpsc::UnboundedSender<InboundCall>,
    cancel: CancellationToken,
) {
    let mut pending: HashMap<String, oneshot::Sender<Result<Value, String>>> = HashMap::new();
    let mut next_id = 1_u64;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            incoming = io.recv_text() => {
                match incoming {
                    Some(Ok(text)) => dispatch_text(&text, &mut pending, &inbound),
                    Some(Err(err)) => {
                        tracing::debug!(error = %err, "websocket read failed");
                        break;
                    }
                    None => break,
                }
            }
            command = outbound.recv() => {
                let Some(command) = command else { break };
                if handle_outbound(&mut io, &mut pending, &mut next_id, command).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = io.close_away().await;
}

fn dispatch_text(
    text: &str,
    pending: &mut HashMap<String, oneshot::Sender<Result<Value, String>>>,
    inbound: &mpsc::UnboundedSender<InboundCall>,
) {
    let frame = match decode(text) {
        Ok(frame) => frame,
        Err(err) => {
            tracing::debug!(error = %err, frame = %text, "dropped malformed OCPP frame");
            return;
        }
    };
    match frame {
        Frame::Call {
            id,
            action,
            payload,
        } => {
            let _ = inbound.send(InboundCall {
                id,
                action,
                payload,
            });
        }
        Frame::Result { id, payload } => {
            if let Some(reply) = pending.remove(&id) {
                let _ = reply.send(Ok(payload));
            }
        }
        Frame::Error {
            id, description, ..
        } => {
            if let Some(reply) = pending.remove(&id) {
                let _ = reply.send(Err(description));
            }
        }
    }
}

async fn handle_outbound(
    io: &mut impl WsIo,
    pending: &mut HashMap<String, oneshot::Sender<Result<Value, String>>>,
    next_id: &mut u64,
    command: Outbound,
) -> Result<(), String> {
    let frame = match command {
        Outbound::Result { id, payload } => Frame::Result { id, payload },
        Outbound::Error {
            id,
            code,
            description,
        } => crate::codec::call_error(&id, &code, &description),
        Outbound::Call {
            action,
            payload,
            reply,
        } => {
            let id = next_id.to_string();
            *next_id += 1;
            pending.insert(id.clone(), reply);
            Frame::Call {
                id,
                action,
                payload,
            }
        }
    };
    io.send_text(encode(&frame)).await
}
