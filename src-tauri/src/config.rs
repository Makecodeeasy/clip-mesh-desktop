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

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 应用配置结构体（P2P 模式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 本设备唯一标识（首次运行时自动生成并持久化）
    pub device_id: String,

    /// 本设备名称（用户可自定义）
    pub device_name: String,

    /// AES-256 加密密钥（十六进制编码，两端需相同）
    pub encryption_key_hex: String,

    /// P2P WebSocket 监听端口（0 = 自动分配）
    #[serde(default)]
    pub p2p_port: u16,

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
            device_id: generate_device_id(),
            device_name: get_hostname(),
            encryption_key_hex: String::new(),
            p2p_port: 0,
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

        fs::create_dir_all(&config_dir).ok();
        config_dir.join("config.json")
    }

    /// 从磁盘加载配置。
    ///
    /// 若配置文件不存在或解析失败，返回默认配置。
    /// 兼容旧版配置文件（自动忽略 server_ip 等已废弃字段）。
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
                config.save().ok();
                config
            }
        }
    }

    /// 将配置保存到磁盘（原子写入：先写临时文件再 rename）。
    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialize error: {}", e))?;

        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, &json)
            .map_err(|e| format!("Write tmp error: {:?} — {}", tmp_path, e))?;
        fs::rename(&tmp_path, &path)
            .map_err(|e| format!("Rename error: {:?} — {}", path, e))?;

        log::debug!("[Config] Saved to {:?}", path);
        Ok(())
    }

    /// 检查配置是否完整（加密密钥已设置）。
    pub fn is_valid(&self) -> bool {
        !self.encryption_key_hex.is_empty()
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
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown Device".to_string())
}
