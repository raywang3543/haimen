# 海门 - Haimen

<p align="center">
  <img src="docs/public/logo.svg" alt="haimen logo" width="300" />
</p>

<p align="center">
  <a href="https://crates.io/crates/haimen"><img src="https://img.shields.io/crates/v/haimen.svg?color=brightgreen" alt="crates.io"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-brightgreen.svg" alt="License: MIT"></a>
</p>

**haimen** 是一个 AI 网关基建 CLI 工具，支持多种消息渠道（飞书/Lark、钉钉、GitHub Webhook）和多种 AI 后端（Ollama、Custom、Codex CLI、OpenClaw）。

## 安装

### 方式一：一键安装脚本（推荐）

从 GitHub Release 下载并运行 cargo-dist 安装器，脚本会自动检测平台、下载对应二进制归档、验证完整性，并配置 PATH。

**macOS / Linux**

```bash
curl -fsSL https://github.com/shenjingnan/haimen/releases/latest/download/haimen-installer.sh | sh
```

**Windows (PowerShell)**

在 PowerShell 中（推荐）：

```powershell
irm https://github.com/shenjingnan/haimen/releases/latest/download/haimen-installer.ps1 | iex
```

在 cmd.exe 中：

```powershell
powershell -c "irm https://github.com/shenjingnan/haimen/releases/latest/download/haimen-installer.ps1 | iex"
```

> Windows 二进制依赖 **Microsoft Visual C++ Redistributable**（大多数系统已内置），若运行时报缺少 `vcruntime140.dll`，请先安装 [VC++ 运行库](https://aka.ms/vs/17/release/vc_redist.x64.exe)。

**国内用户（中国大陆）**：如果 GitHub 访问缓慢，可使用 Gitee 镜像安装

**macOS / Linux**

```bash
curl -fsSL https://gitee.com/shenjingnan/haimen/raw/main/docs/public/install-gitee.sh | sh
```

**Windows (PowerShell)**

在 PowerShell 中（推荐）：

```powershell
irm https://gitee.com/shenjingnan/haimen/raw/main/docs/public/install-gitee.ps1 | iex
```

在 cmd.exe 中：

```powershell
powershell -c "irm https://gitee.com/shenjingnan/haimen/raw/main/docs/public/install-gitee.ps1 | iex"
```

### 方式二：cargo install

```bash
cargo install haimen
```

### 方式三：手动下载

从 [GitHub Releases](https://github.com/shenjingnan/haimen/releases) 下载对应平台的压缩包，解压后放入 `PATH`。

| 平台    | 架构          | 文件名                                    |
| ------- | ------------- | ----------------------------------------- |
| macOS   | Intel         | `haimen-x86_64-apple-darwin.tar.xz`       |
| macOS   | Apple Silicon | `haimen-aarch64-apple-darwin.tar.xz`      |
| Linux   | x86_64        | `haimen-x86_64-unknown-linux-gnu.tar.xz`  |
| Linux   | ARM64         | `haimen-aarch64-unknown-linux-gnu.tar.xz` |
| Windows | x86_64        | `haimen-x86_64-pc-windows-msvc.zip`       |

> Windows on ARM（aarch64）暂不支持，`haimen upgrade` 会明确报错。

### 升级

```bash
haimen upgrade
```

### 卸载

```bash
haimen uninstall
```

## 特性

- **多消息渠道** — 集成飞书/Lark、钉钉、Relay 中转、GitHub Webhook，统一消息模型
- **多 AI 后端** — 支持 Ollama、Custom、Codex CLI、OpenClaw
- **小智 AI 硬件** — 原生支持 小智 AI 聊天硬件（WebSocket 音频流协议）
- **Web 管理控制台** — 内置 HTTP 服务器 + React SPA，管理配置、Agent 和语音
- **TOML 配置管理** — 支持多服务商配置和环境变量引用 `${env.VAR}`
- **双层日志** — 基于 tracing 的日志系统，同时输出到文件和 stderr
- **Shell 补全** — 支持 bash / zsh / fish / powershell / elvish 自动补全

## 支持列表

### AI Agent

| 名称       | 类型          | 说明                       |
| ---------- | ------------- | -------------------------- |
| Ollama     | AgentProvider | 本地模型 API               |
| Custom     | AgentProvider | OpenAI 兼容 API            |
| Codex CLI  | AgentProvider | Codex CLI 集成             |
| OpenClaw   | AgentProvider | OpenClaw CLI（默认）或 Gateway WebSocket |

### 消息渠道

| 名称         | 类型           | 连接方式                |
| ------------ | -------------- | ----------------------- |
| 飞书 / Lark  | MessageChannel | lark-cli 子进程桥接     |
| 钉钉         | MessageChannel | 直连 Web API            |
| Relay 中转   | MessageChannel | 主动连接公网 WebSocket  |
| GitHub       | WebhookHandler | Webhook + @mention 触发 |
| 小智 AI 硬件 | WebSocket      | 音频流协议直连          |

### 图片消息

飞书/Lark 通道会根据飞书消息中的图片资源 ID 下载图片，并将图片转换为网关内部的统一附件格式。自定义消息通道接入图片时，也应将图片填入统一消息的 `images` 字段：

```json
{
  "mime_type": "image/png",
  "data_base64": "iVBORw0KGgo..."
}
```

- `mime_type` 支持 `image/png`、`image/jpeg`、`image/gif`、`image/webp`。
- `data_base64` 是图片原始字节的标准 Base64 编码，不带 `data:image/...;base64,` 前缀。
- 单张图片解码后最大为 10 MiB。

以上是网关内部 `ImageData` 结构，**不是当前 Relay 客户端协议**。Relay 目前只接收 `payload.text`，并将图片列表设为空；Relay 客户端暂时不能通过现有协议发送图片。飞书图片由网关自动下载，不需要客户端自行编码成 Base64。

小智 WebSocket 的本地 `text` 扩展可在文字消息中附带一张图片，使用同样的图片字段：`{"type":"text","text":"请描述图片","images":[{"mime_type":"image/png","data_base64":"..."}]}`。服务端校验 MIME、图片文件头、Base64 和解码后的 10 MiB 上限，再将图片交给当前 Agent。该格式适用于小智 WebSocket，不会改变 Relay 协议。

图片能否交给 AI 处理还取决于 Agent：当前 Codex、Ollama、Custom 和 OpenClaw WebSocket 支持图片输入；OpenClaw CLI 模式暂不支持。所选模型也必须支持视觉输入，OpenClaw Gateway 还会校验图片和 WebSocket 请求大小。

## 快速开始

```bash
# 启动所有启用的连接器和 Agent
haimen start
```

## CLI 命令

```
haimen — AI 网关基建 CLI

USAGE:
  haimen [COMMAND]

COMMANDS:
  config              显示配置信息
  start               启动所有启用的连接器和 Agent
    --echo            回声模式（消息原样返回，不经过 Agent）
    --open-browser    启动成功后自动打开浏览器打开 Web 控制台
    --log-level       终端日志级别（默认关闭终端日志，仅记录到文件）
  agent               AI Agent 调试
    run               单次运行 Agent
      <PROMPT>        发送给 Agent 的消息（位置参数）
      --provider      Agent 提供者（ollama / custom / codex / openclaw）
    chat              交互式 Agent 会话（支持 resume）
      --provider      Agent 提供者（ollama / custom / codex / openclaw）
    log               查看 Agent 调用日志
      --limit         显示条数（默认 20）
      --day           只显示指定日期 (YYYY-MM-DD)
      --source        只显示指定来源（网关 / 语音 / CLI 调试）
      --chat          只显示指定会话 chat_id
      --json          以 JSON 数组输出
  serve               启动 HTTP Web 服务器（xiaozhi WebSocket + GitHub Webhook）
    --host            监听地址（默认 0.0.0.0）
    --port            监听端口（默认 9527）
    --no-browser      不自动打开浏览器
    --xiaozhi-echo    Echo 模式
    --xiaozhi-llm     ASR → AI → TTS 模式（默认）
    --xiaozhi-asr-tts ASR-TTS 回声模式
    --xiaozhi-llm-provider  LLM 提供者
    --xiaozhi-tts-text      TTS 测试文本
    --xiaozhi-tts-voice     TTS 音色
  completion <SHELL>  生成 Shell 补全脚本（bash / zsh / fish / powershell / elvish）
  upgrade             升级 haimen 到最新版本
  uninstall           卸载 haimen
```

## 配置

配置文件位于 `~/.haimen/settings.toml`：

```toml
debug = false
log_level = "info"

[http]
enabled = true
host = "0.0.0.0"
port = 9527
auto_open_browser = true

# 连接器配置
[connectors.lark]
enabled = true
lark_cli_path = "lark-cli"

[connectors.dingtalk]
enabled = true
client_id = "xxx"
client_secret = "${env.DINGTALK_CLIENT_SECRET}"

# 可选：通过独立 haimen-relay 服务接入设备
[connectors.relay]
enabled = true
url = "wss://relay.example.com/ws"
pair = "${file:./.env#pair}"
token = "${file:./.env#token}"

# 在项目根目录的 .env 中写入配对凭证（该文件已被 Git 忽略）：
# pair = "haimenrelay"
# token = "test1234"

# AI 网关配置（支持多服务商）
[gateway]
active_provider = "codex"

[gateway.providers.codex]
# CLI 工具无需额外凭证
# 可选：codex CLI 可执行文件路径（留空按 PATH 查找 "codex"）
# cli_path = "/opt/codex/bin/codex"

[gateway.providers.openclaw]
# 默认 transport = "cli"：无需额外凭证；建议 openclaw gateway 常驻（缺失时自动降级 embedded）
# 可选：openclaw agent id（默认 "main"，OpenClaw 保留 agent）
# agent = "ops"
# 可选：openclaw CLI 可执行文件路径（留空按 PATH 查找 "openclaw"）
# cli_path = "/opt/openclaw/bin/openclaw"
# 如需试用 Gateway WebSocket，改为 transport = "websocket"（CLI 模式仍可切回）
# transport = "websocket"
# gateway_url = "ws://127.0.0.1:18789"
# token_env = "OPENCLAW_GATEWAY_TOKEN"  # 环境变量名，不是 Token 本身
# password_env = "OPENCLAW_GATEWAY_PASSWORD"  # 仅密码鉴权时需要

# WebSocket 首次连接需要 Gateway 地址，以及 Gateway Token 或密码（通过环境变量传入）。
# 如果 Gateway 要求设备配对，根据报错中的 requestId 在 OpenClaw 主机执行：
# openclaw devices list
# openclaw devices approve <requestId>
# haimen 会将设备身份和配对后返回的 deviceToken 保存到 ~/.haimen/openclaw-ws-device.json；
# 后续可用已保存的 deviceToken 连接。WebSocket 模式使用 OpenClaw agent 自己配置的工作目录。

[gateway.providers.ollama]
# 先运行 ollama pull qwen3:8b；模型 ID 必填
model_id = "qwen3:8b"
# 可选：默认 http://localhost:11434
# base_url = "http://localhost:11434"

[gateway.providers.custom]
# OpenAI 兼容接口的 API 根地址（请求路径为 /chat/completions）
base_url = "https://api.example.com/v1"
model_id = "my-model"
api_key = "${env.CUSTOM_AI_API_KEY}"

# ASR 配置（小智硬件，支持多服务商）
[asr]
active_provider = "doubao"

[asr.providers.doubao]
api_key = "${env.DOUBAO_API_KEY}"

[asr.providers.qwen]
api_key = "${env.QWEN_API_KEY}"

# 可选：使用项目下 models/sensevoice-small-v1 的本地离线模型
# 该目录需包含 model.int8.onnx 和 tokens.txt
[asr.providers.sensevoice]
# model_dir = "/absolute/path/to/sensevoice-small-v1"  # 模型放在其他位置时设置

# TTS 配置（小智硬件，支持多服务商）
[tts]
active_provider = "doubao"

[tts.providers.doubao]
api_key = "${env.DOUBAO_API_KEY}"
voice = "zh_female_xiaohe_uranus_bigtts"

[tts.providers.openai]
api_key = "${env.OPENAI_API_KEY}"
voice = "alloy"
model = "tts-1"

[tts.providers.edge]
voice = "zh-CN-XiaoxiaoNeural"
```

使用本地 SenseVoice 时，将 `[asr].active_provider` 设为 `"sensevoice"`，或在 Web 控制台的 ASR 页面选择「SenseVoice 本地」并设为首选。默认读取项目 `models/sensevoice-small-v1/`；服务从其他目录运行或模型放在别处时，请在 `model_dir` 中填写绝对路径。该模型是整段识别，录音由本地音量检测判停，录音结束后返回识别文本；模型文件需自行放置，不随 haimen 二进制提供。

## 通过 API 切换首选 Agent

先在 Web 控制台或 `~/.haimen/settings.toml` 配置目标 Agent，然后请求：

```bash
curl -X PUT http://127.0.0.1:9527/api/v1/settings/agent/active \
  -H 'Content-Type: application/json' \
  -d '{"provider":"codex"}'
```

`provider` 可填已注册的 Agent ID；Web 控制台展示 `openclaw`、`codex`、`ollama`、`custom`。成功响应示例：

```json
{"success":true,"data":{"active_provider":"codex","applied":true,"applied_agent":"codex","generation":3}}
```

接口只修改首选 Agent，保留各提供商参数；切换立即生效，旧会话重置。目标已生效时返回 `applied: false`，不会重置会话。缺少 `provider`、ID 不受支持或目标 Agent 不可用时返回 HTTP 400，配置和当前 Agent 保持不变。

查询当前生效 Agent（响应不含提供商参数）：

```bash
curl http://127.0.0.1:9527/api/v1/settings/agent/active
```

## 许可

[MIT](LICENSE)
