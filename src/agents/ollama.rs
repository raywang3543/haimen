//! Ollama 本地 HTTP Agent。会话历史保存在当前 Agent 实例中，热切换时一起重置。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::gateway::provider::{AgentOutput, AgentProvider};

const MAX_MESSAGES: usize = 40;
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

struct Session {
    messages: Vec<ChatMessage>,
    touched: Instant,
}

/// 通过 Ollama `/api/chat` 处理消息的 Agent。
pub struct OllamaAgent {
    base_url: String,
    model_id: String,
    client: reqwest::Client,
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<Session>>>>,
}

impl OllamaAgent {
    pub fn new(base_url: &str, model_id: &str, timeout_secs: u64) -> Result<Self, String> {
        let url = reqwest::Url::parse(base_url.trim())
            .map_err(|e| format!("Ollama 服务地址无效: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("Ollama 服务地址必须是 http:// 或 https:// URL".to_string());
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Ollama 服务地址不能包含凭证、查询参数或片段".to_string());
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
            .build()
            .map_err(|e| format!("创建 Ollama HTTP 客户端失败: {e}"))?;
        Ok(Self {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            model_id: model_id.trim().to_string(),
            client,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    fn session(&self, session_id: Option<&str>) -> (String, Arc<AsyncMutex<Session>>) {
        let mut sessions = self.sessions.lock().expect("Ollama sessions 锁中毒");
        sessions.retain(|_, session| match session.try_lock() {
            Ok(s) => s.touched.elapsed() < SESSION_TTL,
            Err(_) => true,
        });
        if let Some(id) = session_id {
            if let Some(session) = sessions.get(id) {
                return (id.to_string(), session.clone());
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(AsyncMutex::new(Session {
            messages: Vec::new(),
            touched: Instant::now(),
        }));
        sessions.insert(id.clone(), session.clone());
        (id, session)
    }
}

#[async_trait]
impl AgentProvider for OllamaAgent {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn check_available(&self) -> Result<(), String> {
        if self.model_id.is_empty() {
            return Err("请先配置 Ollama 模型 ID".to_string());
        }
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("无法连接 Ollama: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Ollama 模型列表请求失败: {e}"))?;
        let tags: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Ollama 模型列表解析失败: {e}"))?;
        let found = tags
            .get("models")
            .and_then(|m| m.as_array())
            .is_some_and(|models| {
                models.iter().any(|model| {
                    ["name", "model"].iter().any(|key| {
                        model
                            .get(*key)
                            .and_then(|v| v.as_str())
                            .is_some_and(|name| {
                                name == self.model_id || name == format!("{}:latest", self.model_id)
                            })
                    })
                })
            });
        if found {
            Ok(())
        } else {
            Err(format!(
                "Ollama 模型 {} 未安装，请先运行 ollama pull {}",
                self.model_id, self.model_id
            ))
        }
    }

    async fn process(
        &self,
        message: &str,
        session_id: Option<&str>,
        _work_dir: &str,
    ) -> Result<(AgentOutput, String), String> {
        let (id, session) = self.session(session_id);
        let mut session = session.lock().await;
        let mut messages = session.messages.clone();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: message.to_string(),
        });
        let response = self.client.post(format!("{}/api/chat", self.base_url))
            .json(&serde_json::json!({ "model": self.model_id, "messages": messages, "stream": false }))
            .send().await.map_err(|e| format!("Ollama 请求失败: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(format!("Ollama 返回 {}: {}", status, detail));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Ollama 响应解析失败: {e}"))?;
        let text = body
            .pointer("/message/content")
            .and_then(|v| v.as_str())
            .ok_or("Ollama 响应缺少 message.content")?
            .to_string();
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: text.clone(),
        });
        if messages.len() > MAX_MESSAGES {
            messages.drain(..messages.len() - MAX_MESSAGES);
        }
        session.messages = messages;
        session.touched = Instant::now();
        Ok((
            AgentOutput {
                text,
                events: Vec::new(),
            },
            id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        routing::{get, post},
    };

    #[tokio::test]
    async fn ollama_checks_model_and_preserves_conversation() {
        let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = requests.clone();
        let app = Router::new()
            .route("/api/tags", get(|| async {
                Json(serde_json::json!({"models": [{"name": "qwen3:8b"}]}))
            }))
            .route("/api/chat", post(move |Json(body): Json<serde_json::Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(body);
                    Json(serde_json::json!({"message": {"role": "assistant", "content": "你好"}}))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let agent = OllamaAgent::new(&format!("http://{address}"), "qwen3:8b", 5).unwrap();

        agent.check_available().await.unwrap();
        let (first, sid) = agent.process("第一句", None, ".").await.unwrap();
        assert_eq!(first.text, "你好");
        let (_, resumed_sid) = agent.process("第二句", Some(&sid), ".").await.unwrap();
        assert_eq!(sid, resumed_sid);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["model"], "qwen3:8b");
        assert_eq!(requests[0]["stream"], false);
        assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 3);
        assert_eq!(requests[1]["messages"][1]["content"], "你好");
        server.abort();
    }

    #[test]
    fn rejects_invalid_base_url() {
        assert!(OllamaAgent::new("file:///tmp/ollama", "qwen3:8b", 5).is_err());
    }
}
