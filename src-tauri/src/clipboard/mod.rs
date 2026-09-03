//! # 剪贴板监听与注入模块
//!
//! 本模块实现跨平台系统剪贴板的双向操作：
//! - **监听（Monitor）**：检测本地剪贴板变更，提取文本内容供加密发送。
//! - **注入（Inject）**：将远端解密后的文本静默写入本地剪贴板。
//!
//! ## 防循环机制
//!
//! 当本端收到远端数据并写入本地剪贴板时，会触发监听器的变更事件。
//! 为防止「写入 → 监听 → 再次发送」的无限循环，引入 **哨兵标记（Sentinel）** 机制：
//!
//! 1. 写入剪贴板前，设置 `suppress_guard = true`；
//! 2. 监听器检测到变更时，若 `suppress_guard == true`，跳过本次事件；
//! 3. 写入完成后，在下一个轮询周期重置标记。
//!
//! ## 平台实现
//!
//! | 平台  | 技术方案                          | 监听方式     |
//! |-------|----------------------------------|-------------|
//! | macOS | NSPasteboard (objc/cocoa crate)  | changeCount 轮询 |
//! | Windows | Win32 user32 API               | Clipboard Sequence Number 轮询 |

pub mod monitor;

// 平台条件编译：仅导入当前平台的实现
#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

// ============================================================
// 跨平台公共类型
// ============================================================

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 剪贴板变更事件，由平台监听器产生并发送到通道。
#[derive(Debug, Clone)]
pub struct ClipboardEvent {
    /// 文本内容（已提取的纯文本）
    pub text: String,
    /// 事件发生时的时间戳（Unix 毫秒）
    pub timestamp: i64,
}

/// 防循环哨兵标记（线程安全）。
///
/// 当本端执行「静默写入」操作时置位，监听器检测到变更后据此跳过本次事件，
/// 避免产生「写入→监听→发送→写入」的死循环。
#[derive(Clone)]
pub struct SuppressGuard {
    inner: Arc<AtomicBool>,
}

impl SuppressGuard {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 激活哨兵（即将写入剪贴板）
    pub fn arm(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    /// 解除哨兵
    pub fn disarm(&self) {
        self.inner.store(false, Ordering::SeqCst);
    }

    /// 检查是否处于抑制状态，并自动解除（原子操作）
    pub fn check_and_clear(&self) -> bool {
        self.inner.swap(false, Ordering::SeqCst)
    }
}

impl Default for SuppressGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
// 平台抽象 Trait
// ============================================================

/// 剪贴板操作的平台抽象接口。
///
/// 每个平台（macOS / Windows）需提供以下能力的实现：
/// - 读取当前剪贴板文本
/// - 写入文本到剪贴板
/// - 获取变更序号（用于轮询检测）
pub trait ClipboardBackend: Send + Sync {
    /// 读取系统剪贴板的当前文本内容。
    ///
    /// 返回 `Some(text)` 表示成功读取到文本，`None` 表示剪贴板为空或不含文本。
    fn read_text(&self) -> Option<String>;

    /// 将文本写入系统剪贴板。
    ///
    /// `silent` 参数控制是否激活防循环哨兵：
    /// - `true`：来自远端的静默写入，需激活哨兵
    /// - `false`：普通写入
    fn write_text(&self, text: &str, guard: &SuppressGuard) -> Result<(), String>;

    /// 获取剪贴板变更序号。
    ///
    /// - macOS: `NSPasteboard.changeCount`
    /// - Windows: `GetClipboardSequenceNumber()`
    ///
    /// 轮询器通过比较前后两次序号判断是否发生变更。
    fn change_count(&self) -> i64;
}

// 导出当前平台的默认后端实现
#[cfg(target_os = "macos")]
pub use macos::MacOSClipboard as PlatformClipboard;

#[cfg(target_os = "windows")]
pub use windows::WindowsClipboard as PlatformClipboard;
