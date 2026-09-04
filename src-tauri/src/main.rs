//! # clip-mesh-desktop — P2P 异构终端数据安全协同系统
//!
//! ## 系统架构（P2P 模式）
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │                       Tauri Application                       │
//! │                                                               │
//! │  ┌──────────────────── Rust Backend ────────────────────────┐│
//! │  │                                                          ││
//! │  │  ClipboardMonitor ──→ MeshCipher ──→ Peers Broadcast    ││
//! │  │      (监听本地)        (加密)         (发送到所有节点)     ││
//! │  │         ↑                                  │              ││
//! │  │         │                                  ↓              ││
//! │  │  SuppressGuard  ←── MeshCipher ←── Incoming Messages     ││
//! │  │    (防循环)          (解密)       (从任意节点接收)         ││
//! │  │                                                          ││
//! │  │  DiscoveryManager ←→ mDNS 自动发现                       ││
//! │  │  P2pServer       ←→ 接受入站连接                          ││
//! │  │  P2pClient       ←→ 发起出站连接                          ││
//! │  │                                                          ││
//! │  └──────────────────────────────────────────────────────────┘│
//! │                                                               │
//! │  ┌──────────────── TypeScript Frontend ─────────────────────┐│
//! │  │  配置面板 (加密密钥, 设备名称)                             ││
//! │  │  已连接设备列表                                           ││
//! │  └──────────────────────────────────────────────────────────┘│
//! └──────────────────────────────────────────────────────────────┘
//! ```

#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod clipboard;
mod config;
mod crypto;
mod discovery;
mod p2p_client;
mod p2p_server;
mod tray;
mod websocket;

use clipboard::{ClipboardBackend, ClipboardEvent, PlatformClipboard, SuppressGuard};
use clipboard::monitor::ClipboardMonitor;
use config::AppConfig;
use crypto::MeshCipher;
use discovery::{DiscoveryManager, DiscoveredPeer};
use p2p_server::{P2pServer, PeerEvent};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use websocket::{build_clipboard_message, MeshMessage, MessageType};

// ============================================================
// 全局状态
// ============================================================

struct AppState {
    config: std::sync::Mutex<AppConfig>,
    /// 剪贴板同步暂停标志（C1: 在 send/recv 任务中检查）
    sync_paused: Arc<AtomicBool>,
    /// 当前 P2P 任务句柄（C2: setup 和 start_p2p 共用）
    core_task: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// 取消令牌，用于 H1: 干净关闭所有子任务
    cancel_token: std::sync::Mutex<Option<CancellationToken>>,
}

// ============================================================
// Tauri 命令
// ============================================================

#[tauri::command]
fn get_config(state: tauri::State<AppState>) -> Result<AppConfig, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(config.clone())
}

#[tauri::command]
fn update_config(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
    new_config: AppConfig,
) -> Result<(), String> {
    new_config.save()?;

    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        *config = new_config.clone();
    }

    if let Some(window) = app_handle.get_window("main") {
        let _ = window.emit("config-updated", &new_config);
    }

    log::info!("[Cmd] Config saved to disk");
    Ok(())
}

/// 启动 P2P 服务
#[tauri::command]
fn start_p2p(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let config = {
        let cfg = state.config.lock().map_err(|e| e.to_string())?;
        cfg.clone()
    };

    if config.encryption_key_hex.is_empty() {
        return Err("请先设置加密密钥".to_string());
    }

    // H1 + C2: 取消旧任务（通过 token + abort 双保险）
    {
        let mut token = state.cancel_token.lock().map_err(|e| e.to_string())?;
        if let Some(t) = token.take() {
            t.cancel();
        }
    }
    {
        let mut task = state.core_task.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = task.take() {
            handle.abort();
            log::info!("[Cmd] Previous P2P services aborted");
        }
    }

    let sync_paused = state.sync_paused.clone();
    let new_token = CancellationToken::new();
    let token_clone = new_token.clone();

    {
        let mut token = state.cancel_token.lock().map_err(|e| e.to_string())?;
        *token = Some(new_token);
    }

    let handle_clone = app_handle.clone();
    let new_task = tauri::async_runtime::spawn(async move {
        run_p2p_services(config, handle_clone, sync_paused, token_clone).await;
    });

    {
        let mut task = state.core_task.lock().map_err(|e| e.to_string())?;
        *task = Some(new_task);
    }

    log::info!("[Cmd] P2P services started");
    Ok(())
}

#[tauri::command]
fn toggle_sync(state: tauri::State<AppState>) -> bool {
    let paused = state.sync_paused.fetch_xor(true, Ordering::SeqCst);
    let new_state = !paused;
    log::info!("[Cmd] Sync paused: {}", new_state);
    new_state
}

#[tauri::command]
fn get_sync_status(state: tauri::State<AppState>) -> bool {
    !state.sync_paused.load(Ordering::SeqCst)
}

// ============================================================
// 主入口
// ============================================================

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("╔══════════════════════════════════════════╗");
    log::info!("║  Clip Mesh Desktop v2.0.0 (P2P)         ║");
    log::info!("║  异构终端数据安全协同系统 — P2P 模式       ║");
    log::info!("╚══════════════════════════════════════════╝");

    let app_config = AppConfig::load();
    log::info!("[Main] Device ID: {}", app_config.device_id);
    log::info!("[Main] Device Name: {}", app_config.device_name);
    log::info!("[Main] Config valid: {}", app_config.is_valid());

    let sync_paused = Arc::new(AtomicBool::new(false));

    tauri::Builder::default()
        .system_tray(tray::build_system_tray())
        .on_system_tray_event(tray::handle_tray_event)

        .manage(AppState {
            config: std::sync::Mutex::new(app_config.clone()),
            sync_paused: sync_paused.clone(),
            core_task: std::sync::Mutex::new(None),
            cancel_token: std::sync::Mutex::new(None),
        })

        .invoke_handler(tauri::generate_handler![
            get_config,
            update_config,
            start_p2p,
            toggle_sync,
            get_sync_status,
        ])

        .setup(move |app| {
            let app_handle = app.handle();

            if app_config.is_valid() {
                log::info!("[Main] Config is valid, starting P2P...");

                // C2: 通过 app.state() 获取 AppState，存入 JoinHandle
                let state = app.state::<AppState>();
                let token = CancellationToken::new();
                let token_clone = token.clone();

                {
                    let mut t = state.cancel_token.lock().unwrap();
                    *t = Some(token);
                }

                let config = app_config.clone();
                let handle = app_handle.clone();
                let sp = sync_paused.clone();
                let join_handle = tauri::async_runtime::spawn(async move {
                    run_p2p_services(config, handle, sp, token_clone).await;
                });

                {
                    let mut task = state.core_task.lock().unwrap();
                    *task = Some(join_handle);
                }
            } else {
                log::warn!("[Main] No encryption key — waiting for user configuration");
                if let Some(window) = app_handle.get_window("main") {
                    let _ = window.emit("connection-state", "NotConfigured");
                }
            }

            Ok(())
        })

        .on_window_event(|event| match event.event() {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                event.window().hide().unwrap();
                api.prevent_close();
            }
            _ => {}
        })

        .run(tauri::generate_context!())
        .expect("Failed to run Clip Mesh Desktop");
}

// ============================================================
// P2P 核心服务编排
// ============================================================

async fn run_p2p_services(
    config: AppConfig,
    app_handle: tauri::AppHandle,
    sync_paused: Arc<AtomicBool>,
    cancel: CancellationToken,
) {
    log::info!("[P2P] Starting services...");

    // ---- 共享状态 ----
    let guard = SuppressGuard::new();
    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<ClipboardEvent>(64);
    let (incoming_tx, mut incoming_rx) = mpsc::channel::<MeshMessage>(256);
    let (peer_event_tx, _) = broadcast::channel::<PeerEvent>(32);

    // 已连接节点表：device_id → 发送通道
    let peers: Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // 初始化加密器
    let cipher = match MeshCipher::from_hex_key(&config.encryption_key_hex) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            log::error!("[P2P] Invalid encryption key: {} — aborting", e);
            return;
        }
    };

    // ---- 启动 P2P WebSocket Server ----
    let p2p_server = P2pServer::new(
        config.device_id.clone(),
        incoming_tx.clone(),
        peer_event_tx.clone(),
        peers.clone(),
    );

    let actual_port = match p2p_server.start(config.p2p_port).await {
        Ok(p) => p,
        Err(e) => {
            log::error!("[P2P] Failed to start server: {}", e);
            return;
        }
    };

    // ---- 启动 mDNS 发现 ----
    let mut discovery = match DiscoveryManager::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[P2P] Failed to create discovery manager: {}", e);
            return;
        }
    };

    if let Err(e) = discovery.register(&config.device_id, &config.device_name, actual_port) {
        log::error!("[P2P] Failed to register mDNS service: {}", e);
        return;
    }

    let (peer_discover_tx, mut peer_discover_rx) = mpsc::channel::<DiscoveredPeer>(32);
    if let Err(e) = discovery.browse(peer_discover_tx) {
        log::error!("[P2P] Failed to start mDNS browse: {}", e);
        return;
    }

    // ---- 启动剪贴板监听器（H1: 受 cancel token 控制） ----
    let clipboard_backend = Arc::new(PlatformClipboard::new());
    let monitor = ClipboardMonitor::new(
        clipboard_backend.clone(),
        guard.clone(),
        clipboard_tx,
    );
    let monitor_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = monitor.start() => {}
            _ = monitor_cancel.cancelled() => {
                log::info!("[P2P] Clipboard monitor cancelled");
            }
        }
    });

    // ---- 通知前端：正在监听 ----
    if let Some(window) = app_handle.get_window("main") {
        let _ = window.emit("connection-state", "Listening");
    }
    tray::update_tray_status(&app_handle, &format!("状态: 监听中 (端口 {})", actual_port));

    // ---- 节点发现处理任务（H1: 受 cancel token 控制） ----
    let our_id = config.device_id.clone();
    let our_name = config.device_name.clone();
    let incoming_clone = incoming_tx.clone();
    let peer_event_clone = peer_event_tx.clone();
    let peers_clone = peers.clone();
    let discover_cancel = cancel.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                peer = peer_discover_rx.recv() => {
                    match peer {
                        Some(p) => {
                            let incoming = incoming_clone.clone();
                            let peer_event = peer_event_clone.clone();
                            let peers = peers_clone.clone();
                            let id = our_id.clone();
                            let name = our_name.clone();

                            tokio::spawn(async move {
                                p2p_client::connect_to_peer(
                                    p, &id, &name, incoming, peer_event, peers,
                                )
                                .await;
                            });
                        }
                        None => break,
                    }
                }
                _ = discover_cancel.cancelled() => {
                    log::info!("[P2P] Peer discovery cancelled");
                    break;
                }
            }
        }
    });

    // ---- Peer 事件处理（更新前端）（H1: 受 cancel token 控制） ----
    let handle_peers = app_handle.clone();
    let mut peer_event_rx = peer_event_tx.subscribe();
    let peer_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                event = peer_event_rx.recv() => {
                    match event {
                        Ok(PeerEvent::Connected { device_id, device_name, .. }) => {
                            log::info!(
                                "[P2P] Peer connected: {} ({})",
                                device_name,
                                websocket::short_id(&device_id)
                            );
                            if let Some(window) = handle_peers.get_window("main") {
                                let _ = window.emit("connection-state", "Connected");
                                let _ = window.emit("peer-event", serde_json::json!({
                                    "type": "connected",
                                    "device_id": device_id,
                                    "device_name": device_name,
                                }));
                            }
                            tray::update_tray_status(&handle_peers, "状态: 已连接 ●");
                        }
                        Ok(PeerEvent::Disconnected { device_id }) => {
                            log::info!(
                                "[P2P] Peer disconnected: {}",
                                websocket::short_id(&device_id)
                            );
                            if let Some(window) = handle_peers.get_window("main") {
                                let _ = window.emit("peer-event", serde_json::json!({
                                    "type": "disconnected",
                                    "device_id": device_id,
                                }));
                            }
                        }
                        Err(_) => break,
                    }
                }
                _ = peer_cancel.cancelled() => {
                    log::info!("[P2P] Peer event handler cancelled");
                    break;
                }
            }
        }
    });

    // ---- 本地→远端广播任务（C1: 检查 sync_paused） ----
    let cipher_send = cipher.clone();
    let device_id_send = config.device_id.clone();
    let handle_send = app_handle.clone();
    let peers_send = peers.clone();
    let send_paused = sync_paused.clone();
    let send_cancel = cancel.clone();

    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                maybe_event = clipboard_rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            // C1: 暂停时丢弃事件（不发送到远端）
                            if send_paused.load(Ordering::Relaxed) {
                                continue;
                            }

                            let cipher = match &cipher_send {
                                Some(c) => c,
                                None => continue,
                            };

                            let content_hash = {
                                let mut hasher = Sha256::new();
                                hasher.update(event.text.as_bytes());
                                hex::encode(hasher.finalize())
                            };

                            match cipher.encrypt_text(&event.text, &[]) {
                                Ok(encrypted_b64) => {
                                    let msg = build_clipboard_message(
                                        &device_id_send,
                                        &encrypted_b64,
                                        &content_hash,
                                        event.timestamp,
                                    );

                                    // 广播到所有已连接节点
                                    let peers_guard = peers_send.lock().await;
                                    let peer_count = peers_guard.len();
                                    for (peer_id, sender) in peers_guard.iter() {
                                        if let Err(e) = sender.send(msg.clone()).await {
                                            log::warn!(
                                                "[Sync→] Failed to send to {}: {}",
                                                websocket::short_id(peer_id),
                                                e
                                            );
                                        }
                                    }

                                    if peer_count > 0 {
                                        log::info!(
                                            "[Sync→] Clipboard sent to {} peers ({} chars)",
                                            peer_count,
                                            event.text.len()
                                        );
                                        let preview: String = event.text.chars().take(20).collect();
                                        if let Some(window) = handle_send.get_window("main") {
                                            let _ = window.emit("clipboard-synced", serde_json::json!({
                                                "direction": "out",
                                                "preview": preview,
                                                "chars": event.text.len(),
                                            }));
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::error!("[Sync→] Encryption failed: {}", e);
                                }
                            }
                        }
                        None => break,
                    }
                }
                _ = send_cancel.cancelled() => {
                    log::info!("[P2P] Send task cancelled");
                    break;
                }
            }
        }
    });

    // ---- 远端→本地同步任务（C1: 检查 sync_paused，M3: 校验 content_hash） ----
    let cipher_recv = cipher.clone();
    let device_id_recv = config.device_id.clone();
    let handle_recv = app_handle.clone();
    let recv_paused = sync_paused.clone();
    let recv_cancel = cancel.clone();

    let recv_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                maybe_msg = incoming_rx.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            if msg.msg_type != MessageType::Clipboard {
                                continue;
                            }
                            if msg.sender_id == device_id_recv {
                                continue;
                            }

                            // C1: 暂停时丢弃远端消息（不写入本地剪贴板）
                            if recv_paused.load(Ordering::Relaxed) {
                                continue;
                            }

                            let cipher = match &cipher_recv {
                                Some(c) => c,
                                None => continue,
                            };

                            let payload: websocket::ClipboardPayload =
                                match serde_json::from_value(msg.payload) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        log::error!("[Sync←] Invalid payload: {}", e);
                                        continue;
                                    }
                                };

                            match cipher.decrypt_text(&payload.data, &[]) {
                                Ok(plaintext) => {
                                    // M3: 校验 content_hash
                                    let expected_hash = {
                                        let mut hasher = Sha256::new();
                                        hasher.update(plaintext.as_bytes());
                                        hex::encode(hasher.finalize())
                                    };
                                    if expected_hash != payload.content_hash {
                                        log::warn!(
                                            "[Sync←] Content hash mismatch from {} (possible tampering)",
                                            websocket::short_id(&msg.sender_id)
                                        );
                                    }

                                    let backend = PlatformClipboard::new();
                                    match backend.write_text(&plaintext, &guard) {
                                        Ok(_) => {
                                            log::info!(
                                                "[Sync←] Clipboard received from {} ({} chars)",
                                                websocket::short_id(&msg.sender_id),
                                                plaintext.len()
                                            );
                                            let preview: String = plaintext.chars().take(20).collect();
                                            if let Some(window) = handle_recv.get_window("main") {
                                                let _ = window.emit("clipboard-synced", serde_json::json!({
                                                    "direction": "in",
                                                    "sender_id": &msg.sender_id,
                                                    "preview": preview,
                                                    "chars": plaintext.len(),
                                                }));
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("[Sync←] Failed to write clipboard: {}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::error!("[Sync←] Decryption failed: {}", e);
                                }
                            }
                        }
                        None => break,
                    }
                }
                _ = recv_cancel.cancelled() => {
                    log::info!("[P2P] Receive task cancelled");
                    break;
                }
            }
        }
    });

    log::info!("[P2P] All services started (port {})", actual_port);

    // 持续运行，直到 cancel 或任一核心任务结束
    tokio::select! {
        _ = cancel.cancelled() => {
            log::info!("[P2P] Cancellation received, shutting down...");
        }
        _ = send_task => {
            log::warn!("[P2P] Send task ended unexpectedly");
        }
        _ = recv_task => {
            log::warn!("[P2P] Receive task ended unexpectedly");
        }
    }

    // discovery 会在 drop 时自动清理（Drop trait）
    log::info!("[P2P] Services stopped");
}
