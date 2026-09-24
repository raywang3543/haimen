//! 连接 haimen-relay 的 local 端消息通道。

use std::{
    pin::Pin,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message as WsMessage,
        client::IntoClientRequest,
        http::{HeaderValue, StatusCode},
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    config::settings::{RelayConnectorConfig, resolve_env_ref},
    gateway::{channel::MessageChannel, model::Message},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    Message { payload: Value },
    Status { peer_online: bool },
    Error { code: String },
}

#[derive(Deserialize)]
struct DevicePayload {
    text: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    sender_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

struct Outbound {
    frame: Frame,
    result: oneshot::Sender<Result<(), String>>,
}

pub struct RelayChannel {
    config: RelayConnectorConfig,
    outbound: Arc<RwLock<Option<mpsc::Sender<Outbound>>>>,
    peer_online: Arc<AtomicBool>,
    cancel: CancellationToken,
}

#[derive(Deserialize)]
struct RelayCredentials {
    pair: String,
    token: String,
}

fn resolve_relay_value(value: &str, field: &str) -> Result<String, String> {
    if let Some(reference) = value
        .strip_prefix("${file:")
        .and_then(|s| s.strip_suffix('}'))
    {
        let (path, key) = reference.rsplit_once('#').unwrap_or((reference, ""));
        if path.is_empty() {
            return Err("Relay 凭证文件路径不能为空".to_string());
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 Relay 凭证文件 {path} 失败: {e}"))?;
        if key.is_empty() {
            return Ok(content.trim().to_string());
        }
        if key != field {
            return Err(format!("Relay 凭证字段 {key} 无效，预期 {field}"));
        }
        let credentials: RelayCredentials = toml::from_str(&content)
            .map_err(|e| format!("解析 Relay 凭证文件 {path} 失败: {e}"))?;
        return match key {
            "pair" => Ok(credentials.pair),
            "token" => Ok(credentials.token),
            _ => unreachable!(),
        };
    }
    resolve_env_ref(value)
}

impl RelayChannel {
    pub fn new(config: RelayConnectorConfig) -> Self {
        Self {
            config,
            outbound: Arc::new(RwLock::new(None)),
            peer_online: Arc::new(AtomicBool::new(false)),
            cancel: CancellationToken::new(),
        }
    }

    fn resolved_pair(&self) -> Result<String, String> {
        resolve_relay_value(&self.config.pair, "pair")
    }

    fn resolved_token(&self) -> Result<String, String> {
        resolve_relay_value(&self.config.token, "token")
    }
}

impl Drop for RelayChannel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[async_trait]
impl MessageChannel for RelayChannel {
    fn name(&self) -> &str {
        "relay"
    }

    async fn health_check(&self) -> Result<(), String> {
        if !self.config.url.starts_with("ws://") && !self.config.url.starts_with("wss://") {
            return Err("Relay URL 必须以 ws:// 或 wss:// 开头".to_string());
        }
        self.config
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("Relay URL 无效: {e}"))?;
        let pair = self.resolved_pair()?;
        if pair.trim().is_empty() {
            return Err("Relay pair 不能为空".to_string());
        }
        HeaderValue::from_str(&pair).map_err(|e| format!("Relay pair 无效: {e}"))?;
        let token = self.resolved_token()?;
        if token.is_empty() {
            return Err("Relay token 不能为空".to_string());
        }
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("Relay token 无效: {e}"))?;
        Ok(())
    }

    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = Message> + Send>>, String> {
        self.health_check().await?;
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(run_connection_loop(
            self.config.url.clone(),
            self.resolved_pair()?,
            self.resolved_token()?,
            self.outbound.clone(),
            self.peer_online.clone(),
            self.cancel.clone(),
            tx,
        ));
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn send(&self, conversation_id: &str, message: &str) -> Result<(), String> {
        if !self.peer_online.load(Ordering::Acquire) {
            return Err("Relay 设备端离线".to_string());
        }
        let tx = self
            .outbound
            .read()
            .map_err(|_| "Relay 连接状态锁失效".to_string())?
            .clone()
            .ok_or_else(|| "Relay 尚未连接".to_string())?;
        let (result_tx, result_rx) = oneshot::channel();
        let outbound = Outbound {
            frame: Frame::Message {
                payload: json!({
                    "conversation_id": conversation_id,
                    "sender_id": "haimen",
                    "text": message,
                }),
            },
            result: result_tx,
        };
        tokio::time::timeout(Duration::from_secs(10), tx.send(outbound))
            .await
            .map_err(|_| "Relay 发送队列超时".to_string())?
            .map_err(|_| "Relay 连接已断开".to_string())?;
        tokio::time::timeout(Duration::from_secs(10), result_rx)
            .await
            .map_err(|_| "Relay 写入超时".to_string())?
            .map_err(|_| "Relay 连接已断开".to_string())?
    }
}

async fn run_connection_loop(
    url: String,
    pair: String,
    token: String,
    outbound: Arc<RwLock<Option<mpsc::Sender<Outbound>>>>,
    peer_online: Arc<AtomicBool>,
    cancel: CancellationToken,
    inbound: mpsc::Sender<Message>,
) {
    let mut delay = Duration::from_secs(1);
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let mut request = match url.as_str().into_client_request() {
            Ok(request) => request,
            Err(e) => {
                tracing::error!(error = %e, "Relay URL 无效");
                break;
            }
        };
        let headers = request.headers_mut();
        let pair_header = match HeaderValue::from_str(&pair) {
            Ok(value) => value,
            Err(e) => {
                tracing::error!(error = %e, "Relay pair 无效");
                break;
            }
        };
        let token_header = match HeaderValue::from_str(&format!("Bearer {token}")) {
            Ok(value) => value,
            Err(e) => {
                tracing::error!(error = %e, "Relay token 无效");
                break;
            }
        };
        headers.insert("x-relay-pair", pair_header);
        headers.insert("x-relay-role", HeaderValue::from_static("local"));
        headers.insert("authorization", token_header);

        let connect_result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(10), connect_async(request)) => result,
            _ = cancel.cancelled() => break,
        };
        match connect_result {
            Ok(Ok((socket, _))) => {
                tracing::info!(pair = %pair, "Relay 已连接");
                delay = Duration::from_secs(1);
                let (out_tx, out_rx) = mpsc::channel(128);
                *outbound.write().expect("Relay 连接状态锁失效") = Some(out_tx);
                run_connected(socket, &pair, &inbound, out_rx, &peer_online, &cancel).await;
                peer_online.store(false, Ordering::Release);
                *outbound.write().expect("Relay 连接状态锁失效") = None;
                tracing::warn!(pair = %pair, "Relay 已断开，准备重连");
            }
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                if matches!(
                    response.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::BAD_REQUEST
                ) =>
            {
                tracing::error!(status = %response.status(), "Relay 鉴权失败，停止重连");
                // 保持消息流存活，避免唯一连接器失败时连带关闭 HTTP/LAN 服务。
                cancel.cancelled().await;
                break;
            }
            Ok(Err(e)) => tracing::warn!(error = %e, "Relay 连接失败，准备重连"),
            Err(_) => tracing::warn!("Relay 建连超过 10 秒，准备重连"),
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay) => {},
        }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

async fn run_connected(
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    pair: &str,
    inbound: &mpsc::Sender<Message>,
    mut outbound: mpsc::Receiver<Outbound>,
    peer_online: &AtomicBool,
    cancel: &CancellationToken,
) {
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            outgoing = outbound.recv() => {
                let Some(outgoing) = outgoing else { break };
                let raw = serde_json::to_string(&outgoing.frame).expect("Relay frame serialization");
                let result = sink.send(WsMessage::Text(raw.into())).await
                    .map_err(|e| format!("Relay 写入失败: {e}"));
                let failed = result.is_err();
                let _ = outgoing.result.send(result);
                if failed { break; }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<Frame>(&text) {
                            Ok(Frame::Message { payload }) => {
                                if let Some(message) = parse_device_message(payload, pair) {
                                    if inbound.send(message).await.is_err() { break; }
                                }
                            }
                            Ok(Frame::Status { peer_online: online }) => {
                                peer_online.store(online, Ordering::Release);
                                tracing::info!(pair = %pair, peer_online = online, "Relay 对端状态更新");
                            }
                            Ok(Frame::Error { code }) => {
                                tracing::warn!(code = %code, "Relay 服务端返回错误");
                                if code == "peer_offline" {
                                    peer_online.store(false, Ordering::Release);
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "Relay 收到无效消息帧"),
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

fn parse_device_message(payload: Value, pair: &str) -> Option<Message> {
    let payload = match serde_json::from_value::<DevicePayload>(payload) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(error = %e, "Relay 设备消息缺少 text 字段");
            return None;
        }
    };
    if payload.text.trim().is_empty() {
        return None;
    }
    Some(Message {
        id: payload
            .id
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        conversation_id: payload
            .conversation_id
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| pair.to_string()),
        sender_id: payload
            .sender_id
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "device".to_string()),
        content: payload.text,
        timestamp: Utc::now(),
        channel: "relay".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tokio::sync::Notify;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    #[test]
    fn reads_token_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "pair = \"haimenrelay\"\ntoken = \"test-token\"\n").unwrap();
        let channel = RelayChannel::new(RelayConnectorConfig {
            pair: format!("${{file:{}#pair}}", path.display()),
            token: format!("${{file:{}#token}}", path.display()),
            ..RelayConnectorConfig::default()
        });
        assert_eq!(channel.resolved_pair().unwrap(), "haimenrelay");
        assert_eq!(channel.resolved_token().unwrap(), "test-token");
    }

    #[tokio::test]
    async fn receives_device_message_and_sends_reply() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &Request, response: Response| {
                    assert_eq!(request.headers()["x-relay-pair"], "test-pair");
                    assert_eq!(request.headers()["x-relay-role"], "local");
                    assert_eq!(request.headers()["authorization"], "Bearer test-token");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&Frame::Status { peer_online: true })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&Frame::Message {
                        payload: json!({
                            "id": "request-1",
                            "conversation_id": "conversation-1",
                            "sender_id": "device-1",
                            "text": "你好",
                        }),
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            let reply = socket.next().await.unwrap().unwrap();
            let WsMessage::Text(raw) = reply else {
                panic!("expected text reply")
            };
            let Frame::Message { payload } = serde_json::from_str(&raw).unwrap() else {
                panic!("expected message reply")
            };
            assert_eq!(payload["conversation_id"], "conversation-1");
            assert_eq!(payload["sender_id"], "haimen");
            assert_eq!(payload["text"], "回复");
        });

        let channel = RelayChannel::new(RelayConnectorConfig {
            enabled: true,
            url: format!("ws://{address}/ws"),
            pair: "test-pair".to_string(),
            token: "test-token".to_string(),
        });
        let mut messages = channel.listen().await.unwrap();
        let message = tokio::time::timeout(Duration::from_secs(3), messages.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.id, "request-1");
        assert_eq!(message.conversation_id, "conversation-1");
        assert_eq!(message.sender_id, "device-1");
        assert_eq!(message.content, "你好");
        channel
            .send(&message.conversation_id, "回复")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn authentication_failure_keeps_channel_stream_alive_until_shutdown() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_seen = Arc::new(Notify::new());
        let app = Router::new().route(
            "/ws",
            get({
                let request_seen = request_seen.clone();
                move || {
                    let request_seen = request_seen.clone();
                    async move {
                        request_seen.notify_one();
                        axum::http::StatusCode::UNAUTHORIZED
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let channel = RelayChannel::new(RelayConnectorConfig {
            enabled: true,
            url: format!("ws://{address}/ws"),
            pair: "test-pair".to_string(),
            token: "wrong-token".to_string(),
        });
        let mut messages = channel.listen().await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), request_seen.notified())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), messages.next())
                .await
                .is_err(),
            "Relay auth failure must not close the gateway message stream"
        );
        drop(channel);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), messages.next())
                .await
                .unwrap()
                .is_none()
        );
        server.abort();
    }
}
