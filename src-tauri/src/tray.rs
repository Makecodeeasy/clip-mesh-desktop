//! # 系统托盘模块
//!
//! 实现 clip-mesh 桌面客户端的系统托盘（System Tray）功能，
//! 提供后台常驻运行与快捷操作入口。
//!
//! ## 功能
//!
//! - 托盘图标显示（连接状态可视化）
//! - 右键菜单：显示/隐藏主窗口、暂停/恢复同步、退出应用
//! - 左键单击：显示/隐藏主窗口
//!
//! ## Tauri 集成
//!
//! 本模块基于 Tauri 的 `SystemTray` API 实现，在 `tauri.conf.json` 中
//! 已配置 `systemTray.iconPath` 和 `iconAsTemplate: true`（macOS 模板图标）。

use tauri::{
    AppHandle, CustomMenuItem, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem, SystemTraySubmenu,
};

/// 构建系统托盘实例。
///
/// 创建包含菜单项的系统托盘，菜单结构如下：
///
/// ```text
/// ┌─────────────────────┐
/// │ 显示窗口            │
/// │ ─────────────────── │
/// │ 暂停同步            │
/// │ 重新连接            │
/// │ ─────────────────── │
/// │ 状态: 已连接 ●      │  ← 子菜单（显示节点列表）
/// │   ├── 节点 A (在线) │
/// │   └── 节点 B (在线) │
/// │ ─────────────────── │
/// │ 关于 Clip Mesh      │
/// │ 退出                │
/// └─────────────────────┘
/// ```
pub fn build_system_tray() -> SystemTray {
    // ---- 菜单项定义 ----

    // 显示/隐藏主窗口
    let show_window = CustomMenuItem::new("show_window", "显示窗口");

    // 暂停/恢复剪贴板同步
    let pause_sync = CustomMenuItem::new("pause_sync", "暂停同步");

    // 手动重新连接
    let reconnect = CustomMenuItem::new("reconnect", "重新连接");

    // 状态子菜单（占位，运行时动态更新）
    let status_item = CustomMenuItem::new("status_display", "状态: 未连接").disabled();
    let status_submenu = SystemTraySubmenu::new(
        "节点列表",
        SystemTrayMenu::new().add_item(
            CustomMenuItem::new("no_nodes", "暂无在线节点").disabled(),
        ),
    );

    // 关于
    let about = CustomMenuItem::new("about", "关于 Clip Mesh v1.0.0");

    // 退出
    let quit = CustomMenuItem::new("quit", "退出");

    // ---- 组装菜单 ----
    let menu = SystemTrayMenu::new()
        .add_item(show_window)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(pause_sync)
        .add_item(reconnect)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(status_item)
        .add_submenu(status_submenu)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(about)
        .add_item(quit);

    SystemTray::new().with_menu(menu)
}

/// 注册系统托盘事件处理器。
///
/// 在 Tauri Builder 中通过 `.on_system_tray_event()` 注册，
/// 处理托盘菜单点击与图标交互事件。
pub fn handle_tray_event(app: &AppHandle, event: SystemTrayEvent) {
    match event {
        // ---- 托盘图标左键点击 ----
        SystemTrayEvent::LeftClick { .. } => {
            toggle_main_window(app);
        }

        // ---- 菜单项点击 ----
        SystemTrayEvent::MenuItemClick { id, .. } => {
            match id.as_str() {
                "show_window" => {
                    toggle_main_window(app);
                }

                "pause_sync" => {
                    // 切换同步状态
                    // TODO: 通过 Tauri State 访问全局暂停标志
                    log::info!("[Tray] Pause/Resume sync toggled");

                    // 更新菜单文本
                    if let Some(window) = app.get_window("main") {
                        let _ = window.emit("sync-toggle", ());
                    }
                }

                "reconnect" => {
                    log::info!("[Tray] Manual reconnect requested");
                    if let Some(window) = app.get_window("main") {
                        let _ = window.emit("reconnect-request", ());
                    }
                }

                "about" => {
                    // 显示关于对话框（使用系统通知简化实现）
                    use tauri::api::notification::Notification;
                    let _ = Notification::new(&app.config().tauri.bundle.identifier)
                        .title("Clip Mesh")
                        .body("异构终端数据安全协同系统 v1.0.0\n© 2026 Makecodeeasy")
                        .show();
                }

                "quit" => {
                    log::info!("[Tray] Quit requested");
                    app.exit(0);
                }

                _ => {
                    log::debug!("[Tray] Unhandled menu item: {}", id);
                }
            }
        }

        // ---- 双击事件 ----
        SystemTrayEvent::DoubleClick { .. } => {
            toggle_main_window(app);
        }

        _ => {}
    }
}

/// 切换主窗口的显示/隐藏状态。
fn toggle_main_window(app: &AppHandle) {
    if let Some(window) = app.get_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            log::debug!("[Tray] Window hidden");
        } else {
            let _ = window.show();
            let _ = window.set_focus();
            log::debug!("[Tray] Window shown");
        }
    }
}

/// 运行时更新托盘菜单中的状态文本。
///
/// 供 WebSocket 连接状态变化时调用，实时反映连接状态到托盘菜单。
pub fn update_tray_status(app: &AppHandle, status_text: &str) {
    if let Some(tray) = app.tray_handle().try_get_item("status_display") {
        let _ = tray.set_title(status_text);
    }
}
