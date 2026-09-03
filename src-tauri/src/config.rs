//! # 配置管理模块
//!
//! 负责应用配置的加载、保存与运行时更新。
//!
//! ## 配置文件位置
//!
//! | 平台    | 路径                                            |
//! |---------|------------------------------------------------|
//! | macOS   | ~/Library/Application Support/com.clipmesh.desktop/config.json |
//! | Windows | %APPDATA%\com.clipmesh.desktop\config.json     |
//!
//! ## 配置结构
//!
//! ```json
//! {
//!     "server_ip": "192.168.1.100",
//!     "server_port": 8080,
//!     "auth_token": "v1.xxxx.yyyy",
//!     "device_id": "auto-generated-uuid",
//!     "device_name": "My MacBook",
//!     "encryption_key_hex": "a1b2c3...",
//!     "auto_start": true,
//!     "sync_enabled": true
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 应用配置结构体。
///
/// 所有字段均支持 serde 序列化/反序列化，
/// 可直接与 JSON 文件进行转换。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 服务端 IP 地址
    pub server_ip: String,

    /// 服务端端口号
    pub server_port: u16,

    /// Auth Token（HMAC-SHA256，由配对接口签发）
    pub auth_token: String,

    /// 本设备唯一标识（首次运行时自动生成并持久化）
    pub device_id: String,

    /// 本设备名称（用户可自定义）
    pub device_name: String,

    /// AES-256 加密密钥（十六进制编码）
    /// 由用户在配对后手动设置或通过密钥交换协议获取
    pub encryption_key_hex: String,

    /// 是否开机自启
    #[serde(default = "default_true")]
    pub auto_start: bool,

    /// 是否启用剪贴板同步
    #[serde(default = "default_true")]
    pub sync_enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server_ip: "127.0.0.1".to_string(),
            server_port: 8080,
            auth_token: String::new(),
            device_id: generate_device_id(),
            device_name: get_hostname(),
            encryption_key_hex: String::new(),
            auto_start: true,
            sync_enabled: true,
        }
    }
}

impl AppConfig {
    /// 获取配置文件路径。
    pub fn config_path() -> PathBuf {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("com.clipmesh.desktop");

        // 确保目录存在
        fs::create_dir_all(&config_dir).ok();

        config_dir.join("config.json")
    }

    /// 从磁盘加载配置。
    ///
    /// 若配置文件不存在或解析失败，返回默认配置。
    pub fn load() -> Self {
        let path = Self::config_path();
        match fs::read_to_string(&path) {
            Ok(content) => {
                match serde_json::from_str::<AppConfig>(&content) {
                    Ok(config) => {
                        log::info!("[Config] Loaded from {:?}", path);
                        config
                    }
                    Err(e) => {
                        log::warn!("[Config] Parse error: {}, using defaults", e);
                        AppConfig::default()
                    }
                }
            }
            Err(_) => {
                log::info!("[Config] No config file found, using defaults");
                let config = AppConfig::default();
                // 首次运行时保存默认配置
                config.save().ok();
                config
            }
        }
    }

    /// 将配置保存到磁盘。
    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialize error: {}", e))?;

        fs::write(&path, json)
            .map_err(|e| format!("Write error: {:?} — {}", path, e))?;

        log::debug!("[Config] Saved to {:?}", path);
        Ok(())
    }

    /// 构造 WebSocket 连接 URL。
    pub fn ws_url(&self) -> String {
        format!("ws://{}:{}/api/v1/ws", self.server_ip, self.server_port)
    }

    /// 检查配置是否完整（必要字段已填写）。
    pub fn is_valid(&self) -> bool {
        !self.server_ip.is_empty()
            && self.server_port > 0
            && !self.auth_token.is_empty()
            && !self.encryption_key_hex.is_empty()
    }
}

/// 生成设备唯一标识（16 字节随机数 → 32 字符十六进制）。
fn generate_device_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// 获取主机名作为默认设备名称。
fn get_hostname() -> String {
    // 尝试从环境变量获取主机名
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME")) // Windows
        .unwrap_or_else(|_| "Unknown Device".to_string())
}
