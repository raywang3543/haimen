pub mod process;
pub mod provider;

use std::pin::Pin;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::Stream;

pub use provider::{AgentEventStream, AgentLogEvent, AgentOutput, AgentProvider, TextStream};

/// 以 base64 保存的图片附件。编码内容不应写入日志。
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ImageData {
    pub mime_type: String,
    pub data_base64: String,
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageData")
            .field("mime_type", &self.mime_type)
            .field("encoded_bytes", &self.data_base64.len())
            .finish()
    }
}

/// 统一消息模型
#[derive(Debug, Clone)]
pub struct Message {
    /// 平台内唯一消息 ID
    pub id: String,
    /// 会话标识（对应 chat_id / thread_id / conversation_id）
    pub conversation_id: String,
    /// 发送者 ID
    pub sender_id: String,
    /// 消息文本内容（纯文本，由各平台自行从原始格式转换）
    pub content: String,
    /// 图片附件（base64 编码）
    pub images: Vec<ImageData>,
    /// 消息时间戳
    pub timestamp: DateTime<Utc>,
    /// 来源通道名称（用于日志和调试）
    pub channel: String,
}

/// 消息通道抽象
#[async_trait]
pub trait MessageChannel: Send + Sync {
    /// 通道名称
    fn name(&self) -> &str;
    /// 启动监听，返回消息流
    async fn listen(&self) -> Result<Pin<Box<dyn Stream<Item = Message> + Send>>, String>;
    /// 发送消息到指定会话
    async fn send(&self, conversation_id: &str, message: &str) -> Result<(), String>;
    /// 健康检查
    async fn health_check(&self) -> Result<(), String>;
}
