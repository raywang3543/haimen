# 小冰文字聊天扩展

基于上游 haimen v0.12.1。新增 WebSocket 消息：

```json
{"type":"text","text":"你好"}
```

先完成原有 hello 握手；仅 Ready 状态接受文字，去掉首尾空白后要求 1–32768 UTF-8 字节。AsrLlmTtsStrategy 跳过 ASR，直接复用语音模式的 Agent 会话、流式 TTS 和 Opus 回放；返回现有 stt、tts/start、tts/sentence_start、音频二进制帧及 tts/stop。支持原有 abort。其他策略默认返回不支持文字聊天错误。

相关修改：
- crates/haimen-xiaozhi/src/types.rs：文字指令。
- crates/haimen-xiaozhi/src/strategy.rs：文字回复接口。
- crates/haimen-xiaozhi/src/ws.rs：校验和流式回放；预缓冲期间可打断，先排空事件再处理生成完成，空音频也发结束事件。
- src/xiaozhi_asr_llm_tts.rs：语音和文字共用 Agent/TTS 实现。
- crates/haimen-xiaozhi/tests/text_input.rs：真实 WebSocket 回归测试（模拟生成策略）。

构建时将 Rust 和 CMake 放进 PATH，本机可使用 Android SDK 已安装的 CMake：

```sh
export PATH="/Users/ray/.cargo/bin:/Users/ray/Library/Android/sdk/cmake/3.22.1/bin:$PATH"
cargo fmt --check
cargo clippy --release --locked -- -D warnings
cargo test --release --locked -- --test-threads=1
cargo test --release --locked -p haimen-xiaozhi
cargo build --release --locked
```

这是本地扩展，直接升级到上游发布版会覆盖文字聊天支持。部署前备份 ~/.cargo/bin/haimen，原 LaunchAgent 路径保持不变。

## 本机部署记录（2026-09-22）

已将构建程序安装到 `/Users/ray/.cargo/bin/haimen`，并重启 `gui/501/ai.openclaw.haimen`（实际用户域以 `id -u` 为准）。原版程序和 LaunchAgent 备份：
`/Users/ray/.haimen/backups/before-text-chat-20260922-145223/`。

需要回退时，先复制备份到临时文件再原子替换（避免直接覆盖正在运行的可执行文件）：

```sh
cp /Users/ray/.haimen/backups/before-text-chat-20260922-145223/haimen /Users/ray/.cargo/bin/haimen.rollback
mv /Users/ray/.cargo/bin/haimen.rollback /Users/ray/.cargo/bin/haimen
launchctl kickstart -k gui/$(id -u)/ai.openclaw.haimen
```

回退后语音功能仍按上游行为运行，文字功能不再受支持。
