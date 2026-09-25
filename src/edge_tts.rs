//! 本地 edge-tts CLI 接入：MP3 转为小智设备使用的 24 kHz 单声道 PCM16。

use std::io::{Cursor, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use univoice::tts::error::TtsError;
use univoice::tts::traits::TtsProvider;
use univoice::tts::types::{TextStream, TtsAudioStream, TtsRequest, TtsResponse, TtsStreamChunk};

const SAMPLE_RATE: u32 = 24_000;
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct EdgeTts {
    binary: PathBuf,
    voice: String,
    rate: String,
    volume: String,
    pitch: String,
    proxy: Option<String>,
}

impl EdgeTts {
    pub fn new(
        voice: String,
        rate: String,
        volume: String,
        pitch: String,
        proxy: Option<String>,
    ) -> Result<Self, String> {
        let binary = match which::which("edge-tts") {
            Ok(path) => path,
            Err(error) => std::env::var_os("HOME")
                .and_then(|home| which::which(PathBuf::from(home).join(".local/bin/edge-tts")).ok())
                .ok_or_else(|| format!("找不到全局 edge-tts 命令: {error}"))?,
        };
        Ok(Self {
            binary,
            voice,
            rate,
            volume,
            pitch,
            proxy,
        })
    }

    async fn synthesize_text(&self, text: &str, voice: &str) -> Result<Vec<u8>, TtsError> {
        if text.trim().is_empty() {
            return Err(TtsError::InvalidParameter("TTS 文本为空".into()));
        }
        let temp = tempfile::tempdir()
            .map_err(|e| TtsError::Other(format!("创建 TTS 临时目录失败: {e}")))?;
        let input = temp.path().join("input.txt");
        let output = temp.path().join("audio.mp3");
        tokio::fs::write(&input, text)
            .await
            .map_err(|e| TtsError::Other(format!("写入 TTS 文本失败: {e}")))?;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .arg("--file")
            .arg(&input)
            .arg("--voice")
            .arg(voice)
            .arg("--rate")
            .arg(&self.rate)
            .arg("--volume")
            .arg(&self.volume)
            .arg("--pitch")
            .arg(&self.pitch)
            .arg("--write-media")
            .arg(&output)
            .kill_on_drop(true);
        if let Some(proxy) = &self.proxy {
            command.arg("--proxy").arg(proxy);
        }
        let result = tokio::time::timeout(Duration::from_secs(75), command.output())
            .await
            .map_err(|_| TtsError::Timeout(75_000))?
            .map_err(|e| TtsError::Other(format!("执行 edge-tts 失败: {e}")))?;
        if !result.status.success() {
            let error = String::from_utf8_lossy(&result.stderr);
            return Err(TtsError::Other(format!(
                "edge-tts 合成失败: {}",
                error.trim()
            )));
        }
        let mp3 = tokio::fs::read(&output)
            .await
            .map_err(|e| TtsError::Other(format!("读取 edge-tts 音频失败: {e}")))?;
        if mp3.len() > MAX_AUDIO_BYTES {
            return Err(TtsError::Other("edge-tts 音频过大".into()));
        }
        decode_mp3_to_pcm(&mp3)
    }
}

#[async_trait]
impl TtsProvider for EdgeTts {
    fn name(&self) -> &'static str {
        "edge_tts"
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        let voice = request
            .options
            .as_ref()
            .and_then(|option| option.voice.as_ref())
            .map_or(self.voice.as_str(), |voice| voice.as_str());
        let audio = self.synthesize_text(&request.text, voice).await?;
        Ok(TtsResponse {
            audio,
            format: "pcm".into(),
            duration: None,
        })
    }

    async fn speak_stream(&self, input: TextStream) -> Result<TtsAudioStream, TtsError> {
        let provider = self.clone();
        Ok(Box::pin(stream::unfold(
            (input, provider),
            |(mut input, provider)| async move {
                loop {
                    let text = input.next().await?;
                    if text.trim().is_empty() {
                        continue;
                    }
                    let result = provider
                        .synthesize_text(&text, &provider.voice)
                        .await
                        .map(|audio_chunk| TtsStreamChunk { audio_chunk });
                    return Some((result, (input, provider)));
                }
            },
        )))
    }
}

fn decode_mp3_to_pcm(mp3: &[u8]) -> Result<Vec<u8>, TtsError> {
    if mp3.is_empty() {
        return Err(TtsError::NoAudio);
    }
    let source = MediaSourceStream::new(Box::new(Cursor::new(mp3.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| TtsError::Other(format!("解析 Edge TTS MP3 失败: {e}")))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or(TtsError::NoAudio)?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| TtsError::Other(format!("创建 MP3 解码器失败: {e}")))?;
    let mut samples = Vec::<f32>::new();
    let mut source_rate = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(DecodeError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(TtsError::Other(format!("读取 MP3 音频帧失败: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(TtsError::Other(format!("解码 MP3 音频帧失败: {e}"))),
        };
        let spec = *decoded.spec();
        if source_rate.is_some_and(|rate| rate != spec.rate) {
            return Err(TtsError::Other("MP3 音频采样率中途变化".into()));
        }
        source_rate = Some(spec.rate);
        let channels = spec.channels.count();
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks_exact(channels) {
            samples.push(frame.iter().copied().sum::<f32>() / channels as f32);
        }
    }
    if samples.is_empty() {
        return Err(TtsError::NoAudio);
    }
    let source_rate = source_rate.ok_or(TtsError::NoAudio)?;
    let output_len = (samples.len() as u64 * SAMPLE_RATE as u64 / source_rate as u64) as usize;
    let mut pcm = Vec::with_capacity(output_len * 2);
    for i in 0..output_len {
        let position = i as f64 * source_rate as f64 / SAMPLE_RATE as f64;
        let left = position as usize;
        let fraction = (position - left as f64) as f32;
        let a = samples[left];
        let b = samples.get(left + 1).copied().unwrap_or(a);
        let value = (a + (b - a) * fraction).clamp(-1.0, 1.0);
        pcm.extend_from_slice(&((value * i16::MAX as f32) as i16).to_le_bytes());
    }
    Ok(pcm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_resamples_mp3_to_device_pcm() {
        let mp3 = include_bytes!("fixtures/edge_tts_48k.mp3");
        let pcm = decode_mp3_to_pcm(mp3).unwrap();
        assert!(pcm.len() > 8_000 && pcm.len() < 30_000);
        assert_eq!(pcm.len() % 2, 0);
        assert!(pcm.chunks_exact(2).any(|sample| sample != [0, 0]));
    }

    #[test]
    fn rejects_invalid_audio() {
        assert!(decode_mp3_to_pcm(b"not an mp3").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streams_text_segments_through_cli() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("edge-tts");
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/src/fixtures/edge_tts_48k.mp3");
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --file) input=\"$2\"; shift 2;;\n    --write-media) output=\"$2\"; shift 2;;\n    *) shift;;\n  esac\ndone\n[ -s \"$input\" ] || exit 2\ncp \"{fixture}\" \"$output\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tts = EdgeTts {
            binary,
            voice: "zh-CN-XiaoxiaoNeural".into(),
            rate: "+0%".into(),
            volume: "+0%".into(),
            pitch: "+0Hz".into(),
            proxy: None,
        };
        let input = stream::iter(["你好".into(), "世界".into()]);
        let chunks = tts
            .speak_stream(Box::pin(input))
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .into_iter()
                .all(|chunk| !chunk.unwrap().audio_chunk.is_empty())
        );
    }

    #[tokio::test]
    #[ignore = "需要全局 edge-tts 和网络"]
    async fn synthesizes_with_global_edge_tts() {
        let tts = EdgeTts::new(
            "zh-CN-XiaoxiaoNeural".into(),
            "+0%".into(),
            "+0%".into(),
            "+0Hz".into(),
            None,
        )
        .unwrap();
        let response = tts
            .synthesize(TtsRequest {
                text: "你好".into(),
                options: None,
            })
            .await
            .unwrap();
        assert_eq!(response.format, "pcm");
        assert!(!response.audio.is_empty());
    }
}
