//! 本地 SenseVoice Small ASR，使用 sherpa-onnx 对完整的 PCM16 语音做离线识别。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use futures_util::{Stream, StreamExt, stream};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};
use univoice::asr::{AsrError, AsrProvider, AsrStreamChunk, AudioStream};

const SAMPLE_RATE: i32 = 16_000;
const MAX_AUDIO_BYTES: usize = 120 * SAMPLE_RATE as usize * 2;
const DEFAULT_MODEL_DIR: &str = "models/sensevoice-small-v1";

static RECOGNIZERS: OnceLock<Mutex<HashMap<PathBuf, Arc<OfflineRecognizer>>>> = OnceLock::new();

pub struct SenseVoiceAsr {
    model_dir: PathBuf,
}

impl SenseVoiceAsr {
    pub fn new(model_dir: Option<&str>) -> Result<Self, String> {
        let path = model_dir.filter(|s| !s.trim().is_empty()).map_or_else(
            || {
                let local = PathBuf::from(DEFAULT_MODEL_DIR);
                if local.is_dir() {
                    local
                } else {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_MODEL_DIR)
                }
            },
            PathBuf::from,
        );
        let path = path
            .canonicalize()
            .map_err(|e| format!("SenseVoice 模型目录 {} 不可用: {e}", path.display()))?;

        for filename in ["model.int8.onnx", "tokens.txt"] {
            let file = path.join(filename);
            if !file.is_file() {
                return Err(format!("SenseVoice 缺少模型文件: {}", file.display()));
            }
        }
        Ok(Self { model_dir: path })
    }

    pub fn verify_model(&self) -> Result<(), String> {
        self.recognizer().map(|_| ())
    }

    fn recognizer(&self) -> Result<Arc<OfflineRecognizer>, String> {
        let cache = RECOGNIZERS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut cache = cache
            .lock()
            .map_err(|_| "SenseVoice 模型缓存锁已损坏".to_string())?;
        if let Some(recognizer) = cache.get(&self.model_dir) {
            return Ok(recognizer.clone());
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice.model = Some(
            self.model_dir
                .join("model.int8.onnx")
                .to_string_lossy()
                .into_owned(),
        );
        config.model_config.sense_voice.language = Some("auto".into());
        config.model_config.sense_voice.use_itn = true;
        config.model_config.tokens = Some(
            self.model_dir
                .join("tokens.txt")
                .to_string_lossy()
                .into_owned(),
        );
        config.model_config.num_threads = 2;
        let recognizer =
            Arc::new(OfflineRecognizer::create(&config).ok_or_else(|| {
                format!("无法加载 SenseVoice 模型: {}", self.model_dir.display())
            })?);
        cache.insert(self.model_dir.clone(), recognizer.clone());
        Ok(recognizer)
    }

    fn recognize_pcm(&self, pcm: &[u8]) -> Result<String, String> {
        if pcm.len() % 2 != 0 {
            return Err("SenseVoice 需要完整的 16 位 PCM 采样".into());
        }
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let samples: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0)
            .collect();
        let recognizer = self.recognizer()?;
        let input = recognizer.create_stream();
        input.accept_waveform(SAMPLE_RATE, &samples);
        recognizer.decode(&input);
        input
            .get_result()
            .map(|result| result.text.trim().to_string())
            .ok_or_else(|| "SenseVoice 未返回识别结果".into())
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }
}

#[async_trait]
impl AsrProvider for SenseVoiceAsr {
    fn name(&self) -> &'static str {
        "sensevoice"
    }

    async fn listen_stream(
        &self,
        mut audio: AudioStream,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<AsrStreamChunk, AsrError>> + Send>>, AsrError>
    {
        let model_dir = self.model_dir.clone();
        Ok(Box::pin(stream::once(async move {
            let mut pcm = Vec::new();
            while let Some(chunk) = audio.next().await {
                if pcm.len().saturating_add(chunk.len()) > MAX_AUDIO_BYTES {
                    return Err(AsrError::InvalidParameter(
                        "SenseVoice 音频超过 120 秒".into(),
                    ));
                }
                pcm.extend_from_slice(&chunk);
            }

            let text = tokio::task::spawn_blocking(move || {
                let asr = SenseVoiceAsr { model_dir };
                asr.recognize_pcm(&pcm)
            })
            .await
            .map_err(|e| AsrError::Other(format!("SenseVoice 推理任务失败: {e}")))?
            .map_err(AsrError::Other)?;

            Ok(AsrStreamChunk {
                text,
                is_final: true,
                confidence: None,
                segment: None,
            })
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_model_files() {
        let dir = tempfile::tempdir().unwrap();
        let error = SenseVoiceAsr::new(Some(dir.path().to_str().unwrap()))
            .err()
            .expect("应拒绝缺少模型文件的目录");
        assert!(error.contains("model.int8.onnx"));
    }

    #[test]
    fn rejects_incomplete_pcm_before_loading_model() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("model.int8.onnx"), []).unwrap();
        std::fs::write(dir.path().join("tokens.txt"), []).unwrap();
        let model = SenseVoiceAsr::new(Some(dir.path().to_str().unwrap())).unwrap();
        let error = model.recognize_pcm(&[0]).unwrap_err();
        assert!(error.contains("16 位 PCM"));
    }

    #[test]
    #[ignore = "requires a local SenseVoice model and SENSEVOICE_TEST_PCM"]
    fn recognizes_local_speech() {
        let path = std::env::var("SENSEVOICE_TEST_PCM").expect("请设置 SENSEVOICE_TEST_PCM");
        let pcm = std::fs::read(path).unwrap();
        let model = SenseVoiceAsr::new(None).unwrap();
        let text = model.recognize_pcm(&pcm).unwrap();
        println!("SenseVoice: {text}");
        assert!(text.contains("你好"), "未识别出测试语音中的问候语");
    }
}
