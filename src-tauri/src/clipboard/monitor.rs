//! # 剪贴板变更监听器
//!
//! 基于轮询（Polling）的剪贴板变更检测器。
//!
//! ## 设计说明
//!
//! 不同于事件驱动模型，轮询方式在 macOS 和 Windows 上具有统一的行为语义：
//! - macOS 没有官方的剪贴板变更通知 API，只能轮询 `changeCount`；
//! - Windows 虽有 `AddClipboardFormatListener`，但需要窗口句柄（HWND），
//!   在无窗口后台进程中需要额外创建消息窗口。
//!
//! 因此采用统一的轮询方案，以 500ms 间隔检测变更序号变化。

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;

use super::{ClipboardBackend, ClipboardEvent, SuppressGuard};

/// 轮询间隔（毫秒）。
/// 500ms 在响应速度与 CPU 开销之间取得平衡。
const POLL_INTERVAL_MS: u64 = 500;

/// 剪贴板变更监听器。
///
/// 在后台异步任务中持续轮询系统剪贴板状态，
/// 当检测到变更时产生 `ClipboardEvent` 并通过 mpsc 通道发送给消费者。
pub struct ClipboardMonitor {
    /// 平台相关的剪贴板后端实现
    backend: Arc<dyn ClipboardBackend>,
    /// 防循环哨兵
    guard: SuppressGuard,
    /// 事件发送端
    event_tx: mpsc::Sender<ClipboardEvent>,
}

impl ClipboardMonitor {
    /// 创建新的剪贴板监听器。
    ///
    /// # 参数
    /// - `backend`: 平台剪贴板后端（macOS / Windows）
    /// - `guard`: 防循环哨兵（与注入端共享同一实例）
    /// - `event_tx`: 事件通道发送端（消费端通常为加密模块）
    pub fn new(
        backend: Arc<dyn ClipboardBackend>,
        guard: SuppressGuard,
        event_tx: mpsc::Sender<ClipboardEvent>,
    ) -> Self {
        Self {
            backend,
            guard,
            event_tx,
        }
    }

    /// 启动监听循环。此方法为异步阻塞调用，应在独立的 tokio 任务中运行。
    ///
    /// ## 工作流程
    ///
    /// ```text
    /// loop:
    ///   1. sleep(POLL_INTERVAL)
    ///   2. current_count = backend.change_count()
    ///   3. if current_count != last_count:
    ///        a. if guard.check_and_clear():  ← 哨兵命中，跳过
    ///             continue
    ///        b. text = backend.read_text()
    ///        c. if text is Some && not empty:
    ///             send ClipboardEvent { text, timestamp }
    ///   4. last_count = current_count
    /// ```
    pub async fn start(&self) {
        let mut interval = time::interval(Duration::from_millis(POLL_INTERVAL_MS));
        let mut last_count: i64 = self.backend.change_count();

        log::info!("[Monitor] Clipboard monitor started (poll interval: {}ms)", POLL_INTERVAL_MS);

        loop {
            interval.tick().await;

            let current_count = self.backend.change_count();

            if current_count != last_count {
                // 检查防循环哨兵：若处于抑制状态则跳过本次变更
                if self.guard.check_and_clear() {
                    log::debug!("[Monitor] Change suppressed by sentinel guard");
                    last_count = current_count;
                    continue;
                }

                // 读取剪贴板文本
                if let Some(text) = self.backend.read_text() {
                    if !text.is_empty() {
                        let event = ClipboardEvent {
                            text: text.clone(),
                            timestamp: chrono::Utc::now().timestamp_millis(),
                        };

                        // 发送到事件通道（若通道满则丢弃，避免阻塞）
                        match self.event_tx.try_send(event) {
                            Ok(_) => {
                                let preview: String = if text.chars().count() > 50 {
                                    let p: String = text.chars().take(50).collect();
                                    format!("{}...", p)
                                } else {
                                    text.clone()
                                };
                                log::debug!("[Monitor] Clipboard changed: {:?}", preview);
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                log::warn!("[Monitor] Event channel full, dropping event");
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                log::error!("[Monitor] Event channel closed, stopping monitor");
                                return;
                            }
                        }
                    }
                }

                last_count = current_count;
            }
        }
    }
}
