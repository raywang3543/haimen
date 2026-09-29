//! Optional OpenClaw Gateway WebSocket transport. The CLI transport remains the default.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use haimen_core::{
    ImageData,
    provider::{AgentEventStream, AgentOutput, AgentProvider, TextStream},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::DEFAULT_AGENT_ID;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UNCERTAIN_PREFIX: &str = "OpenClaw 运行状态未确认";
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone)]
pub struct WebSocketConfig {
    pub url: String,
    pub agent: String,
    pub timeout_secs: u64,
    pub token_env: String,
    pub password_env: Option<String>,
    pub identity_path: PathBuf,
}

impl WebSocketConfig {
    pub fn new(identity_path: PathBuf) -> Self {
        Self {
            url: "ws://127.0.0.1:18789".to_string(),
            agent: DEFAULT_AGENT_ID.to_string(),
            timeout_secs: 300,
            token_env: "OPENCLAW_GATEWAY_TOKEN".to_string(),
            password_env: None,
            identity_path,
        }
    }
}

pub struct OpenClawWebSocketAgent {
    config: WebSocketConfig,
}

impl OpenClawWebSocketAgent {
    pub fn new(config: WebSocketConfig) -> Result<Self, String> {
        if !config.url.starts_with("ws://") && !config.url.starts_with("wss://") {
            return Err("OpenClaw WebSocket URL 必须以 ws:// 或 wss:// 开头".to_string());
        }
        if config.agent.trim().is_empty() {
            return Err("OpenClaw agent id 不能为空".to_string());
        }
        if config.timeout_secs < 2 {
            return Err("OpenClaw WebSocket 超时必须至少为 2 秒".to_string());
        }
        Ok(Self { config })
    }

    async fn connect(&self) -> Result<(Socket, Value), String> {
        let identity = DeviceIdentity::load_or_create(&self.config.identity_path)?;
        let token = std::env::var(&self.config.token_env)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| identity.device_token.clone());
        let password = self
            .config
            .password_env
            .as_ref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|v| !v.is_empty());
        let token = if password.is_some() && std::env::var(&self.config.token_env).is_err() {
            None
        } else {
            token
        };
        let (mut socket, _) =
            tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&self.config.url))
                .await
                .map_err(|_| "连接 OpenClaw Gateway 超时".to_string())?
                .map_err(|e| format!("连接 OpenClaw Gateway 失败: {e}"))?;

        let challenge = tokio::time::timeout(CONNECT_TIMEOUT, read_json(&mut socket))
            .await
            .map_err(|_| "等待 OpenClaw connect.challenge 超时".to_string())??;
        if challenge["type"] != "event" || challenge["event"] != "connect.challenge" {
            return Err("OpenClaw Gateway 未发送 connect.challenge".to_string());
        }
        let nonce = challenge["payload"]["nonce"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("OpenClaw connect.challenge 缺少 nonce")?;
        let signed_at = challenge["payload"]["ts"]
            .as_u64()
            .ok_or("OpenClaw connect.challenge 缺少有效时间戳")?;
        let scopes = ["operator.read", "operator.write"];
        let platform = std::env::consts::OS.to_lowercase();
        let signature_payload = format!(
            "v3|{}|cli|cli|operator|{}|{}|{}|{}|{}|",
            identity.id(),
            scopes.join(","),
            signed_at,
            token.as_deref().unwrap_or(""),
            nonce,
            platform,
        );
        let signature =
            URL_SAFE_NO_PAD.encode(identity.key().sign(signature_payload.as_bytes()).to_bytes());
        let mut auth = serde_json::Map::new();
        if let Some(value) = token.as_ref() {
            auth.insert("token".to_string(), json!(value));
        }
        if let Some(value) = password.as_ref() {
            auth.insert("password".to_string(), json!(value));
        }
        let connect = json!({
            "type": "req", "id": "connect", "method": "connect",
            "params": {
                "minProtocol": 4, "maxProtocol": 4,
                "client": { "id": "cli", "displayName": "haimen", "version": env!("CARGO_PKG_VERSION"),
                    "platform": platform, "mode": "cli" },
                "role": "operator", "scopes": scopes,
                "caps": [], "commands": [], "permissions": {},
                "auth": auth,
                "device": { "id": identity.id(), "publicKey": identity.public_key(),
                    "signature": signature, "signedAt": signed_at, "nonce": nonce }
            }
        });
        send_json(&mut socket, &connect).await?;
        let hello = tokio::time::timeout(CONNECT_TIMEOUT, read_json(&mut socket))
            .await
            .map_err(|_| "等待 OpenClaw hello-ok 超时".to_string())??;
        if hello["id"] != "connect" || hello["ok"] != true || hello["payload"]["type"] != "hello-ok"
        {
            return Err(connect_error(&hello));
        }
        if let Some(device_token) = hello["payload"]["auth"]["deviceToken"].as_str() {
            identity.save_token(&self.config.identity_path, device_token)?;
        }
        Ok((socket, hello["payload"]["policy"].clone()))
    }

    async fn run(
        &self,
        message: &str,
        images: &[ImageData],
        session_id: Option<&str>,
        _work_dir: &str,
    ) -> Result<(AgentOutput, String), String> {
        let started = std::time::Instant::now();
        let session_key = session_id
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("agent:{}:haimen:{}", self.config.agent, Uuid::new_v4()));
        let (mut socket, policy) = self.connect().await?;
        let request_id = Uuid::new_v4().to_string();
        let idempotency_key = Uuid::new_v4().to_string();
        let mut request = json!({
            "type": "req", "id": request_id, "method": "agent",
            "params": { "message": if message.trim().is_empty() && !images.is_empty() {
                    "请查看附件图片。"
                } else {
                    message
                }, "agentId": self.config.agent,
                "sessionKey": session_key,
                "timeout": self.config.timeout_secs, "deliver": false,
                "idempotencyKey": idempotency_key }
        });
        if !images.is_empty() {
            let attachments = images
                .iter()
                .enumerate()
                .map(|(index, image)| {
                    if !matches!(
                        image.mime_type.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    ) {
                        return Err(format!("第 {} 张图片格式不支持", index + 1));
                    }
                    let bytes = STANDARD
                        .decode(&image.data_base64)
                        .map_err(|_| format!("第 {} 张图片 Base64 无效", index + 1))?;
                    if bytes.is_empty() {
                        return Err(format!("第 {} 张图片为空", index + 1));
                    }
                    if let Some(limit) = policy["attachments"]["maxImageBytes"].as_u64() {
                        if bytes.len() as u64 > limit {
                            return Err(format!(
                                "第 {} 张图片超过 OpenClaw Gateway 限制（{} > {} 字节）",
                                index + 1,
                                bytes.len(),
                                limit
                            ));
                        }
                    }
                    Ok(json!({ "mimeType": image.mime_type, "content": image.data_base64 }))
                })
                .collect::<Result<Vec<_>, String>>()?;
            request["params"]["attachments"] = json!(attachments);
            if let Some(limit) = policy["maxPayload"].as_u64() {
                let size = request.to_string().len() as u64;
                if size > limit {
                    return Err(format!(
                        "OpenClaw 请求超过 Gateway WebSocket 限制（{size} > {limit} 字节）"
                    ));
                }
            }
        }
        send_json(&mut socket, &request).await.map_err(|e| {
            format!("{UNCERTAIN_PREFIX}（requestKey: {idempotency_key}, sessionKey: {session_key}）: {e}")
        })?;
        let deadline = Duration::from_secs(self.config.timeout_secs - 1);
        let remaining = deadline.saturating_sub(started.elapsed());
        let result = tokio::time::timeout(
            remaining,
            async {
                loop {
                    let frame = read_json(&mut socket).await.map_err(|e| format!(
                        "{UNCERTAIN_PREFIX}（requestKey: {idempotency_key}, sessionKey: {session_key}）: {e}"
                    ))?;
                    if frame["type"] != "res" || frame["id"] != request_id {
                        continue;
                    }
                    if frame["ok"] != true {
                        return Err(gateway_error(&frame));
                    }
                    let payload = &frame["payload"];
                    if payload["status"] == "accepted" {
                        continue;
                    }
                    if payload["status"] != "ok" {
                        return Err(format!(
                            "OpenClaw 运行失败: {}",
                            payload["summary"].as_str().unwrap_or("未知错误")
                        ));
                    }
                    let text = extract_text(payload)?;
                    return Ok((
                        AgentOutput {
                            text,
                            events: Vec::new(),
                        },
                        session_key.clone(),
                    ));
                }
            },
        )
        .await;
        match result {
            Ok(value) => value,
            Err(_) => Err(format!(
                "{UNCERTAIN_PREFIX}（requestKey: {idempotency_key}, sessionKey: {session_key}），等待响应超时；请查看 OpenClaw 会话后再重试"
            )),
        }
    }
}

#[async_trait]
impl AgentProvider for OpenClawWebSocketAgent {
    fn name(&self) -> &str {
        "openclaw"
    }

    async fn check_available(&self) -> Result<(), String> {
        self.connect().await.map(|_| ())
    }

    async fn process(
        &self,
        message: &str,
        session_id: Option<&str>,
        work_dir: &str,
    ) -> Result<(AgentOutput, String), String> {
        self.run(message, &[], session_id, work_dir).await
    }

    async fn process_with_images(
        &self,
        message: &str,
        images: &[ImageData],
        session_id: Option<&str>,
        work_dir: &str,
    ) -> Result<(AgentOutput, String), String> {
        self.run(message, images, session_id, work_dir).await
    }

    async fn process_stream(
        &self,
        message: &str,
        session_id: Option<&str>,
        work_dir: &str,
    ) -> Result<(TextStream, String, AgentEventStream), String> {
        let (output, session_key) = self.run(message, &[], session_id, work_dir).await?;
        let stream: TextStream = Box::pin(tokio_stream::iter([output.text]));
        let (_sender, events) = tokio::sync::mpsc::channel(1);
        Ok((stream, session_key, events))
    }
}

struct DeviceIdentity {
    seed: [u8; 32],
    device_token: Option<String>,
}

impl DeviceIdentity {
    fn key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.seed)
    }
    fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.key().verifying_key().to_bytes())
    }
    fn id(&self) -> String {
        hex::encode(Sha256::digest(self.key().verifying_key().to_bytes()))
    }

    fn load_or_create(path: &Path) -> Result<Self, String> {
        if path.exists() {
            return Self::load(path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建设备身份目录失败: {e}"))?;
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| format!("生成设备密钥失败: {e}"))?;
        let identity = Self {
            seed,
            device_token: None,
        };
        let data = identity.serialize();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(data.to_string().as_bytes())
                    .map_err(|e| format!("保存设备身份失败: {e}"))?;
                Ok(identity)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load(path),
            Err(e) => Err(format!("保存设备身份失败: {e}")),
        }
    }

    fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("读取设备身份失败: {e}"))?;
        let value: Value =
            serde_json::from_str(&raw).map_err(|e| format!("设备身份文件无效: {e}"))?;
        let seed_bytes = URL_SAFE_NO_PAD
            .decode(value["seed"].as_str().unwrap_or(""))
            .map_err(|_| "设备身份密钥无效".to_string())?;
        let seed: [u8; 32] = seed_bytes
            .try_into()
            .map_err(|_| "设备身份密钥长度无效".to_string())?;
        Ok(Self {
            seed,
            device_token: value["device_token"].as_str().map(str::to_owned),
        })
    }

    fn serialize(&self) -> Value {
        json!({ "seed": URL_SAFE_NO_PAD.encode(self.seed), "device_token": self.device_token })
    }

    fn save_token(mut self, path: &Path, token: &str) -> Result<(), String> {
        if self.device_token.as_deref() == Some(token) {
            return Ok(());
        }
        self.device_token = Some(token.to_string());
        std::fs::write(path, self.serialize().to_string())
            .map_err(|e| format!("保存 OpenClaw deviceToken 失败: {e}"))
    }
}

async fn send_json(socket: &mut Socket, value: &Value) -> Result<(), String> {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .map_err(|e| format!("发送 OpenClaw 请求失败: {e}"))
}

async fn read_json(socket: &mut Socket) -> Result<Value, String> {
    loop {
        let message = socket
            .next()
            .await
            .ok_or("OpenClaw WebSocket 已关闭")?
            .map_err(|e| format!("读取 OpenClaw WebSocket 失败: {e}"))?;
        match message {
            Message::Text(text) => {
                return serde_json::from_str(&text)
                    .map_err(|e| format!("OpenClaw 响应 JSON 无效: {e}"));
            }
            Message::Ping(data) => socket
                .send(Message::Pong(data))
                .await
                .map_err(|e| format!("回应 OpenClaw ping 失败: {e}"))?,
            Message::Close(_) => return Err("OpenClaw WebSocket 已关闭".to_string()),
            _ => {}
        }
    }
}

fn gateway_error(frame: &Value) -> String {
    let error = &frame["error"];
    format!(
        "{}: {}",
        error["code"].as_str().unwrap_or("OpenClaw 错误"),
        error["message"].as_str().unwrap_or("未知错误")
    )
}

fn connect_error(frame: &Value) -> String {
    let error = &frame["error"];
    if error["details"]["code"] == "PAIRING_REQUIRED" || error["code"] == "PAIRING_REQUIRED" {
        let request = error["details"]["requestId"]
            .as_str()
            .map(|id| format!("（requestId: {id}）"))
            .unwrap_or_default();
        return format!(
            "OpenClaw 设备待配对{request}；请在 OpenClaw 主机执行 openclaw devices list，然后 openclaw devices approve <requestId>"
        );
    }
    gateway_error(frame)
}

fn extract_text(payload: &Value) -> Result<String, String> {
    let text = payload["result"]["payloads"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        Err("OpenClaw 返回为空".to_string())
    } else {
        Ok(text)
    }
}

pub fn is_uncertain_run_error(error: &str) -> bool {
    error.starts_with(UNCERTAIN_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    #[test]
    fn extracts_final_payloads() {
        let response = json!({ "result": { "payloads": [{"text":"hello"}, {"text":"world"}] } });
        assert_eq!(extract_text(&response).unwrap(), "hello\nworld");
    }

    #[test]
    fn pairing_error_explains_approval() {
        let frame = json!({
            "error": { "code": "UNAUTHORIZED", "message": "pairing required",
                "details": { "code": "PAIRING_REQUIRED", "requestId": "pair-123" } }
        });
        let message = connect_error(&frame);
        assert!(message.contains("pair-123"));
        assert!(message.contains("openclaw devices approve"));
    }

    #[test]
    fn identity_is_stable_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let first = DeviceIdentity::load_or_create(&path).unwrap();
        let second = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(first.id(), second.id());
        first.save_token(&path, "token").unwrap();
        assert_eq!(
            DeviceIdentity::load(&path).unwrap().device_token.as_deref(),
            Some("token")
        );
    }

    #[tokio::test]
    async fn gateway_handshake_and_final_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "type": "event", "event": "connect.challenge",
                        "payload": { "nonce": "test-nonce", "ts": 1_700_000_000_000_u64 }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let connect: Value =
                serde_json::from_str(&socket.next().await.unwrap().unwrap().into_text().unwrap())
                    .unwrap();
            assert_eq!(connect["method"], "connect");
            assert_eq!(connect["params"]["auth"], json!({}));
            let device = &connect["params"]["device"];
            let public: [u8; 32] = URL_SAFE_NO_PAD
                .decode(device["publicKey"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            assert_eq!(device["id"], hex::encode(Sha256::digest(public)));
            let proof = format!(
                "v3|{}|cli|cli|operator|operator.read,operator.write|1700000000000||test-nonce|{}|",
                device["id"].as_str().unwrap(),
                std::env::consts::OS,
            );
            let signature = Signature::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(device["signature"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
            VerifyingKey::from_bytes(&public)
                .unwrap()
                .verify(proof.as_bytes(), &signature)
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "type": "res", "id": "connect", "ok": true,
                        "payload": { "type": "hello-ok", "auth": { "deviceToken": "paired-token" } }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let request: Value =
                serde_json::from_str(&socket.next().await.unwrap().unwrap().into_text().unwrap())
                    .unwrap();
            assert_eq!(request["method"], "agent");
            assert_eq!(request["params"]["message"], "hello");
            assert_eq!(request["params"]["agentId"], "main");
            assert_eq!(request["params"]["deliver"], false);
            assert!(request["params"].get("attachments").is_none());
            assert!(request["params"].get("cwd").is_none());
            let request_id = request["id"].as_str().unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "type": "res", "id": request_id, "ok": true,
                        "payload": { "status": "accepted", "runId": "run-1" }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket.send(Message::Text(json!({
                "type": "res", "id": request_id, "ok": true,
                "payload": { "status": "ok", "result": { "payloads": [{ "text": "world" }] } }
            }).to_string().into())).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let mut config = WebSocketConfig::new(dir.path().join("device.json"));
        config.url = format!("ws://{address}");
        config.token_env = "HAIMEN_WS_TEST_UNSET_TOKEN".to_string();
        let agent = OpenClawWebSocketAgent::new(config).unwrap();
        let (output, session_key) = agent.process("hello", None, "/tmp").await.unwrap();
        assert_eq!(output.text, "world");
        assert!(session_key.starts_with("agent:main:haimen:"));
        assert_eq!(
            DeviceIdentity::load(&dir.path().join("device.json"))
                .unwrap()
                .device_token
                .as_deref(),
            Some("paired-token")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_image_requests_include_attachments() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for expected_message in ["分析图片", "请查看附件图片。"] {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                socket
                    .send(Message::Text(
                        json!({ "type": "event", "event": "connect.challenge",
                            "payload": { "nonce": "test-nonce", "ts": 1_700_000_000_000_u64 } })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let _: Value = serde_json::from_str(
                    &socket.next().await.unwrap().unwrap().into_text().unwrap(),
                )
                .unwrap();
                socket
                    .send(Message::Text(
                        json!({ "type": "res", "id": "connect", "ok": true,
                            "payload": { "type": "hello-ok",
                                "policy": { "maxPayload": 1024,
                                    "attachments": { "maxImageBytes": 100 } } } })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let request: Value = serde_json::from_str(
                    &socket.next().await.unwrap().unwrap().into_text().unwrap(),
                )
                .unwrap();
                assert_eq!(request["method"], "agent");
                assert_eq!(request["params"]["message"], expected_message);
                assert_eq!(
                    request["params"]["attachments"],
                    json!([{ "mimeType": "image/png", "content": "aW1hZ2U=" }])
                );
                let request_id = request["id"].as_str().unwrap();
                socket
                    .send(Message::Text(
                        json!({ "type": "res", "id": request_id, "ok": true,
                            "payload": { "status": "ok", "result": {
                                "payloads": [{ "text": "看到了" }] } } })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let mut config = WebSocketConfig::new(dir.path().join("device.json"));
        config.url = format!("ws://{address}");
        config.token_env = "HAIMEN_WS_TEST_UNSET_TOKEN".to_string();
        let agent = OpenClawWebSocketAgent::new(config).unwrap();
        let image = ImageData {
            mime_type: "image/png".to_string(),
            data_base64: "aW1hZ2U=".to_string(),
        };
        let (first, session_key) = agent
            .process_with_images("分析图片", std::slice::from_ref(&image), None, "/tmp")
            .await
            .unwrap();
        assert_eq!(first.text, "看到了");
        let (second, resumed_key) = agent
            .process_with_images("", &[image], Some(&session_key), "/tmp")
            .await
            .unwrap();
        assert_eq!(second.text, "看到了");
        assert_eq!(resumed_key, session_key);
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires a running paired OpenClaw Gateway or OPENCLAW_GATEWAY_TOKEN"]
    async fn live_gateway_reply() {
        let identity_path = std::env::var("HAIMEN_OPENCLAW_TEST_IDENTITY")
            .expect("set HAIMEN_OPENCLAW_TEST_IDENTITY to a persistent private path");
        let mut config = WebSocketConfig::new(PathBuf::from(identity_path));
        config.timeout_secs = 120;
        let agent = OpenClawWebSocketAgent::new(config).unwrap();
        agent.check_available().await.unwrap();
        let (output, session_key) = agent
            .process("Reply with the single word pong.", None, "/tmp")
            .await
            .unwrap();
        assert!(!output.text.trim().is_empty());
        let (continued, resumed_key) = agent
            .process(
                "Reply with the single word ack.",
                Some(&session_key),
                "/tmp",
            )
            .await
            .unwrap();
        assert!(!continued.text.trim().is_empty());
        assert_eq!(resumed_key, session_key);
    }

    #[tokio::test]
    #[ignore = "requires a running paired OpenClaw Gateway and an available agent model"]
    async fn live_gateway_image_then_text_reply() {
        let identity_path = std::env::var("HAIMEN_OPENCLAW_TEST_IDENTITY")
            .expect("set HAIMEN_OPENCLAW_TEST_IDENTITY to a persistent private path");
        let mut config = WebSocketConfig::new(PathBuf::from(identity_path));
        config.timeout_secs = 120;
        let agent = OpenClawWebSocketAgent::new(config).unwrap();
        let image = ImageData {
            mime_type: "image/png".to_string(),
            data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC".to_string(),
        };
        let (image_reply, session_key) = agent
            .process_with_images("请描述这张图片。", &[image], None, "/tmp")
            .await
            .unwrap();
        assert!(!image_reply.text.trim().is_empty());
        let (text_reply, resumed_key) = agent
            .process("继续回复一句纯文字。", Some(&session_key), "/tmp")
            .await
            .unwrap();
        assert!(!text_reply.text.trim().is_empty());
        assert_eq!(resumed_key, session_key);
    }
}
