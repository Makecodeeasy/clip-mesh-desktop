/**
 * clip-mesh-desktop — P2P 模式 UI 组件与交互逻辑
 *
 * 提供极简的配置界面，包含：
 * - 连接码（一端生成，另一端输入，自动派生加密密钥）
 * - 设备名称
 * - 已连接设备列表
 * - 连接状态指示灯
 * - 同步开关
 */

import { invoke } from "@tauri-apps/api/tauri";

// ============================================================
// 类型定义
// ============================================================

export interface AppConfig {
  device_id: string;
  device_name: string;
  encryption_key_hex: string;
  pairing_code: string;
  p2p_port: number;
  auto_start: boolean;
  sync_enabled: boolean;
}

export type ConnectionState = "connected" | "disconnected" | "listening" | "not_configured";

// ============================================================
// UI 初始化
// ============================================================

export function initUI(): void {
  const app = document.getElementById("app");
  if (!app) return;

  app.innerHTML = `
    <div class="container">
      <header class="header">
        <h1>✂️ Clip Mesh</h1>
        <p class="subtitle">P2P 异构终端数据安全协同</p>
      </header>

      <!-- 连接状态 -->
      <section class="status-section">
        <div class="status-row">
          <span class="status-label">连接状态</span>
          <span id="status-indicator" class="status-dot not-configured"></span>
          <span id="status-text" class="status-text">未配置</span>
        </div>
        <div class="status-row">
          <span class="status-label">设备 ID</span>
          <span id="device-id" class="device-id">-</span>
        </div>
      </section>

      <!-- 配置 -->
      <section class="config-section">
        <h2>P2P 设置</h2>

        <div class="form-row">
          <label>连接码</label>
          <div class="pairing-row">
            <input
              type="text"
              id="input-pairing-code"
              placeholder="输入或生成"
              class="form-input pairing-input"
              maxlength="6"
              autocomplete="off"
              spellcheck="false"
            />
            <button id="btn-generate-code" class="btn btn-secondary btn-generate">生成</button>
          </div>
          <div class="pairing-hint">一端点击生成，另一端输入相同连接码</div>
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

      <!-- 已连接设备 -->
      <section class="peers-section">
        <h2>已连接设备 <span id="peer-count" class="peer-count">0</span></h2>
        <div id="peers-list" class="peers-list">
          <div class="peer-empty">等待发现其他设备...</div>
        </div>
      </section>

      <!-- 操作按钮 -->
      <section class="actions-section">
        <button id="btn-start" class="btn btn-primary">启动 P2P</button>
        <button id="btn-toggle-sync" class="btn btn-secondary" disabled>暂停同步</button>
      </section>

      <!-- 日志区域 -->
      <section class="log-section">
        <div id="log-output" class="log-output"></div>
      </section>

      <footer class="footer">
        <span>Clip Mesh v2.0.0 (P2P)</span>
        <span>·</span>
        <span id="sync-status">未同步</span>
      </footer>

      <div id="sync-toast" class="sync-toast"></div>
    </div>
  `;

  bindEvents();
}

// ============================================================
// 事件绑定
// ============================================================

function bindEvents(): void {
  const startBtn = document.getElementById("btn-start");
  startBtn?.addEventListener("click", handleStart);

  const syncBtn = document.getElementById("btn-toggle-sync");
  syncBtn?.addEventListener("click", handleToggleSync);

  const generateBtn = document.getElementById("btn-generate-code");
  generateBtn?.addEventListener("click", handleGenerateCode);

  // 连接码输入框：自动转大写
  const codeInput = document.getElementById("input-pairing-code") as HTMLInputElement | null;
  codeInput?.addEventListener("input", () => {
    codeInput.value = codeInput.value.toUpperCase().replace(/[^A-Z2-9]/g, "");
  });

  const inputs = document.querySelectorAll(".form-input");
  inputs.forEach((input) => {
    input.addEventListener("keydown", (e: Event) => {
      if ((e as KeyboardEvent).key === "Enter") {
        handleStart();
      }
    });
  });
}

async function handleStart(): Promise<void> {
  const startBtn = document.getElementById("btn-start") as HTMLButtonElement | null;
  if (startBtn) {
    startBtn.disabled = true;
    startBtn.textContent = "启动中...";
  }

  let currentConfig: AppConfig;
  try {
    currentConfig = await invoke<AppConfig>("get_config");
  } catch {
    currentConfig = { device_id: "", device_name: "", encryption_key_hex: "", pairing_code: "", p2p_port: 0, auto_start: true, sync_enabled: true };
  }

  const pairingCode = getInputValue("input-pairing-code") || currentConfig.pairing_code;

  if (!pairingCode) {
    appendLog("请先生成或输入连接码", "error");
    resetStartButton();
    return;
  }

  // 从连接码派生 AES-256 加密密钥
  const encryptionKeyHex = await deriveKey(pairingCode);

  const config: AppConfig = {
    device_id: currentConfig.device_id,
    device_name: getInputValue("input-device-name") || currentConfig.device_name,
    encryption_key_hex: encryptionKeyHex,
    pairing_code: pairingCode,
    p2p_port: 0,
    auto_start: true,
    sync_enabled: true,
  };

  try {
    await invoke("update_config", { newConfig: config });
    appendLog(`连接码: ${pairingCode}，配置已保存`, "info");

    await invoke("start_p2p");
    appendLog("P2P 服务已启动，正在发现设备...", "info");

    const syncBtn = document.getElementById("btn-toggle-sync");
    if (syncBtn) syncBtn.removeAttribute("disabled");

    if (startBtn) {
      startBtn.textContent = "运行中";
    }
  } catch (e) {
    appendLog(`启动失败: ${e}`, "error");
    resetStartButton();
  }
}

function resetStartButton(): void {
  const startBtn = document.getElementById("btn-start") as HTMLButtonElement | null;
  if (startBtn) {
    startBtn.disabled = false;
    startBtn.textContent = "启动 P2P";
  }
}

// ============================================================
// 连接码生成与密钥派生
// ============================================================

/** 连接码字符集（去掉易混淆字符 I/O/0/1） */
const CODE_CHARSET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/** 生成 6 位随机连接码并填入输入框 */
function handleGenerateCode(): void {
  const code = generatePairingCode();
  setInputValue("input-pairing-code", code);
  appendLog(`已生成连接码: ${code}，请在另一台设备输入`, "info");
}

function generatePairingCode(): string {
  const array = new Uint8Array(6);
  crypto.getRandomValues(array);
  let code = "";
  for (let i = 0; i < 6; i++) {
    code += CODE_CHARSET[array[i] % CODE_CHARSET.length];
  }
  return code;
}

/** 从连接码派生 AES-256 密钥（SHA-256 → hex） */
async function deriveKey(code: string): Promise<string> {
  const encoder = new TextEncoder();
  const data = encoder.encode(code);
  const hashBuffer = await crypto.subtle.digest("SHA-256", data);
  const hashArray = new Uint8Array(hashBuffer);
  return Array.from(hashArray)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

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

export function updateStatusIndicator(state: ConnectionState): void {
  const dot = document.getElementById("status-indicator");
  const text = document.getElementById("status-text");
  if (!dot || !text) return;

  dot.classList.remove("connected", "disconnected", "listening", "not-configured");
  dot.classList.add(state);

  const labels: Record<ConnectionState, string> = {
    connected: "已连接",
    listening: "监听中",
    disconnected: "已断开",
    not_configured: "未配置",
  };
  text.textContent = labels[state];
  dot.title = labels[state];
}

export function updateConfigForm(config: AppConfig): void {
  setInputValue("input-pairing-code", config.pairing_code);
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
// 设备列表管理
// ============================================================

interface PeerInfo {
  device_id: string;
  device_name: string;
}

const connectedPeers = new Map<string, PeerInfo>();

export function addPeer(deviceId: string, deviceName: string): void {
  connectedPeers.set(deviceId, { device_id: deviceId, device_name: deviceName });
  renderPeersList();
}

export function removePeer(deviceId: string): void {
  connectedPeers.delete(deviceId);
  renderPeersList();
}

function renderPeersList(): void {
  const listEl = document.getElementById("peers-list");
  const countEl = document.getElementById("peer-count");
  if (!listEl) return;

  if (countEl) countEl.textContent = String(connectedPeers.size);

  if (connectedPeers.size === 0) {
    listEl.innerHTML = '<div class="peer-empty">等待发现其他设备...</div>';
    return;
  }

  let html = "";
  for (const [, peer] of connectedPeers) {
    const shortId = peer.device_id.substring(0, 8);
    html += `
      <div class="peer-item">
        <span class="peer-dot"></span>
        <span class="peer-name">${peer.device_name}</span>
        <span class="peer-id">${shortId}</span>
      </div>
    `;
  }
  listEl.innerHTML = html;
}

// ============================================================
// Toast 提示
// ============================================================

export function showSyncToast(direction: "in" | "out", preview: string, senderId?: string): void {
  const toast = document.getElementById("sync-toast");
  if (!toast) return;

  const icon = direction === "out" ? "↑" : "↓";
  const label = direction === "out" ? "已发送" : "已接收";
  const source = direction === "in" && senderId
    ? ` · 来自 ${senderId.substring(0, 8)}`
    : "";

  const text = preview.replace(/\n/g, " ").substring(0, 30);
  // M7: 使用 textContent 防止 XSS（剪贴板内容可能包含 HTML）
  toast.textContent = "";
  const iconSpan = document.createElement("span");
  iconSpan.className = "toast-icon";
  iconSpan.textContent = icon;
  const previewSpan = document.createElement("span");
  previewSpan.className = "toast-preview";
  previewSpan.textContent = text;
  toast.append(iconSpan, ` ${label}${source} · `, previewSpan);
  toast.classList.add("show");

  clearTimeout((toast as any)._hideTimer);
  (toast as any)._hideTimer = setTimeout(() => {
    toast.classList.remove("show");
  }, 2000);
}

// ============================================================
// 日志面板
// ============================================================

export function appendLog(message: string, level: "info" | "error" | "warn"): void {
  const logEl = document.getElementById("log-output");
  if (!logEl) return;

  const time = new Date().toLocaleTimeString("zh-CN", { hour12: false });
  const line = document.createElement("div");
  line.className = `log-line log-${level}`;
  line.textContent = `[${time}] ${message}`;

  logEl.appendChild(line);
  logEl.scrollTop = logEl.scrollHeight;

  while (logEl.children.length > 50) {
    logEl.removeChild(logEl.firstChild!);
  }
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
