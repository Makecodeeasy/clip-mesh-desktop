//! # P2P WebSocket 服务端模块
//!
//! 每个节点运行一个本地 WebSocket server，接受其他节点的连接。
//! 连接建立后进入消息收发循环，与共享消息总线交互。

use crate::websocket::{self, MeshMessage};
use futures_util::{SinkExt, StreamExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// 单条 WebSocket 消息最大字节数（10 MB）
const MAX_MSG_SIZE: usize = 10 * 1024 * 1024;
/// 等待首条自我介绍消息的超时时间
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// P2P WebSocket 服务端。
///
/// 监听本地端口，接受其他节点的 WebSocket 连接。
/// 每个连接独立收发消息，通过共享 channel 与主逻辑交互。
pub struct P2pServer {
    /// 本设备 ID
    device_id: String,
    /// 接收到的远端消息 → 发送给主逻辑
    incoming_tx: mpsc::Sender<MeshMessage>,
    /// 连接状态广播
    state_tx: broadcast::Sender<PeerEvent>,
    /// 已连接的节点（device_id → 发送通道）
    peers: Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>,
}

/// 对等节点连接事件
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// 新节点已连接（device_id, device_name, 是否为主动发起方）
    Connected {
        device_id: String,
        device_name: String,
        is_initiator: bool,
    },
    /// 节点已断开
    Disconnected { device_id: String },
}

impl P2pServer {
    pub fn new(
        device_id: String,
        incoming_tx: mpsc::Sender<MeshMessage>,
        state_tx: broadcast::Sender<PeerEvent>,
        peers: Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>,
    ) -> Self {
        Self {
            device_id,
            incoming_tx,
            state_tx,
            peers,
        }
    }

    /// 启动 WebSocket server，监听指定端口。
    ///
    /// 返回实际监听的端口号（当请求端口为 0 时有用）。
    pub async fn start(self, port: u16) -> Result<u16, String> {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind {}: {}", addr, e))?;

        let actual_port = listener.local_addr().unwrap().port();
        log::info!("[P2P Server] Listening on 0.0.0.0:{}", actual_port);

        let incoming_tx = self.incoming_tx;
        let state_tx = self.state_tx;
        let peers = self.peers;
        let device_id = self.device_id;

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        log::info!("[P2P Server] Incoming connection from {}", addr);
                        let incoming_tx = incoming_tx.clone();
                        let state_tx = state_tx.clone();
                        let peers = peers.clone();
                        let our_id = device_id.clone();

                        tokio::spawn(async move {
                            match accept_async(stream).await {
                                Ok(ws_stream) => {
                                    handle_incoming_connection(
                                        ws_stream,
                                        our_id,
                                        incoming_tx,
                                        state_tx,
                                        peers,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[P2P Server] WebSocket handshake failed from {}: {}",
                                        addr,
                                        e
                                    );
                                }
                            }
                        });
                    }
                    Err(e) => {
                        log::error!("[P2P Server] Accept error: {}", e);
                    }
                }
            }
        });

        Ok(actual_port)
    }
}

/// 处理一个入站的 WebSocket 连接。
async fn handle_incoming_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    _our_id: String,
    incoming_tx: mpsc::Sender<MeshMessage>,
    state_tx: broadcast::Sender<PeerEvent>,
    peers: Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>,
) {
    let (mut write_half, mut read_half) = ws_stream.split();

    // 用于接收来自主逻辑的待发送消息
    let (peer_tx, mut peer_rx) = mpsc::channel::<MeshMessage>(64);

    // ---- H3: 等待首条消息，带超时 ----
    let first_text = match timeout(HELLO_TIMEOUT, read_half.next()).await {
        Ok(Some(Ok(WsMessage::Text(text)))) => text,
        Ok(Some(Ok(WsMessage::Close(_)))) | Ok(None) => {
            log::warn!("[P2P Server] Connection closed before first message");
            return;
        }
        Ok(Some(Err(e))) => {
            log::warn!("[P2P Server] Read error on first message: {}", e);
            return;
        }
        Err(_) => {
            log::warn!("[P2P Server] Timeout waiting for first message ({}s)", HELLO_TIMEOUT.as_secs());
            return;
        }
        _ => return,
    };

    // 解析第一条消息（心跳 / 自我介绍）
    let first_parsed: MeshMessage = match serde_json::from_str(&first_text) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("[P2P Server] Invalid first message: {}", e);
            return;
        }
    };

    let peer_device_id = first_parsed.sender_id.clone();

    // ---- H4: 从心跳消息提取 device_name ----
    let peer_device_name = first_parsed
        .payload
        .get("device_name")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| websocket::short_id(&peer_device_id))
        .to_string();

    // ---- H5: 原子化 check + insert（单次加锁） ----
    {
        let mut peers_guard = peers.lock().await;
        match peers_guard.entry(peer_device_id.clone()) {
            Entry::Occupied(_) => {
                log::info!(
                    "[P2P Server] Already connected to {}, rejecting duplicate",
                    websocket::short_id(&peer_device_id)
                );
                return;
            }
            Entry::Vacant(e) => {
                e.insert(peer_tx.clone());
            }
        }
    }

    log::info!(
        "[P2P Server] Peer connected: {} ({})",
        peer_device_name,
        websocket::short_id(&peer_device_id)
    );
    let _ = state_tx.send(PeerEvent::Connected {
        device_id: peer_device_id.clone(),
        device_name: peer_device_name,
        is_initiator: false,
    });

    // 转发第一条消息到主逻辑
    let _ = incoming_tx.send(first_parsed).await;

    // ---- 读取循环（M5: 消息大小限制） ----
    let peer_id_clone = peer_device_id.clone();
    let read_task = tokio::spawn(async move {
        while let Some(msg) = read_half.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if text.len() > MAX_MSG_SIZE {
                        log::warn!("[P2P Server] Message too large ({} bytes), dropping", text.len());
                        continue;
                    }
                    match serde_json::from_str::<MeshMessage>(&text) {
                        Ok(mesh_msg) => {
                            let _ = incoming_tx.send(mesh_msg).await;
                        }
                        Err(e) => {
                            log::warn!("[P2P Server] Invalid message: {}", e);
                        }
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    log::info!(
                        "[P2P Server] Peer {} sent close frame",
                        websocket::short_id(&peer_id_clone)
                    );
                    break;
                }
                Err(e) => {
                    log::warn!("[P2P Server] Read error from peer: {}", e);
                    break;
                }
                _ => {}
            }
        }
    });

    // 发送循环
    let write_task = tokio::spawn(async move {
        while let Some(msg) = peer_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    log::error!("[P2P Server] Serialize error: {}", e);
                    continue;
                }
            };
            if let Err(e) = write_half.send(WsMessage::Text(json)).await {
                log::warn!("[P2P Server] Send error: {}", e);
                break;
            }
        }
    });

    // 等待任一任务结束
    tokio::select! {
        _ = read_task => {}
        _ = write_task => {}
    }

    // 清理：从 peers 表移除
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.remove(&peer_device_id);
    }

    log::info!(
        "[P2P Server] Peer {} disconnected",
        websocket::short_id(&peer_device_id)
    );
    let _ = state_tx.send(PeerEvent::Disconnected {
        device_id: peer_device_id,
    });
}
