//! # WebSocket 消息协议模块
//!
//! 定义 P2P 通信的消息格式和辅助函数。
//! 所有节点使用相同的消息协议进行通信。
//!
//! ## 消息协议
//!
//! ```json
//! {
//!     "type": "clipboard | heartbeat | node_status",
//!     "sender_id": "device-uuid",
//!     "payload": { ... },
//!     "timestamp": 1234567890000
//! }
//! ```

use serde::{Deserialize, Serialize};

// ============================================================
// 消息协议定义
// ============================================================

/// 消息类型枚举
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Clipboard,
    Heartbeat,
    NodeStatus,
}

/// WebSocket 传输层统一消息格式。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMessage {
    #[serde(rename = "type")]
    pub msg_type: MessageType,
    pub sender_id: String,
    pub payload: serde_json::Value,
    pub timestamp: i64,
}

/// 剪贴板同步载荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPayload {
    /// 数据类型（MIME），如 "text/plain", "image/png"
    pub content_type: String,
    /// Base64 编码的加密密文
    pub data: String,
    /// 原始内容的 SHA-256 摘要（十六进制）
    pub content_hash: String,
}

/// 构造一条剪贴板同步消息。
pub fn build_clipboard_message(
    device_id: &str,
    encrypted_b64: &str,
    content_hash: &str,
    timestamp: i64,
) -> MeshMessage {
    let payload = ClipboardPayload {
        content_type: "text/plain".to_string(),
        data: encrypted_b64.to_string(),
        content_hash: content_hash.to_string(),
    };

    MeshMessage {
        msg_type: MessageType::Clipboard,
        sender_id: device_id.to_string(),
        payload: serde_json::to_value(&payload).unwrap(),
        timestamp,
    }
}

/// 构造心跳 / 自我介绍消息（携带 device_name）。
pub fn build_heartbeat_message(device_id: &str, device_name: &str) -> MeshMessage {
    MeshMessage {
        msg_type: MessageType::Heartbeat,
        sender_id: device_id.to_string(),
        payload: serde_json::json!({ "device_name": device_name }),
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

/// 安全截取设备 ID 前 8 个字符（按 char 边界）。
pub fn short_id(id: &str) -> &str {
    let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
    &id[..end]
}
