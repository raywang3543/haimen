//! Agent 注册表
//!
//! 将 Agent 实现的"分发"从各处硬编码 match 收敛为集中式注册：
//! 新增 Agent 只需实现 [`AgentProvider`] 并在 [`builtin`] 中注册一行，
//! 无需再改动 `gateway::build_agent` / `cli::create_agent` / web verify 等调用点。

use std::collections::HashMap;
use std::sync::OnceLock;

use super::custom::CustomAgent;
use super::ollama::OllamaAgent;
use crate::config::settings::GatewayConfig;
use crate::gateway::provider::AgentProvider;
use haimen_codex::{CodexAgent, CodexModelConfig, DEFAULT_SANDBOX};
use haimen_openclaw::{DEFAULT_AGENT_ID, OpenClawAgent, OpenClawWebSocketAgent, WebSocketConfig};

/// Agent 提供商的展示信息（供 Web API / 前端渲染）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    /// 提供商 id，与 [`AgentProvider::name`] 保持一致
    pub id: &'static str,
    /// UI 显示名
    pub display_name: &'static str,
}

/// Agent 工厂：根据配置构造一个 Agent 实例
pub type AgentFactory = fn(&GatewayConfig) -> Result<Box<dyn AgentProvider>, String>;

/// 静态 Agent 注册表
pub struct AgentRegistry {
    entries: HashMap<&'static str, (ProviderInfo, AgentFactory)>,
}

impl AgentRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// 注册一个 Agent 提供商。
    ///
    /// `id` 与 [`AgentProvider::name`] 一致；重复注册返回 `Err` 防止撞名。
    pub fn register(
        &mut self,
        id: &'static str,
        display_name: &'static str,
        factory: AgentFactory,
    ) -> Result<(), String> {
        if self.entries.contains_key(id) {
            return Err(format!("Agent 重复注册: {}", id));
        }
        self.entries
            .insert(id, (ProviderInfo { id, display_name }, factory));
        Ok(())
    }

    /// 按名称构造 Agent。未注册的名称返回与历史一致的"不支持的 AI Agent"文案。
    pub fn build(
        &self,
        name: &str,
        config: &GatewayConfig,
    ) -> Result<Box<dyn AgentProvider>, String> {
        match self.entries.get(name) {
            Some((_, factory)) => factory(config),
            None => Err(format!("不支持的 AI Agent: {}", name)),
        }
    }

    /// 列出所有已注册的提供商信息
    pub fn list(&self) -> Vec<ProviderInfo> {
        self.entries
            .values()
            .map(|(info, _)| info.clone())
            .collect()
    }

    /// 是否已注册指定名称
    pub fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 从网关配置解析 codex 沙箱策略
///
/// 优先读取 `[gateway.providers.codex] sandbox`，缺省使用
/// [`haimen_codex::DEFAULT_SANDBOX`]。`GatewayConfig` 保留在主 crate，
/// 故该解析逻辑留在注册表而非 haimen-codex crate。
fn resolve_codex_sandbox(config: &GatewayConfig) -> String {
    config
        .providers
        .get("codex")
        .and_then(|p| p.get("sandbox"))
        .cloned()
        .unwrap_or_else(|| DEFAULT_SANDBOX.to_string())
}

/// 从网关配置解析 openclaw agent id
///
/// 优先读取 `[gateway.providers.openclaw] agent`，缺省使用
/// [`haimen_openclaw::DEFAULT_AGENT_ID`]。`GatewayConfig` 保留在主 crate，
/// 故该解析逻辑留在注册表而非 haimen-openclaw crate。
fn resolve_openclaw_agent(config: &GatewayConfig) -> String {
    config
        .providers
        .get("openclaw")
        .and_then(|p| p.get("agent"))
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_string())
}

/// 从网关配置解析某 Agent 的 CLI 可执行文件路径
///
/// 优先读取 `[gateway.providers.<name>] cli_path`；空值 / 纯空白 / 未配置时
/// 回退到默认裸命令名（如 "codex"），由 `build_command` 按 PATH 查找。
/// 支持绝对路径与 Windows `.cmd` shim（`build_command` 内部处理）。
fn resolve_cli_path(config: &GatewayConfig, provider: &str, default_binary: &str) -> String {
    config
        .providers
        .get(provider)
        .and_then(|p| p.get("cli_path"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_binary.to_string())
}

/// 内置 Agent 注册（新增 Agent 只需在此加一行）
fn builtin() -> AgentRegistry {
    let mut registry = AgentRegistry::new();
    registry
        .register("codex", "Codex CLI", |config| {
            // 沙箱策略从 providers.codex.sandbox 读取，默认放开沙箱：
            // Codex 默认 workspace-write 会阻止子进程访问 macOS 钥匙串等系统资源
            let cli_path = resolve_cli_path(config, "codex", "codex");
            let sandbox = resolve_codex_sandbox(config);
            let fields = config.providers.get("codex");
            let work_dir = fields
                .and_then(|p| p.get("work_dir"))
                .map(|path| path.trim())
                .filter(|path| !path.is_empty())
                .map(crate::gateway::chat_loop::expand_tilde);
            let model_config = CodexModelConfig::new(
                fields.and_then(|p| p.get("model")).map(String::as_str),
                fields
                    .and_then(|p| p.get("model_reasoning_effort"))
                    .map(String::as_str),
            )?;
            Ok(Box::new(
                CodexAgent::new(cli_path, sandbox)
                    .with_model_config(model_config)
                    .with_work_dir(work_dir),
            ))
        })
        .expect("内置 Agent codex 注册失败");
    registry
        .register("openclaw", "OpenClaw", |config| {
            let fields = config.providers.get("openclaw");
            let transport = fields
                .and_then(|p| p.get("transport"))
                .map(String::as_str)
                .unwrap_or("cli");
            let cli_path = resolve_cli_path(config, "openclaw", "openclaw");
            let agent = resolve_openclaw_agent(config);
            let timeout = config.agent_timeout_secs;
            match transport {
                "cli" => Ok(Box::new(OpenClawAgent::new(cli_path, agent, timeout))),
                "websocket" => {
                    let mut ws = WebSocketConfig::new(
                        crate::config::settings::get_settings_dir().join("openclaw-ws-device.json"),
                    );
                    ws.agent = agent;
                    ws.timeout_secs = timeout;
                    if let Some(url) = fields.and_then(|p| p.get("gateway_url")) {
                        if !url.trim().is_empty() {
                            ws.url = url.trim().to_string();
                        }
                    }
                    if let Some(name) = fields.and_then(|p| p.get("token_env")) {
                        if !name.trim().is_empty() {
                            ws.token_env = name.trim().to_string();
                        }
                    }
                    ws.password_env = fields
                        .and_then(|p| p.get("password_env"))
                        .map(String::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    Ok(Box::new(OpenClawWebSocketAgent::new(ws)?))
                }
                other => Err(format!("不支持的 OpenClaw 连接方式: {other}")),
            }
        })
        .expect("内置 Agent openclaw 注册失败");
    registry
        .register("ollama", "Ollama", |config| {
            let fields = config.providers.get("ollama");
            let model_id = fields
                .and_then(|p| p.get("model_id"))
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .ok_or("请先配置 Ollama 模型 ID")?;
            let base_url = fields
                .and_then(|p| p.get("base_url"))
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .unwrap_or("http://localhost:11434");
            Ok(Box::new(OllamaAgent::new(
                base_url,
                model_id,
                config.agent_timeout_secs,
            )?))
        })
        .expect("内置 Agent ollama 注册失败");
    registry
        .register("custom", "自定义 Agent", |config| {
            let fields = config.providers.get("custom");
            let base_url = fields
                .and_then(|p| p.get("base_url"))
                .map(String::as_str)
                .unwrap_or("");
            let model_id = fields
                .and_then(|p| p.get("model_id"))
                .map(String::as_str)
                .unwrap_or("");
            let api_key = fields
                .and_then(|p| p.get("api_key"))
                .map(String::as_str)
                .unwrap_or("");
            let api_key = crate::config::settings::resolve_env_ref(api_key)?;
            Ok(Box::new(CustomAgent::new(
                base_url,
                model_id,
                &api_key,
                config.agent_timeout_secs,
            )?))
        })
        .expect("内置 Agent custom 注册失败");
    registry
}

static REGISTRY: OnceLock<AgentRegistry> = OnceLock::new();

/// 获取全局注册表（惰性初始化，不可变）
pub fn registry() -> &'static AgentRegistry {
    REGISTRY.get_or_init(builtin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> GatewayConfig {
        GatewayConfig::default()
    }

    #[test]
    fn test_builtin_registers_default_provider() {
        // 默认 active_provider 是 "codex"，必须恒注册，否则默认启动即报错
        assert!(registry().has("codex"));
        assert!(registry().has("openclaw"));
        assert!(registry().has("ollama"));
        assert!(registry().has("custom"));
        assert!(!registry().has("claude-code"));
        assert!(!registry().has("hermes"));
    }

    #[test]
    fn test_build_known_agent() {
        let codex = registry()
            .build("codex", &test_config())
            .expect("codex 应可构造");
        assert_eq!(codex.name(), "codex");

        let openclaw = registry()
            .build("openclaw", &test_config())
            .expect("openclaw 应可构造");
        assert_eq!(openclaw.name(), "openclaw");
    }

    #[test]
    fn test_openclaw_websocket_is_opt_in() {
        let mut config = test_config();
        config.providers.insert(
            "openclaw".to_string(),
            HashMap::from([
                ("transport".to_string(), "websocket".to_string()),
                (
                    "gateway_url".to_string(),
                    "ws://127.0.0.1:18789".to_string(),
                ),
            ]),
        );
        assert_eq!(
            registry().build("openclaw", &config).unwrap().name(),
            "openclaw"
        );
        config
            .providers
            .get_mut("openclaw")
            .unwrap()
            .insert("transport".to_string(), "unknown".to_string());
        assert!(registry().build("openclaw", &config).is_err());
    }

    #[test]
    fn test_build_custom_agent_requires_fields() {
        assert!(registry().build("custom", &test_config()).is_err());
        let mut config = test_config();
        config.providers.insert(
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
        assert_eq!(
            registry().build("custom", &config).unwrap().name(),
            "custom"
        );
    }

    #[test]
    fn test_build_unknown_agent() {
        let result = registry().build("unknown-agent", &test_config());
        match result {
            Ok(_) => panic!("未知 agent 应返回 Err"),
            Err(err) => assert_eq!(err, "不支持的 AI Agent: unknown-agent"),
        }
    }

    #[test]
    fn test_list_contains_builtin() {
        let list = registry().list();
        let ids: Vec<&str> = list.iter().map(|info| info.id).collect();
        assert!(ids.contains(&"codex"));
        assert!(ids.contains(&"openclaw"));
        assert!(ids.contains(&"ollama"));
        assert!(ids.contains(&"custom"));
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn test_duplicate_registration_rejected() {
        let mut reg = AgentRegistry::new();
        reg.register("dup", "Dup", |_c| {
            Ok(Box::new(CodexAgent::new("codex", DEFAULT_SANDBOX)))
        })
        .expect("首次注册应成功");
        let err = reg
            .register("dup", "Dup 2", |_c| {
                Ok(Box::new(CodexAgent::new("codex", DEFAULT_SANDBOX)))
            })
            .expect_err("重复注册应返回 Err");
        assert_eq!(err, "Agent 重复注册: dup");
    }

    #[test]
    fn test_factory_receives_config() {
        // 验证工厂能拿到 config。
        let mut reg = AgentRegistry::new();
        reg.register("cfg-agent", "Cfg", |config| {
            let wd = config.work_dir.clone().unwrap_or_default();
            if wd.is_empty() {
                Ok(Box::new(CodexAgent::new("codex", DEFAULT_SANDBOX)))
            } else {
                Err("不应走到".to_string())
            }
        })
        .expect("注册成功");
        let agent = reg.build("cfg-agent", &test_config()).expect("构造成功");
        assert_eq!(agent.name(), "codex");
    }

    #[test]
    fn test_codex_rejects_invalid_reasoning_effort() {
        let mut config = test_config();
        config.providers.insert(
            "codex".into(),
            HashMap::from([("model_reasoning_effort".into(), "invalid".into())]),
        );
        let error = registry().build("codex", &config).err().unwrap();
        assert!(error.contains("思考强度"));
    }

    #[test]
    fn test_resolve_codex_sandbox_default() {
        // 未配置时回退到默认（放开沙箱）
        let config = GatewayConfig::default();
        assert_eq!(resolve_codex_sandbox(&config), DEFAULT_SANDBOX);
    }

    #[test]
    fn test_resolve_codex_sandbox_custom() {
        // 配置 [gateway.providers.codex] sandbox 后应被读取
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("sandbox".to_string(), "workspace-write".to_string());
        providers.insert("codex".to_string(), params);
        config.providers = providers;
        assert_eq!(resolve_codex_sandbox(&config), "workspace-write");
    }

    #[test]
    fn test_resolve_codex_sandbox_ignores_other_providers() {
        // 其他 provider 的 sandbox 配置不影响 codex
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("sandbox".to_string(), "read-only".to_string());
        providers.insert("custom".to_string(), params);
        config.providers = providers;
        assert_eq!(resolve_codex_sandbox(&config), DEFAULT_SANDBOX);
    }

    #[test]
    fn test_resolve_openclaw_agent_default() {
        // 未配置时回退到默认 agent
        let config = GatewayConfig::default();
        assert_eq!(resolve_openclaw_agent(&config), DEFAULT_AGENT_ID);
    }

    #[test]
    fn test_resolve_openclaw_agent_custom() {
        // 配置 [gateway.providers.openclaw] agent 后应被读取
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("agent".to_string(), "ops".to_string());
        providers.insert("openclaw".to_string(), params);
        config.providers = providers;
        assert_eq!(resolve_openclaw_agent(&config), "ops");
    }

    #[test]
    fn test_resolve_openclaw_agent_ignores_other_providers() {
        // 其他 provider 的 agent 配置不影响 openclaw
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("agent".to_string(), "whatever".to_string());
        providers.insert("codex".to_string(), params);
        config.providers = providers;
        assert_eq!(resolve_openclaw_agent(&config), DEFAULT_AGENT_ID);
    }

    #[test]
    fn test_resolve_cli_path_default() {
        // 未配置时回退到默认裸命令名（PATH 查找）
        let config = GatewayConfig::default();
        assert_eq!(resolve_cli_path(&config, "codex", "codex"), "codex");
    }

    #[test]
    fn test_resolve_cli_path_custom() {
        // 配置 [gateway.providers.codex] cli_path 后应被读取
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("cli_path".to_string(), "/opt/codex/bin/codex".to_string());
        providers.insert("codex".to_string(), params);
        config.providers = providers;
        assert_eq!(
            resolve_cli_path(&config, "codex", "codex"),
            "/opt/codex/bin/codex"
        );
    }

    #[test]
    fn test_resolve_cli_path_empty_falls_back() {
        // 显式空串/纯空白回退到默认裸命令名
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("cli_path".to_string(), "   ".to_string());
        providers.insert("codex".to_string(), params);
        config.providers = providers;
        assert_eq!(resolve_cli_path(&config, "codex", "codex"), "codex");
    }

    #[test]
    fn test_resolve_cli_path_ignores_other_providers() {
        // 其他 provider 的 cli_path 配置不影响目标 provider
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        let mut params = HashMap::new();
        params.insert("cli_path".to_string(), "/weird/path".to_string());
        providers.insert("codex".to_string(), params);
        config.providers = providers;
        assert_eq!(
            resolve_cli_path(&config, "openclaw", "openclaw"),
            "openclaw"
        );
    }
}
