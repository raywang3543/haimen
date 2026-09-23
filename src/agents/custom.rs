//! 自定义 OpenAI 兼容聊天 Agent。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;

use crate::gateway::provider::{AgentOutput, AgentProvider};

const MAX_MESSAGES: usize = 40;
const SESSION_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

struct Session {
    messages: Vec<ChatMessage>,
    touched: Instant,
}

/// 调用自定义服务的 `/chat/completions` 接口。
pub struct CustomAgent {
    endpoint: String,
    model_id: String,
    api_key: String,
    client: reqwest::Client,
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<Session>>>>,
}

impl CustomAgent {
    pub fn new(
        base_url: &str,
        model_id: &str,
        api_key: &str,
        timeout_secs: u64,
    ) -> Result<Self, String> {
        let base_url = base_url.trim().trim_end_matches('/');
        let url = reqwest::Url::parse(base_url)
            .map_err(|e| format!("自定义 Agent Base URL 无效: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("Base URL 必须是 http:// 或 https:// URL".to_string());
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Base URL 不能包含凭证、查询参数或片段".to_string());
        }
        if model_id.trim().is_empty() {
            return Err("请先配置自定义 Agent 模型 ID".to_string());
        }
        if api_key.trim().is_empty() {
            return Err("请先配置自定义 Agent API Key".to_string());
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
            .build()
            .map_err(|e| format!("创建自定义 Agent HTTP 客户端失败: {e}"))?;
        Ok(Self {
            endpoint: format!("{base_url}/chat/completions"),
            model_id: model_id.trim().to_string(),
            api_key: api_key.trim().to_string(),
            client,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    fn session(&self, session_id: Option<&str>) -> (String, Arc<AsyncMutex<Session>>) {
        let mut sessions = self.sessions.lock().expect("自定义 Agent sessions 锁中毒");
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
impl AgentProvider for CustomAgent {
    fn name(&self) -> &str {
        "custom"
    }

    async fn check_available(&self) -> Result<(), String> {
        // 兼容服务未必实现 /models；验证字段格式，实际请求由 process 检查。
        Ok(())
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
            role: "user",
            content: message.to_string(),
        });

        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({
                "model": self.model_id,
                "messages": messages,
                "stream": false,
            }))
            .send()
            .await
            .map_err(|e| format!("自定义 Agent 请求失败: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("自定义 Agent 返回 HTTP {status}"));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("自定义 Agent 响应解析失败: {e}"))?;
        let text = body
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .ok_or("自定义 Agent 响应缺少 choices[0].message.content")?
            .to_string();
        messages.push(ChatMessage {
            role: "assistant",
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
    use axum::{Json, Router, http::HeaderMap, routing::post};

    #[tokio::test]
    async fn sends_auth_model_and_conversation_history() {
        let requests = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = requests.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(
                move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                    let captured = captured.clone();
                    async move {
                        assert_eq!(headers.get("authorization").unwrap(), "Bearer test-key");
                        captured.lock().unwrap().push(body);
                        Json(serde_json::json!({"choices": [{"message": {"content": "回答"}}]}))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let agent =
            CustomAgent::new(&format!("http://{address}/v1"), "test-model", "test-key", 5).unwrap();

        let (first, sid) = agent.process("第一句", None, ".").await.unwrap();
        assert_eq!(first.text, "回答");
        let (_, resumed_sid) = agent.process("第二句", Some(&sid), ".").await.unwrap();
        assert_eq!(sid, resumed_sid);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["model"], "test-model");
        assert_eq!(requests[0]["stream"], false);
        assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 3);
        assert_eq!(requests[1]["messages"][1]["content"], "回答");
        server.abort();
    }

    #[test]
    fn requires_valid_configuration() {
        assert!(CustomAgent::new("file:///tmp", "model", "key", 5).is_err());
        assert!(CustomAgent::new("http://localhost:1234/v1", "", "key", 5).is_err());
        assert!(CustomAgent::new("http://localhost:1234/v1", "model", "", 5).is_err());
    }
}
