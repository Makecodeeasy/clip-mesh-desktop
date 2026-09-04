/**
 * clip-mesh-desktop — UI 组件与交互逻辑
 *
 * 提供极简的配置界面，包含：
 * - Server IP / Port 设置
 * - Auth Token 输入
 * - 加密密钥输入
 * - 节点连接状态指示灯
 * - 连接按钮 / 同步开关
 */

import { invoke } from "@tauri-apps/api/tauri";

// ============================================================
// 类型定义
// ============================================================

/** 应用配置（与 Rust 端 AppConfig 对应） */
export interface AppConfig {
  server_ip: string;
  server_port: number;
  auth_token: string;
  device_id: string;
  device_name: string;
  encryption_key_hex: string;
  auto_start: boolean;
  sync_enabled: boolean;
}

/** 连接状态类型 */
export type ConnectionState = "connected" | "disconnected" | "connecting" | "not_configured";

// ============================================================
// UI 初始化
// ============================================================

/**
 * 初始化 UI 组件。
 *
 * 动态生成 HTML 结构并绑定事件处理器。
 * 采用模板字符串构建 UI 以保持零依赖。
 */
export function initUI(): void {
  const app = document.getElementById("app");
  if (!app) return;

  app.innerHTML = `
    <div class="container">
      <header class="header">
        <h1>✂️ Clip Mesh</h1>
        <p class="subtitle">异构终端数据安全协同系统</p>
      </header>

      <!-- 连接状态 -->
      <section class="status-section">
        <div class="status-row">
          <span class="status-label">连接状态</span>
          <span id="status-indicator" class="status-dot not-configured" title="未配置"></span>
          <span id="status-text" class="status-text">未配置</span>
        </div>
        <div class="status-row">
          <span class="status-label">设备 ID</span>
          <span id="device-id" class="device-id">-</span>
        </div>
      </section>

      <!-- 服务器配置 -->
      <section class="config-section">
        <h2>服务器设置</h2>

        <div class="form-row">
          <label for="input-server-ip">Server IP</label>
          <input
            type="text"
            id="input-server-ip"
            placeholder="192.168.1.100"
            class="form-input"
          />
        </div>

        <div class="form-row">
          <label for="input-server-port">Port</label>
          <input
            type="number"
            id="input-server-port"
            placeholder="8080"
            class="form-input form-input-short"
          />
        </div>

        <div class="form-row">
          <label for="input-auth-token">Auth Token</label>
          <input
            type="text"
            id="input-auth-token"
            placeholder="v1.xxxx.yyyy"
            class="form-input"
          />
        </div>

        <div class="form-row">
          <label for="input-enc-key">加密密钥 (Hex)</label>
          <input
            type="text"
            id="input-enc-key"
            placeholder="64 位十六进制字符..."
            class="form-input"
          />
        </div>

        <div class="form-row">
          <label for="input-device-name">设备名称</label>
          <input
            type="text"
            id="input-device-name"
            placeholder="My MacBook"
            class="form-input"
          />
        </div>
      </section>

      <!-- 操作按钮 -->
      <section class="actions-section">
        <button id="btn-connect" class="btn btn-primary">连接</button>
        <button id="btn-toggle-sync" class="btn btn-secondary" disabled>暂停同步</button>
      </section>

      <!-- 日志区域 -->
      <section class="log-section">
        <div id="log-output" class="log-output"></div>
      </section>

      <footer class="footer">
        <span>Clip Mesh v1.0.0</span>
        <span>·</span>
        <span id="sync-status">未同步</span>
      </footer>

      <!-- 同步提示 toast -->
      <div id="sync-toast" class="sync-toast"></div>
    </div>
  `;

  // 绑定事件
  bindEvents();
}

// ============================================================
// 事件绑定
// ============================================================

function bindEvents(): void {
  // 连接按钮 — 保存配置并触发连接/重连
  const connectBtn = document.getElementById("btn-connect");
  connectBtn?.addEventListener("click", handleConnect);

  // 切换同步
  const syncBtn = document.getElementById("btn-toggle-sync");
  syncBtn?.addEventListener("click", handleToggleSync);

  // 输入框回车 → 触发连接
  const inputs = document.querySelectorAll(".form-input");
  inputs.forEach((input) => {
    input.addEventListener("keydown", (e: Event) => {
      if ((e as KeyboardEvent).key === "Enter") {
        handleConnect();
      }
    });
  });
}

/**
 * 处理「连接」按钮点击。
 *
 * 流程：
 * 1. 从表单收集配置
 * 2. 调用 update_config 保存到磁盘
 * 3. 调用 connect 重启核心服务
 */
async function handleConnect(): Promise<void> {
  const connectBtn = document.getElementById("btn-connect") as HTMLButtonElement | null;
  if (connectBtn) {
    connectBtn.disabled = true;
    connectBtn.textContent = "连接中...";
  }

  // 从 Rust 后端读取当前配置（确保 device_id 不被 UI 截断值覆盖）
  let currentConfig: AppConfig;
  try {
    currentConfig = await invoke<AppConfig>("get_config");
  } catch {
    currentConfig = { device_id: "", server_ip: "", server_port: 8080, auth_token: "", device_name: "", encryption_key_hex: "", auto_start: true, sync_enabled: true };
  }

  // 收集配置（表单值覆盖，device_id 始终从后端获取）
  const config: AppConfig = {
    server_ip: getInputValue("input-server-ip") || currentConfig.server_ip,
    server_port: parseInt(getInputValue("input-server-port")) || currentConfig.server_port || 8080,
    auth_token: getInputValue("input-auth-token") || currentConfig.auth_token,
    device_id: currentConfig.device_id, // 始终使用后端存储的完整 device_id
    device_name: getInputValue("input-device-name") || currentConfig.device_name,
    encryption_key_hex: getInputValue("input-enc-key") || currentConfig.encryption_key_hex,
    auto_start: true,
    sync_enabled: true,
  };

  // 基础校验
  if (!config.server_ip) {
    appendLog("请填写 Server IP", "error");
    resetConnectButton();
    return;
  }

  try {
    // 步骤 1: 保存配置
    await invoke("update_config", { newConfig: config });
    appendLog("配置已保存", "info");

    // 步骤 2: 触发连接
    await invoke("connect");
    appendLog("正在连接 " + config.server_ip + ":" + config.server_port + " ...", "info");

    // 启用同步按钮
    const syncBtn = document.getElementById("btn-toggle-sync");
    if (syncBtn) syncBtn.removeAttribute("disabled");

  } catch (e) {
    appendLog(`连接失败: ${e}`, "error");
    resetConnectButton();
  }
}

function resetConnectButton(): void {
  const connectBtn = document.getElementById("btn-connect") as HTMLButtonElement | null;
  if (connectBtn) {
    connectBtn.disabled = false;
    connectBtn.textContent = "连接";
  }
}

/**
 * 处理同步开关切换。
 */
async function handleToggleSync(): Promise<void> {
  try {
    const isPaused = await invoke<boolean>("toggle_sync");
    const btn = document.getElementById("btn-toggle-sync");
    const status = document.getElementById("sync-status");

    if (btn) {
      btn.textContent = isPaused ? "恢复同步" : "暂停同步";
    }
    if (status) {
      status.textContent = isPaused ? "已暂停" : "同步中";
      status.style.color = isPaused ? "#f59e0b" : "#22c55e";
    }
    appendLog(isPaused ? "同步已暂停" : "同步已恢复", "info");
  } catch (e) {
    appendLog(`切换同步失败: ${e}`, "error");
  }
}

// ============================================================
// UI 更新函数
// ============================================================

/**
 * 更新连接状态指示灯。
 *
 * @param state - 连接状态
 *   - "connected": 绿色实心圆点 + "已连接"
 *   - "disconnected": 红色圆点 + "已断开"
 *   - "connecting": 黄色闪烁圆点 + "连接中..."
 *   - "not_configured": 灰色空心圆点 + "未配置"
 */
export function updateStatusIndicator(state: ConnectionState): void {
  const dot = document.getElementById("status-indicator");
  const text = document.getElementById("status-text");

  if (!dot || !text) return;

  // 移除所有状态类
  dot.classList.remove("connected", "disconnected", "connecting", "not-configured");

  // 添加新状态类
  dot.classList.add(state);

  // 更新文本
  const labels: Record<ConnectionState, string> = {
    connected: "已连接",
    disconnected: "已断开",
    connecting: "连接中...",
    not_configured: "未配置",
  };
  text.textContent = labels[state];
  dot.title = labels[state];

  const connectBtn = document.getElementById("btn-connect") as HTMLButtonElement | null;

  // 更新连接按钮状态
  if (connectBtn) {
    if (state === "connected") {
      connectBtn.textContent = "重新连接";
      connectBtn.disabled = false;
    } else if (state === "connecting") {
      connectBtn.textContent = "连接中...";
      connectBtn.disabled = true;
    } else {
      connectBtn.textContent = "连接";
      connectBtn.disabled = false;
    }
  }
}

/**
 * 将配置数据填充到表单。
 *
 * @param config - 从 Rust 后端加载的应用配置
 */
export function updateConfigForm(config: AppConfig): void {
  setInputValue("input-server-ip", config.server_ip);
  setInputValue("input-server-port", String(config.server_port));
  setInputValue("input-auth-token", config.auth_token);
  setInputValue("input-enc-key", config.encryption_key_hex);
  setInputValue("input-device-name", config.device_name);

  const deviceEl = document.getElementById("device-id");
  if (deviceEl) {
    deviceEl.textContent = config.device_id
      ? `${config.device_id.substring(0, 8)}...`
      : "-";
    deviceEl.title = config.device_id;
  }
}

// ============================================================
// 日志面板
// ============================================================

/**
 * 向日志面板追加一条消息。
 */
export function appendLog(message: string, level: "info" | "error" | "warn"): void {
  const logEl = document.getElementById("log-output");
  if (!logEl) return;

  const time = new Date().toLocaleTimeString("zh-CN", { hour12: false });
  const line = document.createElement("div");
  line.className = `log-line log-${level}`;
  line.textContent = `[${time}] ${message}`;

  logEl.appendChild(line);
  logEl.scrollTop = logEl.scrollHeight;

  // 限制日志行数（最多 50 条）
  while (logEl.children.length > 50) {
    logEl.removeChild(logEl.firstChild!);
  }
}

// ============================================================
// 同步提示 Toast
// ============================================================

/**
 * 显示剪贴板同步提示。
 *
 * @param direction - "out" 表示已发送，"in" 表示已接收
 * @param preview - 文本预览（前 20 字符）
 * @param senderId - 发送方设备 ID（仅 "in" 方向）
 */
export function showSyncToast(direction: "in" | "out", preview: string, senderId?: string): void {
  const toast = document.getElementById("sync-toast");
  if (!toast) return;

  const icon = direction === "out" ? "↑" : "↓";
  const label = direction === "out" ? "已发送" : "已接收";
  const source = direction === "in" && senderId
    ? ` · 来自 ${senderId.substring(0, 8)}`
    : "";

  // 截断预览文本并转义
  const text = preview.replace(/\n/g, " ").substring(0, 30);

  toast.innerHTML = `<span class="toast-icon">${icon}</span> ${label}${source} · <span class="toast-preview">${text}</span>`;
  toast.classList.add("show");

  // 2 秒后自动隐藏
  clearTimeout((toast as any)._hideTimer);
  (toast as any)._hideTimer = setTimeout(() => {
    toast.classList.remove("show");
  }, 2000);
}

// ============================================================
// 辅助函数
// ============================================================

function getInputValue(id: string): string {
  const el = document.getElementById(id) as HTMLInputElement;
  return el?.value?.trim() || "";
}

function setInputValue(id: string, value: string): void {
  const el = document.getElementById(id) as HTMLInputElement;
  if (el) el.value = value;
}
