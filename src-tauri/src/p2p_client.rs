//! # P2P WebSocket 客户端模块
//!
//! 主动连接发现到的远端节点。
//! 使用 device_id 比较规则防止两端同时发起连接（只有一方主动连接）。

use crate::discovery::DiscoveredPeer;
use crate::p2p_server::PeerEvent;
use crate::websocket::{self, MeshMessage};
use futures_util::{SinkExt, StreamExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// 单条 WebSocket 消息最大字节数（10 MB）
const MAX_MSG_SIZE: usize = 10 * 1024 * 1024;

/// 连接到发现的远端节点。
///
/// 仅当本端 device_id < 远端 device_id 时才主动发起连接，
/// 避免两端同时连接导致重复。
pub async fn connect_to_peer(
    peer: DiscoveredPeer,
    our_device_id: &str,
    our_device_name: &str,
    incoming_tx: mpsc::Sender<MeshMessage>,
    state_tx: broadcast::Sender<PeerEvent>,
    peers: Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>,
) {
    // 防重复连接规则：device_id 较小的一方主动发起
    if our_device_id >= peer.device_id.as_str() {
        log::debug!(
            "[P2P Client] Skip connecting to {} (they should connect to us)",
            websocket::short_id(&peer.device_id)
        );
        return;
    }

    // 检查是否已连接
    {
        let peers_guard = peers.lock().await;
        if peers_guard.contains_key(&peer.device_id) {
            log::debug!(
                "[P2P Client] Already connected to {}",
                websocket::short_id(&peer.device_id)
            );
            return;
        }
    }

    let ws_url = format!(
        "ws://{}:{}/clipmesh?device_id={}&device_name={}",
        peer.ip,
        peer.port,
        url_encode(our_device_id),
        url_encode(our_device_name),
    );

    log::info!(
        "[P2P Client] Connecting to {} ({}) at ws://{}:{}...",
        peer.device_name,
        websocket::short_id(&peer.device_id),
        peer.ip,
        peer.port,
    );

    let (ws_stream, _) = match connect_async(&ws_url).await {
        Ok(result) => result,
        Err(e) => {
            log::warn!(
                "[P2P Client] Failed to connect to {}: {}",
                websocket::short_id(&peer.device_id),
                e
            );
            return;
        }
    };

    log::info!(
        "[P2P Client] Connected to {} ({})",
        peer.device_name,
        websocket::short_id(&peer.device_id)
    );

    let (mut write_half, mut read_half) = ws_stream.split();

    // 发送第一条消息（自我介绍，携带 device_name）
    let hello = websocket::build_heartbeat_message(our_device_id, our_device_name);
    let json = serde_json::to_string(&hello).unwrap();
    if let Err(e) = write_half.send(WsMessage::Text(json)).await {
        log::warn!("[P2P Client] Failed to send hello: {}", e);
        return;
    }

    // 创建 peer 发送通道
    let (peer_tx, mut peer_rx) = mpsc::channel::<MeshMessage>(64);

    // ---- H5: 原子化 check + insert（单次加锁） ----
    {
        let mut peers_guard = peers.lock().await;
        match peers_guard.entry(peer.device_id.clone()) {
            Entry::Occupied(_) => {
                log::debug!(
                    "[P2P Client] Already connected to {}, skipping",
                    websocket::short_id(&peer.device_id)
                );
                return;
            }
            Entry::Vacant(e) => {
                e.insert(peer_tx);
            }
        }
    }

    let _ = state_tx.send(PeerEvent::Connected {
        device_id: peer.device_id.clone(),
        device_name: peer.device_name.clone(),
        is_initiator: true,
    });

    // 读取循环（M5: 消息大小限制）
    let peer_id = peer.device_id.clone();
    let incoming_tx_clone = incoming_tx;
    let read_task = tokio::spawn(async move {
        while let Some(msg) = read_half.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if text.len() > MAX_MSG_SIZE {
                        log::warn!("[P2P Client] Message too large ({} bytes), dropping", text.len());
                        continue;
                    }
                    match serde_json::from_str::<MeshMessage>(&text) {
                        Ok(mesh_msg) => {
                            let _ = incoming_tx_clone.send(mesh_msg).await;
                        }
                        Err(e) => {
                            log::warn!("[P2P Client] Invalid message: {}", e);
                        }
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    log::info!("[P2P Client] Peer sent close frame");
                    break;
                }
                Err(e) => {
                    log::warn!("[P2P Client] Read error: {}", e);
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
                    log::error!("[P2P Client] Serialize error: {}", e);
                    continue;
                }
            };
            if let Err(e) = write_half.send(WsMessage::Text(json)).await {
                log::warn!("[P2P Client] Send error: {}", e);
                break;
            }
        }
    });

    // 等待任一任务结束
    tokio::select! {
        _ = read_task => {}
        _ = write_task => {}
    }

    // 清理
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.remove(&peer_id);
    }

    log::info!(
        "[P2P Client] Disconnected from {}",
        websocket::short_id(&peer_id)
    );
    let _ = state_tx.send(PeerEvent::Disconnected {
        device_id: peer_id,
    });
}

/// 简易 URL 百分号编码。
fn url_encode(input: &str) -> String {
    let mut result = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}
