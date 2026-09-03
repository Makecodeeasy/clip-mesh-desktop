//! # Windows 剪贴板实现 — Win32 user32 API
//!
//! 通过 `windows` crate 调用 Windows 原生剪贴板 API。
//!
//! ## 核心 API
//!
//! | Win32 API                           | 说明                           |
//! |-------------------------------------|-------------------------------|
//! | `OpenClipboard(NULL)`               | 打开剪贴板（获取独占访问权）    |
//! | `GetClipboardSequenceNumber()`      | 获取变更序号（无需打开剪贴板）   |
//! | `GetClipboardData(CF_UNICODETEXT)`  | 获取 Unicode 文本数据句柄       |
//! | `GlobalLock(hMem)`                  | 锁定全局内存对象，获取数据指针   |
//! | `GlobalUnlock(hMem)`                | 解锁全局内存对象               |
//! | `EmptyClipboard()`                  | 清空剪贴板内容                 |
//! | `SetClipboardData(CF_UNICODETEXT)`  | 设置 Unicode 文本数据           |
//! | `CloseClipboard()`                  | 释放剪贴板独占访问权           |
//!
//! ## 注意事项
//!
//! - 剪贴板操作期间必须持有独占锁，其他进程无法同时访问。
//! - 操作完毕必须调用 `CloseClipboard()`，否则会导致系统级死锁。
//! - `CF_UNICODETEXT` 使用 UTF-16 LE 编码，需与 Rust String（UTF-8）互转。

#[cfg(target_os = "windows")]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData,
                GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
            SystemServices::CF_UNICODETEXT,
        },
    },
};

use super::{ClipboardBackend, SuppressGuard};

/// Windows 剪贴板后端实现。
pub struct WindowsClipboard;

impl WindowsClipboard {
    /// 创建新的 Windows 剪贴板后端实例。
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "windows")]
impl ClipboardBackend for WindowsClipboard {
    /// 读取系统剪贴板的当前 Unicode 文本内容。
    ///
    /// ## Win32 调用序列
    ///
    /// ```c
    /// OpenClipboard(NULL);
    /// HANDLE hData = GetClipboardData(CF_UNICODETEXT);
    /// wchar_t* pText = (wchar_t*)GlobalLock(hData);
    /// // 读取 pText ...
    /// GlobalUnlock(hData);
    /// CloseClipboard();
    /// ```
    fn read_text(&self) -> Option<String> {
        unsafe {
            // 打开剪贴板（NULL 表示无关联窗口）
            if OpenClipboard(None).is_err() {
                log::warn!("[WinClipboard] OpenClipboard failed");
                return None;
            }

            // 获取 Unicode 文本数据句柄
            let h_data = match GetClipboardData(CF_UNICODETEXT) {
                Ok(h) => h,
                Err(_) => {
                    // 剪贴板中不含文本类型数据
                    let _ = CloseClipboard();
                    return None;
                }
            };

            // 锁定全局内存，获取数据指针
            let ptr = GlobalLock(h_data.0 as HGLOBAL);
            if ptr.is_null() {
                let _ = CloseClipboard();
                return None;
            }

            // 读取 UTF-16 字符串直到 NULL 终止符
            let wide_ptr = ptr as *const u16;
            let mut len = 0;
            while *wide_ptr.add(len) != 0 {
                len += 1;
            }

            // 将 UTF-16 切片转换为 Rust String
            let utf16_slice = std::slice::from_raw_parts(wide_ptr, len);
            let result = String::from_utf16_lossy(utf16_slice);

            // 解锁并关闭
            let _ = GlobalUnlock(h_data.0 as HGLOBAL);
            let _ = CloseClipboard();

            Some(result)
        }
    }

    /// 将文本写入系统剪贴板。
    ///
    /// ## Win32 调用序列
    ///
    /// ```c
    /// OpenClipboard(NULL);
    /// EmptyClipboard();
    /// HGLOBAL hMem = GlobalAlloc(GMEM_MOVEABLE, byte_count);
    /// wchar_t* pBuf = (wchar_t*)GlobalLock(hMem);
    /// wcscpy(pBuf, text);          // 复制 UTF-16 数据
    /// GlobalUnlock(hMem);
    /// SetClipboardData(CF_UNICODETEXT, hMem);
    /// CloseClipboard();
    /// // 注意：调用 SetClipboardData 后，系统接管 hMem 的生命周期，
    /// // 不应再调用 GlobalFree。
    /// ```
    fn write_text(&self, text: &str, guard: &SuppressGuard) -> Result<(), String> {
        // 激活防循环哨兵
        guard.arm();

        unsafe {
            // 打开剪贴板
            if OpenClipboard(None).is_err() {
                guard.disarm();
                return Err("OpenClipboard failed".to_string());
            }

            // 清空现有内容
            let _ = EmptyClipboard();

            // 将 UTF-8 转为 UTF-16（含 NULL 终止符）
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let byte_count = utf16.len() * 2; // UTF-16 每单元 2 字节

            // 分配可移动的全局内存块
            let h_mem = match GlobalAlloc(GMEM_MOVEABLE, byte_count) {
                Ok(h) => h,
                Err(e) => {
                    let _ = CloseClipboard();
                    guard.disarm();
                    return Err(format!("GlobalAlloc failed: {}", e));
                }
            };

            // 锁定内存并写入数据
            let ptr = GlobalLock(h_mem);
            if ptr.is_null() {
                let _ = GlobalFree(h_mem);
                let _ = CloseClipboard();
                guard.disarm();
                return Err("GlobalLock failed".to_string());
            }

            // 复制 UTF-16 数据到全局内存
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr as *mut u16, utf16.len());
            let _ = GlobalUnlock(h_mem);

            // 将数据设置到剪贴板（系统接管内存生命周期）
            if SetClipboardData(CF_UNICODETEXT, HANDLE(h_mem.0 as _)).is_err() {
                // 设置失败时需手动释放内存
                let _ = GlobalFree(h_mem);
                let _ = CloseClipboard();
                guard.disarm();
                return Err("SetClipboardData failed".to_string());
            }

            let _ = CloseClipboard();
            // 哨兵由监听器在下次检测时解除
            Ok(())
        }
    }

    /// 获取剪贴板变更序号。
    ///
    /// `GetClipboardSequenceNumber()` 是一个全局递增计数器，
    /// 无需打开剪贴板即可调用，开销极低，适合高频轮询场景。
    fn change_count(&self) -> i64 {
        #[cfg(target_os = "windows")]
        unsafe {
            GetClipboardSequenceNumber() as i64
        }

        #[cfg(not(target_os = "windows"))]
        {
            0
        }
    }
}

// Windows 剪贴板 API 是线程安全的（由系统内核保证）
unsafe impl Send for WindowsClipboard {}
unsafe impl Sync for WindowsClipboard {}
