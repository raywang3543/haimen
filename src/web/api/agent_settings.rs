//! Agent 配置 REST API
//!
//! 提供对 `settings.toml` 中 `[gateway]` 的 Agent 配置的读写接口。
//!
//! 与 ASR/TTS 一致，Agent 共享句柄经 axum State 注入：`PUT` 保存配置后
//! 重建 Agent 并换入共享状态，实现运行时热切换（无需重启）。
//!
//! # 端点
//!
//! - `GET /api/v1/settings/agent` — 获取 Agent 配置（active_provider + providers + 当前生效 agent）
//! - `PUT /api/v1/settings/agent` — 更新 Agent 配置并热切换
//! - `GET /api/v1/settings/agent/active` — 查询当前首选 Agent（不返回凭证）
//! - `PUT /api/v1/settings/agent/active` — 仅切换首选 Agent
//! - `GET /api/v1/agent/providers` — 列出注册表中所有 Agent 提供商
//! - `POST /api/v1/settings/agent/verify` — 验证指定 Agent 是否可用

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::{Json, http::StatusCode};

use crate::config::settings::AppConfig;

// ═══════════════════════════════════════════════════════════════════════════════
// 工具函数
// ═══════════════════════════════════════════════════════════════════════════════

/// 从请求 JSON 中读取 providers 映射
fn parse_providers(value: &serde_json::Value) -> Option<HashMap<String, HashMap<String, String>>> {
    let obj = value.get("providers")?.as_object()?;
    let mut providers = HashMap::new();
    for (name, fields) in obj {
        if let Some(fields_obj) = fields.as_object() {
            let mut creds = HashMap::new();
            for (k, v) in fields_obj {
                if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                    creds.insert(k.clone(), s.to_string());
                }
            }
            providers.insert(name.clone(), creds);
        }
    }
    Some(providers)
}

/// 加载当前配置（文件不存在时返回默认值）
fn load_config() -> AppConfig {
    crate::config::settings::load_settings()
        .ok()
        .flatten()
        .unwrap_or_default()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Agent 端点
// ═══════════════════════════════════════════════════════════════════════════════

/// `GET /api/v1/settings/agent`
///
/// 返回完整的 Agent 配置：所有已配置的提供商参数 + 当前激活的提供商 +
/// 运行时实际生效的 Agent（`applied_agent` + `generation`）。
pub async fn get_agent_settings(
    State(shared_agent): State<crate::gateway::agent_handle::SharedAgent>,
) -> Json<serde_json::Value> {
    let cfg = load_config();
    Json(serde_json::json!({
        "success": true,
        "data": {
            "active_provider": cfg.gateway.active_provider,
            "providers": cfg.gateway.providers,
            "resolved": {
                "agent": cfg.gateway.resolved_agent(),
            },
            "applied_agent": crate::gateway::agent_handle::current_name(&shared_agent),
            "generation": crate::gateway::agent_handle::current_generation(&shared_agent),
        }
    }))
}

/// `GET /api/v1/settings/agent/active`
///
/// 返回配置中的首选 Agent 和运行时生效状态，不包含提供商参数。
pub async fn get_active_agent(
    State(shared_agent): State<crate::gateway::agent_handle::SharedAgent>,
) -> Json<serde_json::Value> {
    let cfg = load_config();
    let snapshot = crate::gateway::agent_handle::snapshot(&shared_agent);
    Json(serde_json::json!({
        "success": true,
        "data": {
            "active_provider": cfg.gateway.active_provider,
            "applied_agent": snapshot.name,
            "generation": snapshot.generation,
        }
    }))
}

/// `PUT /api/v1/settings/agent`
///
/// 替换完整的 Agent 配置并热切换。支持以下字段：
/// - `active_provider` — 切换当前激活的 Agent 提供商
/// - `providers` — 所有提供商的完整参数映射
///
/// 原子切换顺序：**构建候选 → check_available → 写盘 → 换入内存**。
/// 任一步失败返回 4xx/5xx，磁盘与内存都停留在旧配置（自动回滚）。
/// 成功后下一次消息/事件即使用新 Agent；会话因换代自动重置。
pub async fn update_agent_settings(
    State(shared_agent): State<crate::gateway::agent_handle::SharedAgent>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let mut cfg = load_config();

    // 更新 active_provider（如果提供）
    if let Some(active) = body.get("active_provider").and_then(|v| v.as_str()) {
        if !active.is_empty() {
            cfg.gateway.active_provider = active.to_string();
        }
    }

    // 更新 providers（如果提供）
    if let Some(providers) = parse_providers(&body) {
        cfg.gateway.providers = providers;
    }

    let name = cfg.gateway.resolved_agent();

    // 1. 构建候选（未注册的 provider → 400，零改动）
    let candidate = match crate::agents::registry::registry().build(&name, &cfg.gateway) {
        Ok(a) => a,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": e.clone(),
                    "message": e,
                })),
            ));
        }
    };

    // 2. check_available（Agent 不可用 → 400，零改动，旧 Agent 继续工作）
    if let Err(e) = candidate.check_available().await {
        let msg = format!("{} 不可用，配置未保存: {}", name, e);
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": msg.clone(),
                "message": msg,
            })),
        ));
    }

    // 3. 写盘（失败 → 500，内存未换入）
    if let Err(e) = crate::config::settings::save_settings(&cfg) {
        tracing::warn!(error = %e, "保存 Agent 配置到文件失败");
        let msg = format!("保存配置失败: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "success": false,
                "error": msg.clone(),
                "message": msg,
            })),
        ));
    }

    // 4. 换入内存（此刻才换代，单条赋值原子生效）
    let (applied_agent, generation) = {
        let mut guard = shared_agent.write().expect("agent handle 锁中毒");
        guard.swap(Arc::from(candidate));
        (guard.name.clone(), guard.generation)
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "active_provider": cfg.gateway.active_provider,
            "providers": cfg.gateway.providers,
            "applied": true,
            "applied_agent": applied_agent,
            "generation": generation,
        }
    })))
}

/// `PUT /api/v1/settings/agent/active`
///
/// 请求体：`{"provider":"codex"}`。保留所有提供商参数，只切换首选 Agent。
/// 沿用配置更新接口的可用性检查与原子热切换；已是当前 Agent 时不重置会话。
/// 响应仅返回切换状态，不返回已保存的凭证。
pub async fn set_active_agent(
    State(shared_agent): State<crate::gateway::agent_handle::SharedAgent>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let provider = body
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": "provider 必须是非空字符串",
                    "message": "provider 必须是非空字符串",
                })),
            )
        })?;
    if !crate::agents::registry::registry().has(provider) {
        let message = format!("不支持的 AI Agent: {provider}");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": message,
                "message": message,
            })),
        ));
    }

    let current = load_config();
    let snapshot = crate::gateway::agent_handle::snapshot(&shared_agent);
    if current.gateway.active_provider == provider && snapshot.name == provider {
        return Ok(Json(serde_json::json!({
            "success": true,
            "data": {
                "active_provider": provider,
                "applied": false,
                "applied_agent": snapshot.name,
                "generation": snapshot.generation,
            }
        })));
    }

    let Json(updated) = update_agent_settings(
        State(shared_agent),
        Json(serde_json::json!({"active_provider": provider})),
    )
    .await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "active_provider": updated["data"]["active_provider"],
            "applied": true,
            "applied_agent": updated["data"]["applied_agent"],
            "generation": updated["data"]["generation"],
        }
    })))
}

/// `GET /api/v1/agent/providers`
///
/// 返回注册表中所有可用的 Agent 提供商（id + display_name）。
/// 前端据此动态渲染提供商 Tab，新增 Agent 后无需改动前端。
pub async fn list_agent_providers() -> Json<serde_json::Value> {
    let providers: Vec<serde_json::Value> = crate::agents::registry::registry()
        .list()
        .into_iter()
        .map(|info| {
            serde_json::json!({
                "id": info.id,
                "display_name": info.display_name,
            })
        })
        .collect();

    Json(serde_json::json!({
        "success": true,
        "data": { "providers": providers }
    }))
}

/// `POST /api/v1/settings/agent/verify`
///
/// 验证指定 Agent 提供商是否可用。
///
/// 请求体包含 `provider` 字段；可选 `fields` 用于验证**草稿**配置
/// （尚未保存到配置）。统一通过注册表构造 Agent 并调用其
/// `check_available()`（如 CLI 版本检查或 Ollama 模型检查），
/// 新增 Agent 无需改动此处。
pub async fn verify_agent_credentials(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-code");

    let mut cfg = load_config();

    // 草稿字段覆盖已保存配置；不写盘、不改内存。
    if let Some(fields) = body.get("fields").and_then(|v| v.as_object()) {
        let provider_fields = cfg
            .gateway
            .providers
            .entry(provider.to_string())
            .or_default();
        for (key, value) in fields {
            if let Some(value) = value.as_str() {
                if value.trim().is_empty() {
                    provider_fields.remove(key);
                } else {
                    provider_fields.insert(key.clone(), value.to_string());
                }
            }
        }
    }
    // 保留旧客户端直接传 cli_path 的行为。
    if let Some(path) = body
        .get("cli_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        cfg.gateway
            .providers
            .entry(provider.to_string())
            .or_default()
            .insert("cli_path".to_string(), path.trim().to_string());
    }

    let agent = match crate::agents::registry::registry().build(provider, &cfg.gateway) {
        Ok(agent) => agent,
        Err(e) => {
            return Ok(Json(serde_json::json!({
                "success": true,
                "data": { "valid": false, "message": e }
            })));
        }
    };

    match agent.check_available().await {
        Ok(()) => Ok(Json(serde_json::json!({
            "success": true,
            "data": { "valid": true, "message": if provider == "custom" {
                "配置格式有效；服务连接和 API Key 将在发送消息时验证".to_string()
            } else if provider == "codex" {
                "Codex CLI 可用；模型权限和思考强度兼容性将在发送消息时验证".to_string()
            } else {
                format!("{} 可用", agent.name())
            } }
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "success": true,
            "data": { "valid": false, "message": e }
        }))),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_providers_valid() {
        let json = serde_json::json!({
            "providers": {
                "claude-code": {
                    "note": "CLI 工具无需额外凭证"
                },
                "codex": {
                    "note": "CLI 工具无需额外凭证"
                }
            }
        });
        let providers = parse_providers(&json).unwrap();
        assert_eq!(providers.len(), 2);
        assert!(providers.contains_key("claude-code"));
        assert!(providers.contains_key("codex"));
    }

    #[test]
    fn test_parse_providers_skips_empty() {
        let json = serde_json::json!({
            "providers": {
                "claude-code": {
                    "note": ""
                }
            }
        });
        let providers = parse_providers(&json).unwrap();
        let cc = providers.get("claude-code").unwrap();
        assert!(cc.get("note").is_none());
    }

    #[test]
    fn test_parse_providers_missing() {
        let json = serde_json::json!({"other": "value"});
        assert!(parse_providers(&json).is_none());
    }

    // ─── 热切换原子性（回滚）测试 ─────────────────────────

    use crate::gateway::provider::{AgentOutput, AgentProvider};
    use crate::test_util::run_with_temp_home_async;
    use async_trait::async_trait;

    struct MockAgent;

    #[async_trait]
    impl AgentProvider for MockAgent {
        fn name(&self) -> &str {
            "mock-agent"
        }

        async fn check_available(&self) -> Result<(), String> {
            Ok(())
        }

        async fn process(
            &self,
            _msg: &str,
            _session_id: Option<&str>,
            _work_dir: &str,
        ) -> Result<(AgentOutput, String), String> {
            Ok((AgentOutput::default(), "mock-session".to_string()))
        }
    }

    #[tokio::test]
    async fn test_invalid_codex_effort_does_not_save_or_swap() {
        run_with_temp_home_async(move |_home| async move {
            let before = load_config();
            let shared = crate::gateway::agent_handle::into_shared(Box::new(MockAgent));
            let body = Json(serde_json::json!({
                "active_provider": "codex",
                "providers": {"codex": {"model_reasoning_effort": "invalid"}}
            }));
            let (status, payload) = update_agent_settings(State(shared.clone()), body)
                .await
                .expect_err("invalid effort must fail before checking CLI availability");
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(payload.0["message"].as_str().unwrap().contains("思考强度"));
            assert_eq!(load_config().gateway, before.gateway);
            assert_eq!(crate::gateway::agent_handle::current_generation(&shared), 0);
        })
        .await;
    }

    #[tokio::test]
    async fn test_update_unknown_provider_rolls_back() {
        run_with_temp_home_async(move |_home| async move {
            let shared = crate::gateway::agent_handle::into_shared(Box::new(MockAgent));
            let body = Json(serde_json::json!({ "active_provider": "does-not-exist" }));

            let result = update_agent_settings(axum::extract::State(shared.clone()), body).await;
            let (status, payload) = result.expect_err("未知 provider 应返回 400");
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let msg = payload.0["message"].as_str().unwrap_or_default();
            assert!(
                msg.contains("does-not-exist"),
                "错误信息应包含 provider 名，实际: {}",
                msg
            );

            // 回滚：共享状态保持原样（未换代）
            let snap = crate::gateway::agent_handle::snapshot(&shared);
            assert_eq!(snap.name, "mock-agent");
            assert_eq!(snap.generation, 0);
        })
        .await;
    }

    #[tokio::test]
    async fn test_set_active_agent_preserves_config_and_skips_repeated_switch() {
        run_with_temp_home_async(move |_home| async move {
            let mut cfg = AppConfig::default();
            cfg.gateway.providers.insert(
                "custom".to_string(),
                HashMap::from([
                    (
                        "base_url".to_string(),
                        "https://api.example.com/v1".to_string(),
                    ),
                    ("model_id".to_string(), "example-model".to_string()),
                    ("api_key".to_string(), "test-key".to_string()),
                ]),
            );
            cfg.gateway.providers.insert(
                "codex".to_string(),
                HashMap::from([("cli_path".to_string(), "/opt/codex".to_string())]),
            );
            crate::config::settings::save_settings(&cfg).unwrap();

            let shared = crate::gateway::agent_handle::into_shared(Box::new(MockAgent));
            let Json(result) = set_active_agent(
                State(shared.clone()),
                Json(serde_json::json!({"provider": "custom"})),
            )
            .await
            .unwrap();
            assert_eq!(result["data"]["active_provider"], "custom");
            assert_eq!(result["data"]["applied"], true);
            assert_eq!(result["data"]["generation"], 1);
            assert!(result["data"].get("providers").is_none());
            assert_eq!(
                crate::gateway::agent_handle::current_name(&shared),
                "custom"
            );
            let saved = crate::config::settings::load_settings().unwrap().unwrap();
            assert_eq!(saved.gateway.active_provider, "custom");
            assert_eq!(saved.gateway.providers, cfg.gateway.providers);
            let Json(current) = get_active_agent(State(shared.clone())).await;
            assert_eq!(current["data"]["active_provider"], "custom");
            assert_eq!(current["data"]["applied_agent"], "custom");
            assert!(current["data"].get("providers").is_none());

            let Json(repeated) = set_active_agent(
                State(shared.clone()),
                Json(serde_json::json!({"provider": "custom"})),
            )
            .await
            .unwrap();
            assert_eq!(repeated["data"]["applied"], false);
            assert_eq!(repeated["data"]["generation"], 1);
            assert_eq!(crate::gateway::agent_handle::current_generation(&shared), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn test_set_active_agent_rejects_invalid_provider_without_switching() {
        run_with_temp_home_async(move |_home| async move {
            let shared = crate::gateway::agent_handle::into_shared(Box::new(MockAgent));
            for body in [
                serde_json::json!({}),
                serde_json::json!({"provider": "  "}),
                serde_json::json!({"provider": "does-not-exist"}),
            ] {
                let (status, payload) = set_active_agent(State(shared.clone()), Json(body))
                    .await
                    .expect_err("无效 provider 应返回 400");
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(payload.0["success"], false);
            }
            assert_eq!(crate::gateway::agent_handle::current_generation(&shared), 0);
            assert!(crate::config::settings::load_settings().unwrap().is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn test_verify_ollama_draft_fields() {
        run_with_temp_home_async(move |_home| async move {
            let app = axum::Router::new().route(
                "/api/tags",
                axum::routing::get(|| async {
                    Json(serde_json::json!({"models": [{"name": "qwen3:8b"}]}))
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

            let result = verify_agent_credentials(Json(serde_json::json!({
                "provider": "ollama",
                "fields": {
                    "model_id": "qwen3:8b",
                    "base_url": format!("http://{address}"),
                }
            })))
            .await
            .unwrap();
            assert_eq!(result.0["data"]["valid"], true);
            assert!(crate::config::settings::load_settings().unwrap().is_none());
            server.abort();
        })
        .await;
    }

    #[tokio::test]
    async fn test_verify_custom_draft_fields_checks_format_only() {
        run_with_temp_home_async(move |_home| async move {
            let result = verify_agent_credentials(Json(serde_json::json!({
                "provider": "custom",
                "fields": {
                    "base_url": "https://api.example.com/v1",
                    "model_id": "example-model",
                    "api_key": "test-key",
                }
            })))
            .await
            .unwrap();
            assert_eq!(result.0["data"]["valid"], true);
            assert!(
                result.0["data"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("将在发送消息时验证")
            );
            assert!(crate::config::settings::load_settings().unwrap().is_none());
        })
        .await;
    }
}
