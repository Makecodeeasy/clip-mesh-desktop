/**
 * clip-mesh-desktop — 前端入口
 *
 * 初始化 UI 并注册 Tauri 事件监听器。
 * 本前端采用原生 TypeScript 实现，无需 React/Vue 等框架依赖，
 * 保持极小的包体积与快速的启动性能。
 */

import { invoke } from "@tauri-apps/api/tauri";
import { listen } from "@tauri-apps/api/event";
import { AppConfig, initUI, updateStatusIndicator, updateConfigForm, appendLog } from "./app";
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
    if (!config.server_ip || !config.auth_token) {
      updateStatusIndicator("not_configured");
      appendLog("请填写服务器配置后点击「连接」", "info");
    } else {
      appendLog(`服务器: ${config.server_ip}:${config.server_port}`, "info");
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
      appendLog("已连接到服务端 ✓", "info");
    } else if (state === "Disconnected") {
      updateStatusIndicator("disconnected");
      appendLog("连接已断开", "warn");
    } else if (state === "Connecting") {
      updateStatusIndicator("connecting");
      appendLog("正在建立连接...", "info");
    } else if (state === "NotConfigured") {
      updateStatusIndicator("not_configured");
    } else if (state.startsWith("Reconnecting")) {
      updateStatusIndicator("connecting");
      // 从 "Reconnecting(3)" 中提取重试次数
      const match = state.match(/\((\d+)\)/);
      const attempt = match ? match[1] : "?";
      appendLog(`连接失败，第 ${attempt} 次重试中...`, "warn");
    }
  });

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
