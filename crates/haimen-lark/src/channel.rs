use std::pin::Pin;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::Stream;
use futures_util::StreamExt;

use haimen_core::Message;
use haimen_core::MessageChannel;

use crate::bridge::LarkCliBridge;
use crate::types::BridgeHealth;
use crate::types::FeishuEvent;

pub struct LarkChannel {
    bridge: LarkCliBridge,
}

impl LarkChannel {
    pub fn new(lark_cli_path: impl Into<String>) -> Self {
        Self {
            bridge: LarkCliBridge::new(lark_cli_path),
        }
    }

    /// 结构化健康探测（供 Web 控制台区分 CLI 未安装 / 未认证）
    pub async fn probe(&self) -> BridgeHealth {
        self.bridge.health_check().await
    }
}

#[async_trait]
impl MessageChannel for LarkChannel {
    fn name(&self) -> &str {
        "lark"
    }

    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = Message> + Send>>, String> {
        let raw_stream = self
            .bridge
            .stream(&[
                "event",
                "consume",
                "im.message.receive_v1",
                "--as",
                "bot",
                "--quiet",
            ])
            .await?;

        let cli_path = self.bridge.path().to_string();

        let message_stream = raw_stream.filter_map(move |line_result| {
            let cli_path = cli_path.clone();
            async move {
            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "读取 lark-cli 事件流错误");
                    return None;
                }
            };

            if line.trim().is_empty() {
                return None;
            }

            let event: FeishuEvent = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => return None,
            };

            let bridge = LarkCliBridge::new(cli_path);
            let parsed = match event.message_type.as_str() {
                "text" => Ok((extract_text_content(&event.content), Vec::new())),
                "image" => load_image(&bridge, &event)
                    .await
                    .map(|image| ("请描述这张图片。".to_string(), vec![image])),
                "post" => load_post(&bridge, &event).await,
                _ => return None,
            };
            let (content, images) = match parsed {
                Ok(parsed) => parsed,
                Err(e) => {
                    tracing::warn!(message_id = %event.message_id, error = %e, "飞书图片处理失败");
                    let _ = bridge.exec(&[
                        "im", "+messages-send", "--as", "bot", "--chat-id", &event.chat_id,
                        "--text", "❌ 无法读取图片，请检查飞书应用的 im:resource 权限和图片格式。",
                    ]).await;
                    return None;
                }
            };
            if content.is_empty() && images.is_empty() {
                return None;
            }
            let message = Message {
                conversation_kind: match event.chat_type.as_str() {
                    "p2p" => haimen_core::ConversationKind::Private,
                    "group" => haimen_core::ConversationKind::Group,
                    _ => haimen_core::ConversationKind::Unknown,
                },
                id: event.message_id,
                conversation_id: event.chat_id,
                sender_id: event.sender_id,
                content,
                images,
                timestamp: Utc::now(),
                channel: "lark".to_string(),
            };

            Some(message)
            }
        });

        Ok(Box::pin(message_stream))
    }

    async fn send(&self, conversation_id: &str, message: &str) -> Result<(), String> {
        self.bridge
            .exec(&[
                "im",
                "+messages-send",
                "--as",
                "bot",
                "--chat-id",
                conversation_id,
                "--text",
                message,
            ])
            .await?;
        Ok(())
    }

    async fn health_check(&self) -> Result<(), String> {
        let health = self.bridge.health_check().await;
        if !health.lark_cli_found {
            return Err("lark-cli 未安装".to_string());
        }
        if !health.authenticated {
            return Err("飞书未认证".to_string());
        }
        Ok(())
    }
}

async fn load_image(
    bridge: &LarkCliBridge,
    event: &FeishuEvent,
) -> Result<haimen_core::ImageData, String> {
    if !event.message_id.starts_with("om_")
        || !event
            .message_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("飞书消息 ID 无效".to_string());
    }
    // event consume 会将图片内容预渲染为文本；需要查询原始消息获取 image_key。
    let key = if let Some(key) = extract_image_key(&event.content) {
        key
    } else {
        let path = format!("/open-apis/im/v1/messages/{}", event.message_id);
        let response = bridge.exec(&["api", "GET", &path, "--as", "bot"]).await?;
        extract_image_key(&response.to_string()).ok_or("飞书消息中未找到 image_key")?
    };
    bridge.download_image(&event.message_id, &key).await
}

async fn load_post(
    bridge: &LarkCliBridge,
    event: &FeishuEvent,
) -> Result<(String, Vec<haimen_core::ImageData>), String> {
    let (mut text, mut keys) = parse_post_content(&event.content);
    if keys.is_empty() && event.message_id.starts_with("om_") {
        let path = format!("/open-apis/im/v1/messages/{}", event.message_id);
        if let Ok(response) = bridge.exec(&["api", "GET", &path, "--as", "bot"]).await {
            if let Some(raw) = response
                .pointer("/data/items/0/body/content")
                .and_then(|value| value.as_str())
            {
                let (raw_text, raw_keys) = parse_post_content(raw);
                if !raw_text.is_empty() {
                    text = raw_text;
                }
                keys = raw_keys;
            }
        }
    }

    let mut images = Vec::with_capacity(keys.len());
    for key in keys {
        images.push(bridge.download_image(&event.message_id, &key).await?);
    }
    if text.is_empty() && !images.is_empty() {
        text = "请描述这张图片。".to_string();
    }
    Ok((text, images))
}

fn parse_post_content(content: &str) -> (String, Vec<String>) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(rows) = value.get("content").and_then(|value| value.as_array()) {
            let mut text = Vec::new();
            let mut keys = Vec::new();
            if let Some(title) = value.get("title").and_then(|value| value.as_str()) {
                if !title.trim().is_empty() {
                    text.push(title.trim().to_string());
                }
            }
            for row in rows {
                let mut line = String::new();
                if let Some(items) = row.as_array() {
                    for item in items {
                        if let Some(key) = item.get("image_key").and_then(|value| value.as_str()) {
                            if !keys.iter().any(|existing| existing == key) {
                                keys.push(key.to_string());
                            }
                        } else if let Some(part) = item.get("text").and_then(|value| value.as_str())
                        {
                            line.push_str(part);
                        }
                    }
                }
                if !line.trim().is_empty() {
                    text.push(line.trim().to_string());
                }
            }
            return (text.join("\n"), keys);
        }
    }

    let mut text = String::new();
    let mut keys = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[Image:") {
        text.push_str(&rest[..start]);
        let marker = &rest[start + "[Image:".len()..];
        let Some(end) = marker.find(']') else {
            text.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let key = marker[..end].trim();
        if !key.is_empty() && !keys.iter().any(|existing| existing == key) {
            keys.push(key.to_string());
        }
        rest = &marker[end + 1..];
    }
    text.push_str(rest);
    (text.trim().to_string(), keys)
}

fn extract_image_key(content: &str) -> Option<String> {
    fn find(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::Object(map) => map
                .get("image_key")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| map.values().find_map(find)),
            serde_json::Value::Array(items) => items.iter().find_map(find),
            serde_json::Value::String(text) => serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|nested| find(&nested)),
            _ => None,
        }
    }
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| find(&value))
}

/// 从飞书文本消息内容中提取文本
pub fn extract_text_content(content: &str) -> String {
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
            return text.to_string();
        }
    }
    content
        .trim_start_matches('"')
        .trim_end_matches('"')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_content_json() {
        let content = r#"{"text":"你好世界"}"#;
        assert_eq!(extract_text_content(content), "你好世界");
    }

    #[test]
    fn test_extract_text_content_plain() {
        assert_eq!(extract_text_content("\"hello\""), "hello");
        assert_eq!(extract_text_content("plain text"), "plain text");
    }

    #[test]
    fn test_extract_text_content_empty() {
        assert_eq!(extract_text_content(""), "");
    }

    #[test]
    fn test_extract_image_key_from_raw_message() {
        let response = r#"{"data":{"items":[{"body":{"content":"{\"image_key\":\"img_abc\"}"}}]}}"#;
        assert_eq!(extract_image_key(response).as_deref(), Some("img_abc"));
        assert_eq!(extract_image_key("[image]"), None);
    }

    #[test]
    fn test_parse_post_image_and_caption() {
        let rendered = "[Image: img_v3_abc-123]\n你看到这张图片了吗？";
        let (text, keys) = parse_post_content(rendered);
        assert_eq!(text, "你看到这张图片了吗？");
        assert_eq!(keys, vec!["img_v3_abc-123"]);

        let raw = serde_json::json!({
            "title": "",
            "content": [
                [{"tag": "img", "image_key": "img_v3_abc-123"}],
                [{"tag": "text", "text": "你看到这张图片了吗？"}]
            ],
            "content_v2": [[{"tag": "img", "image_key": "img_v3_abc-123"}]]
        });
        let (text, keys) = parse_post_content(&raw.to_string());
        assert_eq!(text, "你看到这张图片了吗？");
        assert_eq!(keys, vec!["img_v3_abc-123"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_load_image_fetches_raw_key_and_encodes_download() {
        use base64::Engine;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("source.png");
        let bytes = b"\x89PNG\r\n\x1a\nimage-data";
        std::fs::write(&image, bytes).unwrap();
        let cli = dir.path().join("fake-lark-cli");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = api ]; then\n  echo '{{\"ok\":true,\"data\":{{\"items\":[{{\"body\":{{\"content\":\"{{\\\"image_key\\\":\\\"img_abc-123\\\"}}\"}}}}]}}}}'\nelse\n  cp '{}' image.png\nfi\n",
                image.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let event = FeishuEvent {
            message_id: "om_example".to_string(),
            chat_id: "oc_example".to_string(),
            chat_type: "p2p".to_string(),
            sender_id: "ou_example".to_string(),
            message_type: "image".to_string(),
            content: "[image]".to_string(),
            create_time: "0".to_string(),
            event_id: None,
        };
        let result = load_image(&LarkCliBridge::new(cli.to_string_lossy()), &event)
            .await
            .unwrap();
        assert_eq!(result.mime_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(result.data_base64)
                .unwrap(),
            bytes
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_load_post_fetches_image_and_caption() {
        use base64::Engine;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("source.png");
        let bytes = b"\x89PNG\r\n\x1a\npost-image";
        std::fs::write(&image, bytes).unwrap();
        let response = dir.path().join("response.json");
        let body = serde_json::json!({
            "content": [
                [{"tag": "img", "image_key": "img_v3_abc-123"}],
                [{"tag": "text", "text": "你看到这张图片了吗？"}]
            ]
        });
        std::fs::write(
            &response,
            serde_json::json!({"data": {"items": [{"body": {"content": body.to_string()}}]}})
                .to_string(),
        )
        .unwrap();
        let cli = dir.path().join("fake-lark-cli");
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nif [ \"$1\" = api ]; then cat '{}'; else cp '{}' image.png; fi\n",
                response.display(),
                image.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let event = FeishuEvent {
            message_id: "om_post".to_string(),
            chat_id: "oc_example".to_string(),
            chat_type: "p2p".to_string(),
            sender_id: "ou_example".to_string(),
            message_type: "post".to_string(),
            content: "你看到这张图片了吗？".to_string(),
            create_time: "0".to_string(),
            event_id: None,
        };

        let (text, images) = load_post(&LarkCliBridge::new(cli.to_string_lossy()), &event)
            .await
            .unwrap();
        assert_eq!(text, "你看到这张图片了吗？");
        assert_eq!(images.len(), 1);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&images[0].data_base64)
                .unwrap(),
            bytes
        );
    }
}
