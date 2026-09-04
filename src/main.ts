/**
 * clip-mesh-desktop — 前端入口
 *
 * 初始化 UI 并注册 Tauri 事件监听器。
 * 本前端采用原生 TypeScript 实现，无需 React/Vue 等框架依赖，
 * 保持极小的包体积与快速的启动性能。
 */

import { invoke } from "@tauri-apps/api/tauri";
import { listen } from "@tauri-apps/api/event";
import { AppConfig, initUI, updateStatusIndicator, updateConfigForm, addPeer, removePeer, appendLog, showSyncToast } from "./app";
import "./styles.css";

// ============================================================
// 应用启动
// ============================================================

document.addEventListener("DOMContentLoaded", async () => {
  // 初始化 UI 组件
  initUI();

  // 从 Rust 后端加载配置
  try {
    const config = await invoke<AppConfig>("get_config");
    updateConfigForm(config);
    appendLog("配置已加载", "info");

    // 根据配置有效性设置初始状态
    if (!config.pairing_code) {
      updateStatusIndicator("not_configured");
      appendLog("请生成或输入连接码后点击「启动 P2P」", "info");
    } else {
      appendLog(`设备: ${config.device_name || config.device_id.substring(0, 8)}`, "info");
      appendLog(`连接码: ${config.pairing_code}`, "info");
    }
  } catch (e) {
    appendLog(`加载配置失败: ${e}`, "error");
  }

  // 注册事件监听器
  registerEventListeners();
});

// ============================================================
// Tauri 事件监听
// ============================================================

/**
 * 注册来自 Rust 后端的事件监听器。
 *
 * 事件流：
 *   Rust (emit) → TypeScript (listen) → UI 更新
 */
function registerEventListeners() {
  // 连接状态变化
  listen<string>("connection-state", (event) => {
    const state = event.payload;
    console.log("[Frontend] Connection state:", state);

    // 映射 Rust 状态字符串到 UI 状态
    if (state === "Connected") {
      updateStatusIndicator("connected");
      appendLog("已连接到对等设备 ✓", "info");
    } else if (state === "Disconnected") {
      updateStatusIndicator("disconnected");
      appendLog("连接已断开", "warn");
    } else if (state === "Listening") {
      updateStatusIndicator("listening");
      appendLog("P2P 服务已启动，等待设备发现...", "info");
    } else if (state === "NotConfigured") {
      updateStatusIndicator("not_configured");
    }
  });

  // 对等设备事件（连接/断开）
  listen<{ type: string; device_id: string; device_name?: string }>(
    "peer-event",
    (event) => {
      const { type, device_id, device_name } = event.payload;
      if (type === "connected" && device_name) {
        addPeer(device_id, device_name);
        appendLog(`设备已连接: ${device_name}`, "info");
      } else if (type === "disconnected") {
        removePeer(device_id);
        appendLog(`设备已断开: ${device_id.substring(0, 8)}`, "warn");
      }
    }
  );

  // 配置更新通知
  listen<AppConfig>("config-updated", (event) => {
    console.log("[Frontend] Config updated");
    updateConfigForm(event.payload);
  });

  // 同步开关通知
  listen("sync-toggle", () => {
    console.log("[Frontend] Sync toggle event received");
    refreshSyncStatus();
  });

  // 托盘菜单「重新启动 P2P」
  listen("reconnect-request", async () => {
    console.log("[Frontend] Reconnect (restart P2P) requested from tray");
    appendLog("从托盘重新启动 P2P...", "info");
    try {
      await invoke("start_p2p");
      appendLog("P2P 服务已重新启动", "info");
    } catch (e) {
      appendLog(`重新启动失败: ${e}`, "error");
    }
  });

  // 剪贴板同步提示
  listen<{ direction: "in" | "out"; sender_id?: string; preview: string; chars: number }>(
    "clipboard-synced",
    (event) => {
      const { direction, sender_id, preview, chars } = event.payload;
      const label = direction === "out" ? "已发送" : "已接收";
      appendLog(`${label} ${chars} 字符${direction === "in" && sender_id ? ` (来自 ${sender_id.substring(0, 8)})` : ""}`, "info");
      showSyncToast(direction, preview, sender_id);
    }
  );
}

/**
 * 刷新同步状态显示。
 */
async function refreshSyncStatus() {
  try {
    const isSyncing = await invoke<boolean>("get_sync_status");
    const syncBtn = document.getElementById("btn-toggle-sync");
    if (syncBtn) {
      syncBtn.textContent = isSyncing ? "暂停同步" : "恢复同步";
      syncBtn.classList.toggle("active", isSyncing);
    }
  } catch (e) {
    console.error("[Frontend] Failed to get sync status:", e);
  }
}
