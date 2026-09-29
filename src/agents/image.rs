//! OpenAI 兼容聊天接口的图片内容块。

use base64::Engine;
use haimen_core::ImageData;
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub(super) enum ImageUrlShape {
    /// OpenAI Chat Completions: `image_url: { "url": "data:..." }`。
    Object,
    /// Ollama 兼容接口的文档示例使用 `image_url: "data:..."`。
    String,
}

pub(super) fn user_content(
    text: &str,
    images: &[ImageData],
    shape: ImageUrlShape,
) -> Result<Value, String> {
    if images.is_empty() {
        return Ok(Value::String(text.to_string()));
    }
    let mut parts = vec![json!({ "type": "text", "text": text })];
    for image in images {
        if !matches!(
            image.mime_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return Err(format!("不支持图片格式: {}", image.mime_type));
        }
        if image.data_base64.len() > 14 * 1024 * 1024 {
            return Err("图片超过 10 MiB 限制".to_string());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data_base64)
            .map_err(|e| format!("图片 base64 解码失败: {e}"))?;
        if bytes.len() > 10 * 1024 * 1024 {
            return Err("图片超过 10 MiB 限制".to_string());
        }
        let url = format!("data:{};base64,{}", image.mime_type, image.data_base64);
        let part = match shape {
            ImageUrlShape::Object => json!({ "type": "image_url", "image_url": { "url": url } }),
            ImageUrlShape::String => json!({ "type": "image_url", "image_url": url }),
        };
        parts.push(part);
    }
    Ok(Value::Array(parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_data_url_without_changing_text_only_messages() {
        assert_eq!(
            user_content("hello", &[], ImageUrlShape::Object).unwrap(),
            "hello"
        );
        let image = ImageData {
            mime_type: "image/png".into(),
            data_base64: "aW1hZ2U=".into(),
        };
        let openai =
            user_content("看图", std::slice::from_ref(&image), ImageUrlShape::Object).unwrap();
        assert_eq!(openai[0], json!({ "type": "text", "text": "看图" }));
        assert_eq!(
            openai[1],
            json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,aW1hZ2U=" } })
        );
        let ollama = user_content("看图", &[image], ImageUrlShape::String).unwrap();
        assert_eq!(ollama[1]["image_url"], "data:image/png;base64,aW1hZ2U=");
    }

    #[test]
    fn rejects_malformed_image() {
        let image = ImageData {
            mime_type: "image/png".into(),
            data_base64: "not base64!".into(),
        };
        assert!(user_content("", &[image], ImageUrlShape::Object).is_err());
    }
}
