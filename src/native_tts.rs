//! macOS AVSpeechSynthesizer provider. A small embedded Swift helper yields
//! 24 kHz mono PCM16 on stdout while the system synthesizes each text segment.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use univoice::tts::error::TtsError;
use univoice::tts::traits::TtsProvider;
use univoice::tts::types::{TextStream, TtsAudioStream, TtsRequest, TtsResponse, TtsStreamChunk};

pub const DEFAULT_VOICE: &str = "com.apple.voice.premium.zh-CN.Yue";
pub const DEFAULT_RATE: f32 = 0.5;
pub const DEFAULT_PITCH: f32 = 1.0;
pub const DEFAULT_VOLUME: f32 = 1.0;
pub const DEFAULT_DELAY: f64 = 0.0;

static HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/haimen-native-tts"));

#[derive(Debug, Deserialize)]
pub struct NativeVoice {
    pub id: String,
    pub name: String,
    pub language: String,
}

#[derive(Clone)]
pub struct NativeTts {
    voice: String,
    rate: f32,
    pitch: f32,
    volume: f32,
    pre_delay: f64,
    post_delay: f64,
    helper: Arc<Helper>,
}

struct Helper {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Helper {
    fn new() -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("创建原生 TTS 临时目录失败: {e}"))?;
        let path = dir.path().join("haimen-native-tts");
        std::fs::write(&path, HELPER).map_err(|e| format!("写入原生 TTS 程序失败: {e}"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("设置原生 TTS 程序权限失败: {e}"))?;
        Ok(Self { _dir: dir, path })
    }
}

impl NativeTts {
    pub fn new(
        voice: String,
        rate: f32,
        pitch: f32,
        volume: f32,
        pre_delay: f64,
        post_delay: f64,
    ) -> Result<Self, String> {
        if !(0.0..=1.0).contains(&rate)
            || !(0.5..=2.0).contains(&pitch)
            || !(0.0..=1.0).contains(&volume)
            || !pre_delay.is_finite()
            || pre_delay < 0.0
            || !post_delay.is_finite()
            || post_delay < 0.0
        {
            return Err("原生 TTS 参数超出范围".into());
        }
        Ok(Self {
            voice,
            rate,
            pitch,
            volume,
            pre_delay,
            post_delay,
            helper: Arc::new(Helper::new()?),
        })
    }

    async fn start(&self, text: &str) -> Result<Running, TtsError> {
        let mut child = Command::new(&self.helper.path)
            .arg(&self.voice)
            .arg(self.rate.to_string())
            .arg(self.pitch.to_string())
            .arg(self.volume.to_string())
            .arg(self.pre_delay.to_string())
            .arg(self.post_delay.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| TtsError::Other(format!("启动原生 TTS 失败: {e}")))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| TtsError::Other("无法连接原生 TTS 的文本输入".to_string()))?;
        stdin
            .write_all(text.as_bytes())
            .await
            .map_err(|e| TtsError::Other(format!("向原生 TTS 发送文本失败: {e}")))?;
        drop(stdin);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TtsError::Other("无法连接原生 TTS 的音频输出".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TtsError::Other("无法连接原生 TTS 的错误输出".to_string()))?;
        Ok(Running {
            child,
            stdout,
            stderr,
        })
    }

    pub async fn available_voices() -> Result<Vec<NativeVoice>, String> {
        let helper = Helper::new()?;
        let output = Command::new(&helper.path)
            .arg("--voices")
            .output()
            .await
            .map_err(|e| format!("读取 macOS 音色失败: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        serde_json::from_slice(&output.stdout).map_err(|e| format!("解析 macOS 音色失败: {e}"))
    }
}

struct Running {
    child: Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
}

struct StreamState {
    input: TextStream,
    provider: NativeTts,
    running: Option<Running>,
    done: bool,
}

#[async_trait]
impl TtsProvider for NativeTts {
    fn name(&self) -> &'static str {
        "macos_native"
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        let input: TextStream = Box::pin(stream::iter([request.text]));
        let mut chunks = self.speak_stream(input).await?;
        let mut audio = Vec::new();
        while let Some(chunk) = chunks.next().await {
            audio.extend_from_slice(&chunk?.audio_chunk);
        }
        Ok(TtsResponse {
            audio,
            format: "pcm".into(),
            duration: None,
        })
    }

    async fn speak_stream(&self, input: TextStream) -> Result<TtsAudioStream, TtsError> {
        let state = StreamState {
            input,
            provider: self.clone(),
            running: None,
            done: false,
        };
        Ok(Box::pin(stream::unfold(state, |mut state| async move {
            if state.done {
                return None;
            }
            loop {
                if state.running.is_none() {
                    let text = loop {
                        let text = state.input.next().await?;
                        if !text.trim().is_empty() {
                            break text;
                        }
                    };
                    match state.provider.start(&text).await {
                        Ok(running) => state.running = Some(running),
                        Err(error) => {
                            state.done = true;
                            return Some((Err(error), state));
                        }
                    }
                }

                let running = state.running.as_mut().expect("running process missing");
                let mut audio_chunk = vec![0u8; 8192];
                match running.stdout.read(&mut audio_chunk).await {
                    Ok(n) if n > 0 => {
                        audio_chunk.truncate(n);
                        return Some((Ok(TtsStreamChunk { audio_chunk }), state));
                    }
                    Ok(_) => {
                        let mut running = state.running.take().expect("running process missing");
                        let mut diagnostics = Vec::new();
                        let stderr_result = running.stderr.read_to_end(&mut diagnostics).await;
                        let status_result = running.child.wait().await;
                        if let Err(error) = stderr_result {
                            state.done = true;
                            return Some((Err(io_error(error)), state));
                        }
                        match status_result {
                            Ok(status) if status.success() => continue,
                            Ok(status) => {
                                state.done = true;
                                let message = String::from_utf8_lossy(&diagnostics);
                                return Some((
                                    Err(TtsError::Other(format!(
                                        "macOS 原生 TTS 失败 ({status}): {}",
                                        message.trim()
                                    ))),
                                    state,
                                ));
                            }
                            Err(error) => {
                                state.done = true;
                                return Some((Err(io_error(error)), state));
                            }
                        }
                    }
                    Err(error) => {
                        state.done = true;
                        return Some((Err(io_error(error)), state));
                    }
                }
            }
        })))
    }
}

fn io_error(error: io::Error) -> TtsError {
    TtsError::Other(format!("读取 macOS 原生 TTS 音频失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_voice_streams_pcm() {
        let voices = NativeTts::available_voices().await.unwrap();
        let voice = voices
            .iter()
            .find(|voice| voice.language == "zh-CN")
            .or_else(|| voices.first())
            .expect("macOS 应提供至少一个音色");
        let mut config = crate::config::settings::TtsConfig {
            active_provider: "macos_native".into(),
            ..Default::default()
        };
        config
            .providers
            .entry("macos_native".into())
            .or_default()
            .insert("voice".into(), voice.id.clone());
        let provider = crate::tts_factory::create_tts_provider(&config).unwrap();
        assert_eq!(provider.name(), "macos_native");
        let input: TextStream = Box::pin(stream::iter(["你好，海门。".to_string()]));
        let mut output = provider.speak_stream(input).await.unwrap();
        let mut bytes = 0;
        while let Some(chunk) = output.next().await {
            bytes += chunk.unwrap().audio_chunk.len();
        }
        assert!(bytes > 2880, "原生语音应产生多于一帧的 PCM 数据");
        assert_eq!(bytes % 2, 0, "PCM16 数据应按采样对齐");
    }
}
