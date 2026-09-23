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

## 按轮关闭 TTS（2026-09-23）

小冰静音时使用以下请求，取消静音时传 `true`。省略字段默认为 `true`，兼容原客户端。

```json
{"type":"text","text":"你好","tts_enabled":false}
{"type":"listen","state":"start","tts_enabled":false}
```

语音仍执行 ASR；`listen.stop` 不修改本轮输出设置，服务端 VAD 自动结束录音时也保留该设置。
该字段属于本轮请求，保存在 WebSocket 连接内，不修改全局 TTS 设置或影响其他连接。
播放中收到新的 `listen.start` 时，按新请求的值启动下一轮。

`AsrLlmTtsStrategy` 纯文字分支消费 Agent 文本和工具事件，保留会话与日志，跳过 TTS
提供商创建、进度语音、Opus 连续静音编码和告别语音。按句立即发送原有
`tts/sentence_start` 文本事件（此处仅复用消息名，没有执行 TTS），完成后发送 `tts/stop`；
无音频二进制帧。空回复也结束，Agent 错误返回 `error`，`abort` 可取消生成。
其他未实现纯文字功能的策略返回错误，不会偷偷合成。文字回复继续触发小冰的 Jev 表情分析。

切换以每轮请求为界：回答途中静音仅立即停止本地播放，已发起的服务端任务继续，下一轮跳过 TTS；
取消静音不会为当前文字回复补生成语音。须构建并部署新版 haimen 才会生效；旧服务端忽略此字段。
本机已部署，见下方 2026-09-23 记录；其他机器仍需单独更新。

## 回答期间提交新消息

客户端先停止本地播放并发送 `{"type":"abort","request_id":"本次打断编号"}`。
服务端停止旧轮次，先发送原有 `tts/stop`，再发送
`{"type":"aborted","request_id":"本次打断编号"}`。
客户端等到编号匹配的确认后，才发下一条 `text` 或 `listen/start`；等待期间丢弃旧轮次数据。
即使旧回复已经正常结束，服务端也会返回对应确认，避免将自然结束误认为打断完成。
不带 `request_id` 的旧客户端仍仅收到原有 `tts/stop`，保持兼容。

该扩展已于 2026-09-23 部署并重启本机服务。更新前备份：
`/Users/ray/.haimen/backups/before-reply-interrupt-20260923-193318/`。
部署程序 SHA-256：`2d99bfe183d39f34b6864462cb4ad2aeb7e479d109c1eb3369dd2d95499c21e6`。
真实请求验证：收到旧轮次音频后打断，编号确认匹配；下一条静音问题返回
“连续发送测试通过”及正常结束通知，期间没有旧音频混入。健康检查正常。

## 本机部署记录（2026-09-23）

已执行 `cargo build --release --locked`，原子替换 `/Users/ray/.cargo/bin/haimen`，
并重启 `gui/501/ai.openclaw.haimen`。更新前的程序及 LaunchAgent 备份位于：
`/Users/ray/.haimen/backups/before-mute-tts-20260923-191916/`。

部署程序 SHA-256：`4ed5a55be316871bf4b548743a3b8a414313d8ce60bad2df19109f2e1061278c`。
`/health` 返回 `ok`。真实 WebSocket 验证携带 `tts_enabled:false`，收到“静音测试通过。”
及 `tts/stop`，音频二进制帧数为 0。客户端需要运行包含该请求字段的新版 APP。

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
