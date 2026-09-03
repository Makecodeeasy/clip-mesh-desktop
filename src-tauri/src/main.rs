//! # clip-mesh-desktop — 异构终端数据安全协同系统桌面客户端
//!
//! ## 系统架构
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │                       Tauri Application                       │
//! │                                                               │
//! │  ┌──────────────────── Rust Backend ────────────────────────┐│
//! │  │                                                          ││
//! │  │  ClipboardMonitor ──→ MeshCipher ──→ MeshWsClient ──→   ││
//! │  │      (监听本地)        (加密)         (发送到服务端)       ││
//! │  │         ↑                                  │              ││
//! │  │         │                                  ↓              ││
//! │  │  SuppressGuard  ←── MeshCipher ←── MeshWsClient          ││
//! │  │    (防循环)          (解密)       (接收远端消息)           ││
//! │  │                                                          ││
//! │  │  SystemTray ←── 连接状态广播                               ││
//! │  │                                                          ││
//! │  └──────────────────────────────────────────────────────────┘│
//! │                                                               │
//! │  ┌──────────────── TypeScript Frontend ─────────────────────┐│
//! │  │  配置面板 (Server IP/Port, Token)                         ││
//! │  │  连接状态指示灯                                           ││
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
mod tray;
mod websocket;

use clipboard::{ClipboardEvent, PlatformClipboard, SuppressGuard, ClipboardBackend};
use clipboard::monitor::ClipboardMonitor;
use config::AppConfig;
use crypto::MeshCipher;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::mpsc;
use websocket::{build_clipboard_message, ConnectionState, MeshMessage, MeshWsClient, WsClientConfig, MessageType};

// ============================================================
// 全局状态（供 Tauri 命令与前端访问）
// ============================================================

/// 应用运行时状态，通过 Tauri 的 `manage()` 机制注入。
struct AppState {
    config: std::sync::Mutex<AppConfig>,
    ws_sender: std::sync::Mutex<Option<mpsc::Sender<MeshMessage>>>,
    sync_paused: std::sync::atomic::AtomicBool,
    /// 核心服务任务的 JoinHandle，用于在重连时 abort 旧任务
    core_task: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// 用于向 UI 发送连接状态的广播通道
    state_broadcaster: std::sync::Mutex<Option<tokio::sync::broadcast::Sender<ConnectionState>>>,
}

// ============================================================
// Tauri 命令（供前端 TypeScript 调用）
// ============================================================

/// 获取当前配置（前端读取）
#[tauri::command]
fn get_config(state: tauri::State<AppState>) -> Result<AppConfig, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    Ok(config.clone())
}

/// 更新配置（前端写入）— 仅保存，不触发重连
#[tauri::command]
fn update_config(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
    new_config: AppConfig,
) -> Result<(), String> {
    // 保存到磁盘
    new_config.save()?;

    // 更新运行时配置
    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        *config = new_config.clone();
    }

    // 通知前端配置已更新
    if let Some(window) = app_handle.get_window("main") {
        let _ = window.emit("config-updated", &new_config);
    }

    log::info!("[Cmd] Config saved to disk");
    Ok(())
}

/// 连接/重连服务端 — abort 旧的核心服务任务，用当前配置重新启动
#[tauri::command]
fn connect(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // 1. 读取当前配置
    let config = {
        let cfg = state.config.lock().map_err(|e| e.to_string())?;
        cfg.clone()
    };

    // 2. 校验配置完整性
    if config.server_ip.is_empty() || config.server_port == 0 {
        return Err("请先填写 Server IP 和 Port".to_string());
    }

    // 3. abort 旧的核心服务任务
    {
        let mut task = state.core_task.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = task.take() {
            handle.abort();
            log::info!("[Cmd] Previous core services aborted");
        }
    }

    // 4. 启动新的核心服务
    let handle_clone = app_handle.clone();
    let new_task = tauri::async_runtime::spawn(async move {
        run_core_services(config, handle_clone).await;
    });

    // 5. 保存 JoinHandle
    {
        let mut task = state.core_task.lock().map_err(|e| e.to_string())?;
        *task = Some(new_task);
    }

    log::info!("[Cmd] Core services restarted with current config");
    Ok(())
}

/// 切换同步暂停/恢复
#[tauri::command]
fn toggle_sync(state: tauri::State<AppState>) -> bool {
    let paused = state.sync_paused.fetch_xor(true, std::sync::atomic::Ordering::SeqCst);
    let new_state = !paused;
    log::info!("[Cmd] Sync paused: {}", new_state);
    new_state
}

/// 获取当前同步状态
#[tauri::command]
fn get_sync_status(state: tauri::State<AppState>) -> bool {
    !state.sync_paused.load(std::sync::atomic::Ordering::SeqCst)
}

// ============================================================
// 主入口
// ============================================================

fn main() {
    // 初始化日志
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("╔══════════════════════════════════════════╗");
    log::info!("║  Clip Mesh Desktop v1.0.0                ║");
    log::info!("║  异构终端数据安全协同系统 — 桌面客户端      ║");
    log::info!("╚══════════════════════════════════════════╝");

    // 加载配置
    let app_config = AppConfig::load();
    log::info!("[Main] Device ID: {}", app_config.device_id);
    log::info!("[Main] Device Name: {}", app_config.device_name);
    log::info!("[Main] Server: {}:{}", app_config.server_ip, app_config.server_port);
    log::info!("[Main] Config valid: {}", app_config.is_valid());

    // 构建 Tauri 应用
    tauri::Builder::default()
        // 注册系统托盘
        .system_tray(tray::build_system_tray())
        .on_system_tray_event(tray::handle_tray_event)

        // 注入全局状态
        .manage(AppState {
            config: std::sync::Mutex::new(app_config.clone()),
            ws_sender: std::sync::Mutex::new(None),
            sync_paused: std::sync::atomic::AtomicBool::new(false),
            core_task: std::sync::Mutex::new(None),
            state_broadcaster: std::sync::Mutex::new(None),
        })

        // 注册 Tauri 命令
        .invoke_handler(tauri::generate_handler![
            get_config,
            update_config,
            connect,
            toggle_sync,
            get_sync_status,
        ])

        // 应用启动回调
        .setup(move |app| {
            let app_handle = app.handle();

            // 仅当配置有效时才自动连接
            if app_config.is_valid() {
                log::info!("[Main] Config is valid, auto-connecting...");
                let config = app_config.clone();
                let handle = app_handle.clone();

                let _task = tauri::async_runtime::spawn(async move {
                    run_core_services(config, handle).await;
                });

                // 需要在 setup 之后保存 JoinHandle，通过 emit 延迟处理
                // 这里简单处理：不保存 handle，首次 connect 命令会重新启动
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    // JoinHandle 无法跨线程安全传递到此处的 state，
                    // 但首次 connect 命令会 abort 并重启，所以这里不需要精确管理
                });
            } else {
                log::warn!("[Main] Config is incomplete — waiting for user to fill in settings and click Connect");
                // 通知前端显示"未配置"状态
                if let Some(window) = app_handle.get_window("main") {
                    let _ = window.emit("connection-state", "NotConfigured");
                }
            }

            Ok(())
        })

        // 窗口关闭事件：隐藏而非退出（保持托盘常驻）
        .on_window_event(|event| match event.event() {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // 阻止关闭，改为隐藏到托盘
                event.window().hide().unwrap();
                api.prevent_close();
            }
            _ => {}
        })

        .run(tauri::generate_context!())
        .expect("Failed to run Clip Mesh Desktop");
}

// ============================================================
// 核心服务编排
// ============================================================

/// 启动所有核心后台服务。
///
/// 此异步函数编排以下组件的生命周期：
/// 1. WebSocket 客户端连接
/// 2. 剪贴板监听器
/// 3. 本地→远端同步（加密+发送）
/// 4. 远端→本地同步（接收+解密+注入）
async fn run_core_services(config: AppConfig, app_handle: tauri::AppHandle) {
    log::info!("[Core] Starting services for {}:{}", config.server_ip, config.server_port);

    // ---- 初始化共享组件 ----

    // 防循环哨兵（监听器与注入器共享同一实例）
    let guard = SuppressGuard::new();

    // 剪贴板事件通道
    let (clipboard_tx, mut clipboard_rx) = mpsc::channel::<ClipboardEvent>(64);

    // 初始化加密器
    let cipher = if !config.encryption_key_hex.is_empty() {
        match MeshCipher::from_hex_key(&config.encryption_key_hex) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                log::warn!("[Core] Invalid encryption key: {} — clipboard sync disabled", e);
                None
            }
        }
    } else {
        log::warn!("[Core] No encryption key configured — clipboard sync disabled");
        None
    };

    // ---- 启动 WebSocket 客户端 ----
    let ws_config = WsClientConfig {
        server_url: config.ws_url(),
        device_id: config.device_id.clone(),
        device_name: config.device_name.clone(),
        auth_token: config.auth_token.clone(),
    };

    let (ws_client, mut incoming_rx, mut state_rx) = MeshWsClient::new(ws_config);
    let ws_sender = ws_client.sender();

    // WebSocket 连接主循环（独立任务）
    tokio::spawn(ws_client.start());

    // ---- 启动剪贴板监听器 ----
    let clipboard_backend = Arc::new(PlatformClipboard::new());
    let monitor = ClipboardMonitor::new(
        clipboard_backend.clone(),
        guard.clone(),
        clipboard_tx,
    );

    // 剪贴板监听循环（独立任务）
    tokio::spawn(async move {
        monitor.start().await;
    });

    // ---- 状态广播任务（更新托盘 + 前端） ----
    let handle_state = app_handle.clone();
    let state_task = tokio::spawn(async move {
        while let Ok(state) = state_rx.recv().await {
            let status_text = match &state {
                ConnectionState::Connected => "状态: 已连接 ●",
                ConnectionState::Connecting => "状态: 连接中...",
                ConnectionState::Reconnecting(_) => "状态: 重连中...",
                ConnectionState::Disconnected => "状态: 已断开 ○",
            };
            tray::update_tray_status(&handle_state, status_text);

            // 同步通知前端
            if let Some(window) = handle_state.get_window("main") {
                let state_str = match &state {
                    ConnectionState::Connected => "Connected",
                    ConnectionState::Connecting => "Connecting",
                    ConnectionState::Reconnecting(n) => &format!("Reconnecting({})", n).to_string().clone(),
                    ConnectionState::Disconnected => "Disconnected",
                };
                let _ = window.emit("connection-state", state_str);
            }
        }
    });

    // ---- 本地→远端同步任务 ----
    let cipher_send = cipher.clone();
    let device_id_send = config.device_id.clone();

    let send_task = tokio::spawn(async move {
        while let Some(event) = clipboard_rx.recv().await {
            let cipher = match &cipher_send {
                Some(c) => c,
                None => {
                    log::warn!("[Sync→] No cipher available, skipping");
                    continue;
                }
            };

            // 计算内容哈希（用于去重）
            let content_hash = {
                let mut hasher = Sha256::new();
                hasher.update(event.text.as_bytes());
                hex::encode(hasher.finalize())
            };

            // AES-256-GCM 加密
            match cipher.encrypt_text(&event.text, &[]) {
                Ok(encrypted_b64) => {
                    let msg = build_clipboard_message(
                        &device_id_send,
                        &encrypted_b64,
                        &content_hash,
                        event.timestamp,
                    );

                    if let Err(e) = ws_sender.send(msg).await {
                        log::error!("[Sync→] Failed to send: {}", e);
                    } else {
                        log::info!("[Sync→] Clipboard sent ({} chars)", event.text.len());
                    }
                }
                Err(e) => {
                    log::error!("[Sync→] Encryption failed: {}", e);
                }
            }
        }
    });

    // ---- 远端→本地同步任务 ----
    let cipher_recv = cipher.clone();
    let device_id_recv = config.device_id.clone();

    let recv_task = tokio::spawn(async move {
        while let Some(msg) = incoming_rx.recv().await {
            // 仅处理剪贴板类型消息
            if msg.msg_type != MessageType::Clipboard {
                continue;
            }

            // 忽略自己发出的消息
            if msg.sender_id == device_id_recv {
                continue;
            }

            let cipher = match &cipher_recv {
                Some(c) => c,
                None => {
                    log::warn!("[Sync←] No cipher available, skipping");
                    continue;
                }
            };

            let payload: websocket::ClipboardPayload = match serde_json::from_value(msg.payload) {
                Ok(p) => p,
                Err(e) => {
                    log::error!("[Sync←] Invalid payload: {}", e);
                    continue;
                }
            };

            match cipher.decrypt_text(&payload.data, &[]) {
                Ok(plaintext) => {
                    let backend = PlatformClipboard::new();
                    match backend.write_text(&plaintext, &guard) {
                        Ok(_) => {
                            log::info!(
                                "[Sync←] Clipboard received from {} ({} chars)",
                                msg.sender_id,
                                plaintext.len()
                            );
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
    });

    log::info!("[Core] All services started");

    // 等待任一子任务结束（通常意味着连接失败或通道关闭）
    tokio::select! {
        _ = state_task => { log::warn!("[Core] State broadcast task ended"); }
        _ = send_task => { log::warn!("[Core] Send task ended"); }
        _ = recv_task => { log::warn!("[Core] Receive task ended"); }
    }

    log::info!("[Core] Services stopped");
}
