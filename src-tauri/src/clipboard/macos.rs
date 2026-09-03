//! # macOS 剪贴板实现 — NSPasteboard
//!
//! 通过 Objective-C Runtime（`objc` / `cocoa` crate）调用 macOS 原生剪贴板 API。
//!
//! ## 核心 API
//!
//! | Objective-C                        | Rust (objc crate)                        | 说明           |
//! |------------------------------------|------------------------------------------|---------------|
//! | `[NSPasteboard generalPasteboard]` | `msg_send![cls, generalPasteboard]`      | 获取系统剪贴板  |
//! | `[pb changeCount]`                 | `msg_send![pb, changeCount]`             | 变更计数       |
//! | `[pb stringForType:type]`          | `msg_send![pb, stringForType:type]`      | 读取文本       |
//! | `[pb clearContents]`               | `msg_send![pb, clearContents]`           | 清空剪贴板     |
//! | `[pb setString:forType:]`          | `msg_send![pb, setString:forType:]`      | 写入文本       |
//!
//! ## 注意事项
//!
//! - NSPasteboard 操作必须在主线程或拥有 AutoreleasePool 的线程中执行。
//! - 使用 `cocoa::base::nil` 检查返回值是否为空对象。
//! - `changeCount` 为 `NSInteger`（i64），每次剪贴板内容变更时递增。

use objc::{msg_send, sel, sel_impl};
use objc::runtime::{Class, Object};
use cocoa::base::nil;
use cocoa::foundation::NSString;

use super::{ClipboardBackend, SuppressGuard};

/// macOS 剪贴板后端实现。
pub struct MacOSClipboard;

impl MacOSClipboard {
    /// 创建新的 macOS 剪贴板后端实例。
    pub fn new() -> Self {
        // 验证 NSPasteboard 类可用
        assert!(
            Class::get("NSPasteboard").is_some(),
            "NSPasteboard class not found — must run on macOS"
        );
        Self
    }

    /// 获取系统通用剪贴板（General Pasteboard）的 Objective-C 对象指针。
    ///
    /// 等价于 Objective-C 调用：`[NSPasteboard generalPasteboard]`
    unsafe fn general_pasteboard() -> *mut Object {
        let cls = Class::get("NSPasteboard").expect("NSPasteboard class");
        msg_send![cls, generalPasteboard]
    }

    /// 获取 `NSPasteboardTypeString` 常量（UTI 类型标识符）。
    ///
    /// 在 macOS 10.14+ 中，推荐使用 `public.utf8-plain-text` UTI，
    /// 但 `NSPasteboardTypeString`（即 `NSStringPboardType`）仍兼容所有版本。
    unsafe fn pasteboard_type_string() -> *mut Object {
        let cls = Class::get("NSPasteboard").expect("NSPasteboard class");
        // NSPasteboardTypeString 是一个类属性
        msg_send![cls, propertyListForType: nil]
    }
}

impl ClipboardBackend for MacOSClipboard {
    /// 读取系统剪贴板的当前文本内容。
    ///
    /// 实现步骤：
    /// 1. 获取 generalPasteboard
    /// 2. 构造 NSPasteboardTypeString 类型标识
    /// 3. 调用 `stringForType:` 获取 NSString
    /// 4. 转换为 Rust String
    fn read_text(&self) -> Option<String> {
        unsafe {
            let pb = Self::general_pasteboard();
            if pb == nil {
                return None;
            }

            // 构造类型标识 "public.utf8-plain-text"
            let type_str = NSString::alloc(nil).init_str("public.utf8-plain-text");

            // [pb stringForType: type_str]
            let ns_string: *mut Object = msg_send![pb, stringForType: type_str];

            if ns_string == nil {
                return None;
            }

            // 将 NSString 转换为 Rust &str → String
            let utf8_ptr: *const std::os::raw::c_char = msg_send![ns_string, UTF8String];
            if utf8_ptr.is_null() {
                return None;
            }

            let c_str = std::ffi::CStr::from_ptr(utf8_ptr);
            match c_str.to_str() {
                Ok(s) => {
                    let result = s.to_string();
                    // 释放临时 NSString
                    let _: () = msg_send![type_str, release];
                    Some(result)
                }
                Err(_) => {
                    let _: () = msg_send![type_str, release];
                    None
                }
            }
        }
    }

    /// 将文本写入系统剪贴板。
    ///
    /// 实现步骤：
    /// 1. 激活防循环哨兵
    /// 2. `[pb clearContents]` 清空剪贴板
    /// 3. `[pb setString:forType:]` 写入新文本
    /// 4. 短暂延迟后解除哨兵（等待监听器轮询过本次变更）
    fn write_text(&self, text: &str, guard: &SuppressGuard) -> Result<(), String> {
        unsafe {
            // 激活防循环哨兵
            guard.arm();

            let pb = Self::general_pasteboard();
            if pb == nil {
                guard.disarm();
                return Err("Failed to get general pasteboard".to_string());
            }

            // 清空剪贴板
            let _: bool = msg_send![pb, clearContents];

            // 构造 NSString 与类型标识
            let ns_text = NSString::alloc(nil).init_str(text);
            let type_str = NSString::alloc(nil).init_str("public.utf8-plain-text");

            // [pb setString: ns_text forType: type_str]
            // 返回 BOOL 表示是否成功
            let success: bool = msg_send![pb, setString: ns_text forType: type_str];

            // 释放临时对象
            let _: () = msg_send![ns_text, release];
            let _: () = msg_send![type_str, release];

            if !success {
                guard.disarm();
                return Err("setString:forType: returned NO".to_string());
            }

            // 注意：哨兵不在此处解除。
            // 它将由监听器在下一次检测到变更时通过 check_and_clear() 原子地读取并解除。
            Ok(())
        }
    }

    /// 获取剪贴板变更计数。
    ///
    /// `NSPasteboard.changeCount` 是一个递增的 NSInteger，
    /// 每当剪贴板内容发生变更（包括其他应用程序的操作），该值加 1。
    fn change_count(&self) -> i64 {
        unsafe {
            let pb = Self::general_pasteboard();
            if pb == nil {
                return 0;
            }
            let count: i64 = msg_send![pb, changeCount];
            count
        }
    }
}

// 安全标记：MacOSClipboard 内部无可变状态，所有操作通过 msg_send 与系统 API 交互。
// NSPasteboard 本身是线程安全的（Apple 文档保证）。
unsafe impl Send for MacOSClipboard {}
unsafe impl Sync for MacOSClipboard {}
