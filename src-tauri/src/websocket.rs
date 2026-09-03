//! # WebSocket 客户端模块
//!
//! 实现与 clip-mesh-core 服务端的双向 WebSocket 通信。
//!
//! ## 功能特性
//!
//! - **自动重连**：连接断开后按指数退避策略尝试重新连接。
//! - **心跳维持**：定时发送应用层心跳消息，保持连接活跃。
//! - **双向消息**：支持发送（本地剪贴板→服务端）和接收（服务端→本地剪贴板）。
//!
//! ## 消息协议（与 Go 端 `network.Message` 一致）
//!
//! ```json
//! {
//!     "type": "clipboard | heartbeat | node_status",
//!     "sender_id": "device-uuid",
//!     "payload": { ... },
//!     "timestamp": 1234567890000
//! }
//! ```

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::time;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message as WsMessage};

// ============================================================
// 消息协议定义
// ============================================================

/// 消息类型枚举（与 Go 端 `network.MessageType` 对齐）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Clipboard,
    Heartbeat,
    NodeStatus,
    Broadcast,
}

/// WebSocket 传输层统一消息格式。
///
/// 与 Go 端 `network.Message` 结构完全一致，确保序列化兼容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMessage {
    /// 消息类型
    #[serde(rename = "type")]
    pub msg_type: MessageType,

    /// 发送方设备 ID
    pub sender_id: String,

    /// 消息载荷（JSON 原始值，延迟解析）
    pub payload: serde_json::Value,

    /// 时间戳（Unix 毫秒）
    pub timestamp: i64,
}

/// 剪贴板同步载荷（与 Go 端 `network.ClipboardPayload` 一致）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPayload {
    /// 数据类型（MIME）
    pub content_type: String,
    /// Base64 编码的加密密文
    pub data: String,
    /// 原始内容的 SHA-256 摘要（十六进制）
    pub content_hash: String,
}

// ============================================================
// 连接状态
// ============================================================

/// WebSocket 连接状态
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    /// 未连接
    Disconnected,
    /// 正在连接
    Connecting,
    /// 已连接
    Connected,
    /// 重连中（附带重试次数）
    Reconnecting(u32),
}

// ============================================================
// WebSocket 客户端
// ============================================================

/// WebSocket 客户端配置
#[derive(Clone)]
pub struct WsClientConfig {
    /// 服务端地址，如 "ws://192.168.1.100:8080/api/v1/ws"
    pub server_url: String,
    /// 本设备 ID
    pub device_id: String,
    /// 本设备名称
    pub device_name: String,
    /// Auth Token（通过查询参数传递）
    pub auth_token: String,
}

/// WebSocket 客户端。
///
/// 管理到 clip-mesh-core 的 WebSocket 连接，提供：
/// - `outgoing_tx`: 发送端，供外部（剪贴板监听器）投递待发送消息
/// - `incoming_rx`: 接收端，供外部（剪贴板注入器）消费收到的消息
/// - `state_rx`: 连接状态广播，供 UI 层感知连接变化
pub struct MeshWsClient {
    /// 客户端配置
    config: WsClientConfig,
    /// 待发送消息通道（外部 → WebSocket）
    outgoing_tx: mpsc::Sender<MeshMessage>,
    outgoing_rx: mpsc::Receiver<MeshMessage>,
    /// 接收消息通道（WebSocket → 外部）
    incoming_tx: mpsc::Sender<MeshMessage>,
    /// 连接状态广播
    state_tx: broadcast::Sender<ConnectionState>,
}

impl MeshWsClient {
    /// 创建 WebSocket 客户端实例。
    ///
    /// 返回 `(client, incoming_rx, state_rx)` 三元组：
    /// - `client`: 需调用 `start()` 启动连接循环
    /// - `incoming_rx`: 消费端接收远端消息
    /// - `state_rx`: UI 端订阅连接状态变化
    pub fn new(config: WsClientConfig) -> (Self, mpsc::Receiver<MeshMessage>, broadcast::Receiver<ConnectionState>) {
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<MeshMessage>(256);
        let (incoming_tx, incoming_rx) = mpsc::channel::<MeshMessage>(256);
        let (state_tx, state_rx) = broadcast::channel::<ConnectionState>(16);

        let client = Self {
            config,
            outgoing_tx,
            outgoing_rx,
            incoming_tx,
            state_tx,
        };

        (client, incoming_rx, state_rx)
    }

    /// 获取发送端引用，供外部模块投递消息。
    pub fn sender(&self) -> mpsc::Sender<MeshMessage> {
        self.outgoing_tx.clone()
    }

    /// 启动 WebSocket 连接主循环（含自动重连）。
    ///
    /// 此方法为异步阻塞调用，应在独立 tokio 任务中运行。
    ///
    /// ## 重连策略
    ///
    /// 采用指数退避（Exponential Backoff）：
    /// - 首次重连：1 秒
    /// - 第二次：2 秒
    /// - 第三次：4 秒
    /// - ...最大间隔 60 秒
    pub async fn start(mut self) {
        let mut retry_count: u32 = 0;
        let base_delay = Duration::from_secs(1);
        let max_delay = Duration::from_secs(60);

        loop {
            // 广播连接状态
            let state = if retry_count == 0 {
                ConnectionState::Connecting
            } else {
                ConnectionState::Reconnecting(retry_count)
            };
            let _ = self.state_tx.send(state);

            // 尝试连接（查询参数需 URL 编码，防止空格/中文等特殊字符导致 URL 非法）
            let device_id_encoded = url_encode(&self.config.device_id);
            let device_name_encoded = url_encode(&self.config.device_name);
            let ws_url = format!(
                "{}?device_id={}&device_name={}",
                self.config.server_url, device_id_encoded, device_name_encoded
            );

            log::info!("[WS] Connecting to {}...", ws_url);

            match connect_async(&ws_url).await {
                Ok((ws_stream, _response)) => {
                    log::info!("[WS] Connected successfully");
                    let _ = self.state_tx.send(ConnectionState::Connected);
                    retry_count = 0; // 重置重试计数

                    // 进入消息循环
                    self.run_message_loop(ws_stream).await;

                    log::warn!("[WS] Connection lost");
                    let _ = self.state_tx.send(ConnectionState::Disconnected);
                }
                Err(e) => {
                    log::error!("[WS] Connection failed: {}", e);
                }
            }

            // 指数退避等待
            retry_count += 1;
            let delay = base_delay
                .checked_mul(2u32.saturating_pow(retry_count.min(6)))
                .unwrap_or(max_delay)
                .min(max_delay);

            log::info!("[WS] Retrying in {:?} (attempt {})", delay, retry_count);
            time::sleep(delay).await;
        }
    }

    /// 运行消息收发循环。
    ///
    /// 将 WebSocket 拆分为读/写半区，与心跳定时器、发送通道一起
    /// 在 select! 中统一调度。
    async fn run_message_loop(
        &mut self,
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) {
        let (mut write_half, mut read_half) = ws_stream.split();

        // 心跳定时器：每 30 秒发送一次应用层心跳
        let mut heartbeat_interval = time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                // ---- 接收远端消息 ----
                msg = read_half.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            self.handle_incoming_text(&text).await;
                        }
                        Some(Ok(WsMessage::Close(_))) => {
                            log::info!("[WS] Received close frame");
                            return;
                        }
                        Some(Err(e)) => {
                            log::error!("[WS] Read error: {}", e);
                            return;
                        }
                        None => {
                            log::warn!("[WS] Stream ended");
                            return;
                        }
                        _ => {
                            // 忽略 Ping/Pong/Binary 帧（由 tungstenite 内部处理）
                        }
                    }
                }

                // ---- 发送本地消息 ----
                Some(msg) = self.outgoing_rx.recv() => {
                    let json = match serde_json::to_string(&msg) {
                        Ok(j) => j,
                        Err(e) => {
                            log::error!("[WS] Serialize error: {}", e);
                            continue;
                        }
                    };
                    if let Err(e) = write_half.send(WsMessage::Text(json)).await {
                        log::error!("[WS] Send error: {}", e);
                        return;
                    }
                }

                // ---- 心跳定时器 ----
                _ = heartbeat_interval.tick() => {
                    let heartbeat = MeshMessage {
                        msg_type: MessageType::Heartbeat,
                        sender_id: self.config.device_id.clone(),
                        payload: serde_json::Value::Null,
                        timestamp: chrono::Utc::now().timestamp_millis(),
                    };
                    let json = serde_json::to_string(&heartbeat).unwrap();
                    if let Err(e) = write_half.send(WsMessage::Text(json)).await {
                        log::error!("[WS] Heartbeat send error: {}", e);
                        return;
                    }
                    log::debug!("[WS] Heartbeat sent");
                }
            }
        }
    }

    /// 处理收到的文本消息。
    async fn handle_incoming_text(&self, text: &str) {
        match serde_json::from_str::<MeshMessage>(text) {
            Ok(msg) => {
                log::debug!("[WS] Received {:?} from {}", msg.msg_type, msg.sender_id);
                // 转发到接收通道
                if let Err(e) = self.incoming_tx.try_send(msg) {
                    log::warn!("[WS] Incoming channel error: {}", e);
                }
            }
            Err(e) => {
                log::warn!("[WS] Invalid message JSON: {} — raw: {}", e, text);
            }
        }
    }
}

/// 构造一条剪贴板同步消息。
///
/// # 参数
/// - `device_id`: 本设备 ID
/// - `encrypted_b64`: Base64 编码的加密密文
/// - `content_hash`: 原始内容的 SHA-256 摘要
/// - `timestamp`: 时间戳（Unix 毫秒）
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

/// 简易 URL 百分号编码（RFC 3986 非保留字符不编码）。
///
/// 用于对 WebSocket URL 中的查询参数值进行编码，
/// 防止空格、中文、`...` 等字符导致 URL 解析失败。
fn url_encode(input: &str) -> String {
    let mut result = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            // 非保留字符：字母、数字、`-` `.` `_` `~` 不编码
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                result.push(byte as char);
            }
            // 其余字符百分号编码
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}
